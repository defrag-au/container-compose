//! The command line.
//!
//! `config`, `plan`, `up` (foreground and detached), `down`, `ps` and `logs`
//! are implemented. The foreground `up` is the supervisor.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use clap::{Parser, Subcommand};
use serde::Serialize;

use container_compose_plan::apply::{self, DownOptions, UpOptions};
use container_compose_plan::{plan as build_plan, plan_with, Detail, Plan, PlanOptions, Refusal};
use container_compose_runtime::{
    argv, labels, observe, probe, Command as RuntimeCommand, ObservedContainer, Probe,
};
use container_compose_stack::duration::Span;
use container_compose_stack::env::{self, Guard};
use container_compose_stack::model::{PullPolicy, RestartPolicy};
use container_compose_stack::ports::Protocol;
use container_compose_stack::resolve::{
    MountSource, ResolvedHealthcheck, ResolvedService, ResolvedStack,
};
use container_compose_stack::{self as stack, Options};
use container_compose_supervisor::cache;
use container_compose_supervisor::clock;
use container_compose_supervisor::lock::{self, LockError};
use container_compose_supervisor::{
    run as supervise, Options as SupervisorOptions, SupervisorError,
};

use crate::error::Error;

#[derive(Debug, Parser)]
#[command(
    name = "container-compose",
    version,
    about = "A composition layer over Apple's container runtime"
)]
pub struct Cli {
    /// The stack file. There is no upward search.
    #[arg(short = 'f', long, default_value = "stack.toml", global = true)]
    pub file: PathBuf,

    /// Override the project name.
    #[arg(short = 'p', long, global = true)]
    pub project_name: Option<String>,

    /// The `container` binary to invoke.
    #[arg(long, default_value = "container", global = true)]
    pub runtime: String,

    /// Echo every `container` argv before running it.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Disable ANSI colour.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Skip the git check on the env file.
    #[arg(long, global = true)]
    pub no_env_check: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the resolved stack, without contacting the runtime.
    Config {
        /// Emit the machine-readable document.
        #[arg(long)]
        json: bool,
    },
    /// Print what `up` would do, without running anything.
    Plan {
        /// Emit the exact argv, one block per step.
        #[arg(long, conflicts_with = "json")]
        commands: bool,
        /// Emit the machine-readable document.
        #[arg(long)]
        json: bool,
    },
    /// Create and start the stack, in dependency order.
    Up {
        /// Start these services and their dependencies; default is all.
        services: Vec<String>,
        /// Create and start, then exit. Requires every service to be `restart = "no"`.
        #[arg(short = 'd', long)]
        detach: bool,
        /// Graceful stop window on the failure path.
        #[arg(long, default_value = "15s", value_parser = parse_span)]
        timeout: Span,
        /// On failure, leave services this invocation started running.
        #[arg(long)]
        keep_started: bool,
        /// Refuse a config mismatch instead of replacing the container.
        #[arg(long)]
        no_recreate: bool,
        /// With `-d`, wait for readiness before exiting.
        #[arg(long)]
        wait: bool,
        /// On Ctrl-C, leave everything running.
        #[arg(long)]
        no_stop_on_exit: bool,
        /// Print the plan instead of applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop and delete this project's containers.
    Down {
        /// Delete this project's volumes after the containers are gone.
        #[arg(long)]
        volumes: bool,
        /// Graceful stop window per container.
        #[arg(long, default_value = "15s", value_parser = parse_span)]
        timeout: Span,
        /// Print the teardown without running it.
        #[arg(long)]
        dry_run: bool,
    },
    /// List this project's containers.
    Ps {
        /// Emit the machine-readable document.
        #[arg(long)]
        json: bool,
        /// Run each healthcheck now instead of reading the supervisor's cache.
        #[arg(long)]
        probe: bool,
    },
    /// Run a command in a running service.
    Exec {
        service: String,
        /// Keep stdin open.
        #[arg(short = 'i', long)]
        interactive: bool,
        /// Allocate a TTY.
        #[arg(short = 't', long)]
        tty: bool,
        /// Disable stdin and the TTY.
        #[arg(short = 'T', long = "no-tty")]
        no_tty: bool,
        /// The command, after `--`.
        #[arg(last = true, required = true, num_args = 1..)]
        command: Vec<String>,
    },
    /// Run a one-off container with a service's configuration.
    Run {
        service: String,
        /// Keep the container when it exits.
        #[arg(long)]
        keep: bool,
        /// Keep stdin open.
        #[arg(short = 'i', long)]
        interactive: bool,
        /// Allocate a TTY.
        #[arg(short = 't', long)]
        tty: bool,
        /// Disable stdin and the TTY.
        #[arg(short = 'T', long = "no-tty")]
        no_tty: bool,
        /// A command that replaces the service's, after `--`.
        #[arg(last = true, num_args = 0..)]
        command: Vec<String>,
    },
    /// Build the images of services with a build table.
    Build {
        services: Vec<String>,
        /// Do not use the build cache.
        #[arg(long)]
        no_cache: bool,
        /// Print the argv without running it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Pull the services' images.
    Pull {
        services: Vec<String>,
        /// Print the argv without running it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print a service's logs.
    Logs {
        /// Services to show; default is all.
        services: Vec<String>,
        /// Follow the output.
        #[arg(long)]
        follow: bool,
        /// Lines from the end.
        #[arg(short = 'n', long, default_value = "200")]
        lines: u32,
    },
}

pub fn run(cli: Cli) -> Result<(), Error> {
    match &cli.command {
        Command::Config { json } => config(&cli, *json),
        Command::Plan { commands, json } => plan_command(&cli, *commands, *json),
        Command::Up {
            services,
            detach,
            timeout,
            keep_started,
            no_recreate,
            wait,
            no_stop_on_exit,
            dry_run,
        } => up_command(
            &cli,
            &UpFlags {
                services,
                detach: *detach,
                timeout: *timeout,
                keep_started: *keep_started,
                no_recreate: *no_recreate,
                wait: *wait,
                no_stop_on_exit: *no_stop_on_exit,
                dry_run: *dry_run,
            },
        ),
        Command::Down {
            volumes,
            timeout,
            dry_run,
        } => down_command(&cli, *volumes, *timeout, *dry_run),
        Command::Ps { json, probe } => ps_command(&cli, *json, *probe),
        Command::Logs {
            services,
            follow,
            lines,
        } => logs_command(&cli, services, *follow, *lines),
        Command::Exec {
            service,
            interactive,
            tty,
            no_tty,
            command,
        } => exec_command(&cli, service, *interactive, *tty, *no_tty, command),
        Command::Run {
            service,
            keep,
            interactive,
            tty,
            no_tty,
            command,
        } => run_command(&cli, service, *keep, *interactive, *tty, *no_tty, command),
        Command::Build {
            services,
            no_cache,
            dry_run,
        } => build_command(&cli, services, *no_cache, *dry_run),
        Command::Pull { services, dry_run } => pull_command(&cli, services, *dry_run),
    }
}

fn config(cli: &Cli, json: bool) -> Result<(), Error> {
    let stack = load_checked(cli, false)?;

    let document = ConfigDocument::new(&stack);
    let rendered = if json {
        serde_json::to_string_pretty(&document)
            .map_err(|error| Error::Internal(format!("cannot render the config: {error}")))?
    } else {
        toml::to_string_pretty(&document)
            .map_err(|error| Error::Internal(format!("cannot render the config: {error}")))?
    };

    print!("{rendered}");
    if !rendered.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn plan_command(cli: &Cli, commands: bool, json: bool) -> Result<(), Error> {
    let stack = load_checked(cli, true)?;
    print_plan(cli, &stack, commands, json)
}

fn print_plan(cli: &Cli, stack: &ResolvedStack, commands: bool, json: bool) -> Result<(), Error> {
    let observed = observe(&cli.runtime).map_err(|error| Error::Unavailable(error.to_string()))?;
    let plan = build_plan(stack, &observed);

    let rendered = if json {
        serde_json::to_string_pretty(&plan)
            .map_err(|error| Error::Internal(format!("cannot render the plan: {error}")))?
    } else if commands {
        render_commands(&plan)
    } else {
        render_table(&plan)
    };
    print!("{rendered}");
    if !rendered.ends_with('\n') {
        println!();
    }

    if plan.refusals.is_empty() {
        Ok(())
    } else {
        Err(Error::Refused(plan.refusals))
    }
}

struct UpFlags<'a> {
    services: &'a [String],
    detach: bool,
    timeout: Span,
    keep_started: bool,
    no_recreate: bool,
    wait: bool,
    no_stop_on_exit: bool,
    dry_run: bool,
}

fn up_command(cli: &Cli, flags: &UpFlags<'_>) -> Result<(), Error> {
    let stack = load_checked(cli, true)?;

    if flags.dry_run {
        return print_plan(cli, &stack, false, false);
    }

    // One supervisor per project. The lock is held for the whole command —
    // through the foreground loop — and a stale pid does not block.
    let _lock = lock::Lock::acquire(&stack.name).map_err(|error| match error {
        LockError::Held { .. } => Error::Refused(vec![Refusal {
            path: "up".to_owned(),
            message: error.to_string(),
        }]),
        LockError::Io(error) => Error::Internal(error.to_string()),
    })?;

    if flags.detach {
        // `-d` exits immediately, so no process is left to honour a policy.
        if let Some(service) = stack
            .services
            .values()
            .find(|service| service.restart != RestartPolicy::No)
        {
            return Err(Error::Refused(vec![Refusal {
                path: format!("[services.{}].restart", service.name),
                message: format!(
                    "`up -d` cannot honour `restart = \"{}\"`; run `up` in the foreground, \
                     or set `restart = \"no\"`",
                    service.restart.as_str()
                ),
            }]));
        }
    }

    let observed = observe(&cli.runtime).map_err(|error| Error::Unavailable(error.to_string()))?;
    let plan = plan_with(
        &stack,
        &observed,
        &PlanOptions {
            no_recreate: flags.no_recreate,
        },
    );
    if !plan.refusals.is_empty() {
        eprint!("{}", render_table(&plan));
        return Err(Error::Refused(plan.refusals));
    }

    let options = UpOptions {
        runtime: cli.runtime.clone(),
        services: flags.services.to_vec(),
        timeout: timeout_secs(flags.timeout),
        keep_started: flags.keep_started,
        // Foreground `up` waits for readiness before supervising; `-d` waits
        // only when asked.
        wait: !flags.detach || flags.wait,
        verbose: cli.verbose,
    };
    let outcome = apply::up(&stack, &plan, &options, &mut std::io::stderr())?;

    if flags.detach {
        return Ok(());
    }

    let supervisor_options = SupervisorOptions {
        runtime: cli.runtime.clone(),
        poll_interval: std::time::Duration::from_secs(1),
        stop_timeout: timeout_secs(flags.timeout),
        no_stop_on_exit: flags.no_stop_on_exit,
        verbose: cli.verbose,
    };
    supervise(
        &stack,
        &outcome.selected,
        &supervisor_options,
        &mut std::io::stderr(),
    )
    .map_err(|error| match error {
        SupervisorError::Killed => Error::Interrupted,
        other => Error::Runtime(other.to_string()),
    })
}

fn down_command(cli: &Cli, volumes: bool, timeout: Span, dry_run: bool) -> Result<(), Error> {
    let stack = stack::load(
        &cli.file,
        &Options {
            project_name: cli.project_name.clone(),
        },
    )?;

    // A supervisor would restart what `down` stopped, so it has to go first.
    if let Some(pid) = lock::owner(&stack.name) {
        return Err(Error::Refused(vec![Refusal {
            path: "down".to_owned(),
            message: format!(
                "a supervisor is running for this project (pid {pid}); stop it before `down`"
            ),
        }]));
    }

    let observed = observe(&cli.runtime).map_err(|error| Error::Unavailable(error.to_string()))?;

    let options = DownOptions {
        runtime: cli.runtime.clone(),
        timeout: timeout_secs(timeout),
        volumes,
        dry_run,
        verbose: cli.verbose,
    };
    apply::down(&stack, &observed, &options, &mut std::io::stderr())?;
    if !dry_run {
        // The supervisor is gone with the lock, so the cache is only clutter.
        cache::remove(&stack.name);
    }
    Ok(())
}

fn ps_command(cli: &Cli, json: bool, probe_now: bool) -> Result<(), Error> {
    // `ps` is the one command that works without the stack file: an interrupted
    // session must stay inspectable. A file that exists but does not load is
    // still an error.
    let stack = if cli.file.exists() {
        Some(stack::load(
            &cli.file,
            &Options {
                project_name: cli.project_name.clone(),
            },
        )?)
    } else {
        None
    };
    if probe_now && stack.is_none() {
        return Err(Error::Usage(
            "`ps --probe` needs the stack file for the healthchecks".to_owned(),
        ));
    }

    let observed = observe(&cli.runtime).map_err(|error| Error::Unavailable(error.to_string()))?;
    let project = stack
        .as_ref()
        .map(|stack| stack.name.clone())
        .unwrap_or_else(|| project_from_path(cli));

    // Only a live supervisor's cache is trusted, and only for the same stack
    // config. A missing file, a dead pid or a hash mismatch is `unknown`.
    let health_cache = cache::live(
        &project,
        stack.as_ref().map(|stack| stack.config_hash.as_str()),
    );

    let mut containers: Vec<&ObservedContainer> = observed
        .containers
        .iter()
        .filter(|container| container.project() == Some(project.as_str()))
        // A kept one-off `run` container is not part of the stack; show it only
        // while it is alive.
        .filter(|container| container.label(labels::RUN).is_none() || container.is_running())
        .collect();
    containers.sort_by_key(|container| {
        ps_rank(
            stack.as_ref().map(|stack| stack.order.as_slice()),
            container.service(),
            &container.id,
        )
    });

    let entries: Vec<PsEntry> = containers
        .into_iter()
        .map(|container| {
            let service = stack.as_ref().and_then(|stack| {
                container
                    .service()
                    .and_then(|name| stack.services.get(name))
            });
            let ports = service.map(render_ports).unwrap_or_default();
            let health = if probe_now && container.is_running() {
                match service.and_then(|service| service.healthcheck.as_ref()) {
                    Some(check) => probe_health(&cli.runtime, &container.id, check),
                    None => unknown_health(),
                }
            } else if container.is_running() {
                health_cache
                    .as_ref()
                    .and_then(|cache| {
                        container
                            .service()
                            .and_then(|name| cache.services.get(name))
                    })
                    .map_or_else(unknown_health, |cached| PsHealth {
                        state: cached.state.clone(),
                        checked_at: cached.checked_at.clone(),
                        source: "supervisor".to_owned(),
                    })
            } else {
                unknown_health()
            };
            PsEntry {
                name: container.id.clone(),
                service: container.service().map(str::to_owned),
                state: container.status.state.clone(),
                health,
                image: container.configuration.image.reference.clone(),
                ports,
            }
        })
        .collect();

    let rendered = if json {
        serde_json::to_string_pretty(&entries)
            .map_err(|error| Error::Internal(format!("cannot render ps: {error}")))?
    } else {
        render_ps(&entries)
    };
    print!("{rendered}");
    if !rendered.ends_with('\n') {
        println!();
    }
    Ok(())
}

struct TerminalFlags {
    interactive: bool,
    tty: bool,
}

/// `-T` disables both; otherwise a TTY is allocated when stdin is one, and
/// `-i`/`-t` can ask for either explicitly.
fn terminal_flags(interactive: bool, tty: bool, no_tty: bool) -> TerminalFlags {
    if no_tty {
        return TerminalFlags {
            interactive: false,
            tty: false,
        };
    }
    let stdin_tty = std::io::stdin().is_terminal();
    let tty = tty || stdin_tty;
    TerminalFlags {
        interactive: interactive || tty || stdin_tty,
        tty,
    }
}

fn run_attached(command: RuntimeCommand) -> Result<(), Error> {
    match command.status() {
        Ok(_) => Ok(()),
        Err(error) => match error.status {
            Some(code) => Err(Error::Child(code)),
            None => Err(Error::Runtime(error.to_string())),
        },
    }
}

fn exec_command(
    cli: &Cli,
    service_name: &str,
    interactive: bool,
    tty: bool,
    no_tty: bool,
    command: &[String],
) -> Result<(), Error> {
    let stack = stack::load(
        &cli.file,
        &Options {
            project_name: cli.project_name.clone(),
        },
    )?;
    let service = stack.services.get(service_name).ok_or_else(|| {
        Error::Usage(format!(
            "no service named `{service_name}`; the stack has {}",
            stack.order.join(", ")
        ))
    })?;
    let container = argv::container_name(&stack.name, &service.name);

    let observed = observe(&cli.runtime).map_err(|error| Error::Unavailable(error.to_string()))?;
    match observed.container(&container) {
        Some(found) if found.is_running() => {}
        Some(_) => {
            return Err(Error::Runtime(format!(
                "`{container}` is not running; `exec` does not start it"
            )))
        }
        None => {
            return Err(Error::Runtime(format!(
                "`{container}` does not exist; run `container-compose up -d`"
            )))
        }
    }

    let flags = terminal_flags(interactive, tty, no_tty);
    let exec_argv = argv::exec_attached(&container, command, flags.interactive, flags.tty);
    run_attached(RuntimeCommand::new(&cli.runtime).args(exec_argv))
}

fn run_command(
    cli: &Cli,
    service_name: &str,
    keep: bool,
    interactive: bool,
    tty: bool,
    no_tty: bool,
    command: &[String],
) -> Result<(), Error> {
    let stack = load_checked(cli, true)?;
    let service = stack.services.get(service_name).ok_or_else(|| {
        Error::Usage(format!(
            "no service named `{service_name}`; the stack has {}",
            stack.order.join(", ")
        ))
    })?;
    let run_name = argv::run_name(&stack.name, &service.name);

    let observed = observe(&cli.runtime).map_err(|error| Error::Unavailable(error.to_string()))?;
    if observed.container(&run_name).is_some() {
        return Err(Error::Refused(vec![Refusal {
            path: "run".to_owned(),
            message: format!(
                "`{run_name}` already exists; remove it with `container delete {run_name}`"
            ),
        }]));
    }

    // A named volume can only be held by one running container. The service's
    // own container is that holder whenever it is up.
    let service_container = argv::container_name(&stack.name, &service.name);
    let holds_volume = service
        .volumes
        .iter()
        .any(|mount| matches!(mount.source, MountSource::Volume { .. }));
    if holds_volume
        && observed
            .container(&service_container)
            .is_some_and(|container| container.is_running())
    {
        return Err(Error::Refused(vec![Refusal {
            path: "run".to_owned(),
            message: format!(
                "`{service_name}` mounts a named volume, which its running container holds; \
                 stop it first or use `exec` in the running container"
            ),
        }]));
    }

    let flags = terminal_flags(interactive, tty, no_tty);
    let command = if command.is_empty() {
        None
    } else {
        Some(command)
    };
    let run_argv = argv::run_container(
        &stack,
        service,
        command,
        flags.interactive,
        flags.tty,
        !keep,
    );
    run_attached(
        RuntimeCommand::new(&cli.runtime)
            .args(run_argv)
            .envs(service.env.clone()),
    )
}

fn selected_services(stack: &ResolvedStack, requested: &[String]) -> Result<Vec<String>, Error> {
    if requested.is_empty() {
        return Ok(stack.order.clone());
    }
    for name in requested {
        if !stack.services.contains_key(name) {
            return Err(Error::Usage(format!("no service named `{name}`")));
        }
    }
    Ok(stack
        .order
        .iter()
        .filter(|name| requested.contains(name))
        .cloned()
        .collect())
}

fn build_command(
    cli: &Cli,
    services: &[String],
    no_cache: bool,
    dry_run: bool,
) -> Result<(), Error> {
    let stack = stack::load(
        &cli.file,
        &Options {
            project_name: cli.project_name.clone(),
        },
    )?;
    let selected = selected_services(&stack, services)?;

    let mut built = 0;
    for name in selected {
        let service = &stack.services[&name];
        let Some(build) = &service.build else {
            println!("skip     {name}  no build table");
            continue;
        };
        let build_argv = argv::build_image(&service.image, build, no_cache);
        if dry_run {
            println!("container {}", build_argv.join(" "));
            continue;
        }
        println!("building {}", service.image);
        RuntimeCommand::new(&cli.runtime)
            .args(build_argv)
            .run()
            .map_err(|error| Error::Runtime(error.to_string()))?;
        built += 1;
    }
    if !dry_run {
        println!("built {built} image{}", if built == 1 { "" } else { "s" });
    }
    Ok(())
}

fn pull_command(cli: &Cli, services: &[String], dry_run: bool) -> Result<(), Error> {
    let stack = stack::load(
        &cli.file,
        &Options {
            project_name: cli.project_name.clone(),
        },
    )?;
    let selected = selected_services(&stack, services)?;

    for name in selected {
        let service = &stack.services[&name];
        if service.build.is_some() {
            println!("skip     {name}  built locally");
            continue;
        }
        if service.pull == PullPolicy::Never {
            println!("skip     {name}  pull = \"never\"");
            continue;
        }
        let pull_argv = argv::pull_image(&service.image);
        if dry_run {
            println!("container {}", pull_argv.join(" "));
            continue;
        }
        println!("pulling  {}", service.image);
        RuntimeCommand::new(&cli.runtime)
            .args(pull_argv)
            .run()
            .map_err(|error| Error::Runtime(error.to_string()))?;
    }
    Ok(())
}

fn probe_health(runtime: &str, container: &str, check: &ResolvedHealthcheck) -> PsHealth {
    let state = match probe::probe(runtime, container, &check.exec, check.timeout.as_duration()) {
        Ok(Probe::Healthy) => "healthy",
        Ok(Probe::Failed { .. }) => "unhealthy",
        Ok(Probe::Unknown { .. }) => "unknown",
        Ok(Probe::MissingBinary { binary, .. }) => {
            eprintln!("warning: {container}: probe binary `{binary}` does not exist");
            "unhealthy"
        }
        Err(error) => {
            eprintln!("warning: cannot probe {container}: {error}");
            "unknown"
        }
    };
    PsHealth {
        state: state.to_owned(),
        checked_at: Some(clock::format_rfc3339_seconds(clock::wall_ms() / 1_000)),
        source: "probe".to_owned(),
    }
}

fn unknown_health() -> PsHealth {
    PsHealth {
        state: "unknown".to_owned(),
        checked_at: None,
        source: "none".to_owned(),
    }
}

fn logs_command(cli: &Cli, services: &[String], follow: bool, lines: u32) -> Result<(), Error> {
    let stack = stack::load(
        &cli.file,
        &Options {
            project_name: cli.project_name.clone(),
        },
    )?;

    let selected: Vec<String> = if services.is_empty() {
        stack.order.clone()
    } else {
        for name in services {
            if !stack.services.contains_key(name) {
                return Err(Error::Usage(format!("no service named `{name}`")));
            }
        }
        services.to_vec()
    };

    let targets: Vec<(String, Vec<String>)> = selected
        .iter()
        .map(|service| {
            let container = argv::container_name(&stack.name, service);
            (service.clone(), argv::logs(&container, follow, Some(lines)))
        })
        .collect();

    // One service: pass the output through unmodified.
    if targets.len() == 1 {
        let (_, log_argv) = &targets[0];
        let mut print = |line: &str| println!("{line}");
        RuntimeCommand::new(&cli.runtime)
            .args(log_argv.iter().cloned())
            .stream_lines(&mut print)
            .map_err(|error| Error::Runtime(error.to_string()))?;
        return Ok(());
    }

    // More than one: every line carries its service name. Without `--follow`
    // the streams are finite, so sequential is honest.
    if !follow {
        for (service, log_argv) in &targets {
            let mut print = |line: &str| println!("{service} | {line}");
            RuntimeCommand::new(&cli.runtime)
                .args(log_argv.iter().cloned())
                .stream_lines(&mut print)
                .map_err(|error| Error::Runtime(error.to_string()))?;
        }
        return Ok(());
    }

    let (sender, receiver) = mpsc::channel();
    for (service, log_argv) in targets {
        let runtime = cli.runtime.clone();
        let sender = sender.clone();
        std::thread::spawn(move || {
            let lines = sender.clone();
            let label = service.clone();
            let mut send = move |line: &str| {
                let _ = lines.send(LogEvent::Line {
                    service: label.clone(),
                    text: line.to_owned(),
                });
            };
            if let Err(error) = RuntimeCommand::new(runtime)
                .args(log_argv.iter().cloned())
                .stream_lines(&mut send)
            {
                let _ = sender.send(LogEvent::Failed {
                    service,
                    message: error.to_string(),
                });
            }
        });
    }
    drop(sender);

    let mut failure = None;
    for event in receiver {
        match event {
            LogEvent::Line { service, text } => println!("{service} | {text}"),
            LogEvent::Failed { service, message } => {
                eprintln!("{service}: {message}");
                failure = Some(message);
            }
        }
    }
    match failure {
        Some(message) => Err(Error::Runtime(message)),
        None => Ok(()),
    }
}

enum LogEvent {
    Line { service: String, text: String },
    Failed { service: String, message: String },
}

fn project_from_path(cli: &Cli) -> String {
    if let Some(name) = &cli.project_name {
        return name.clone();
    }
    let dir = cli
        .file
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    dir.canonicalize()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "stack".to_owned())
}

fn parse_span(value: &str) -> Result<Span, String> {
    Span::parse(value)
}

/// `container stop --time` takes whole seconds; round up so a sub-second grace
/// period is never rounded to an immediate kill.
fn timeout_secs(span: Span) -> u64 {
    u64::try_from(span.millis().div_ceil(1_000)).unwrap_or(u64::MAX)
}

fn load_checked(cli: &Cli, refuse: bool) -> Result<ResolvedStack, Error> {
    let stack = stack::load(
        &cli.file,
        &Options {
            project_name: cli.project_name.clone(),
        },
    )?;

    if !cli.no_env_check {
        if let Some(path) = &stack.env_file {
            if refuse {
                if let Some(refusal) = env_refusal(path) {
                    return Err(Error::Refused(vec![refusal]));
                }
            } else if let Some(warning) = env_warning(path) {
                eprintln!("{warning}");
            }
        }
    }

    Ok(stack)
}

fn env_warning(path: &Path) -> Option<String> {
    match env::check_secret_file(path) {
        Guard::Tracked => Some(format!(
            "warning: `{}` is tracked by git; secrets in it will be committed. \
             Run `git rm --cached {}` and add it to `.gitignore`.",
            path.display(),
            path.display()
        )),
        Guard::NotIgnored => Some(format!(
            "warning: `{}` is not ignored by git; the next `git add .` will commit it. \
             Add it to `.gitignore`.",
            path.display()
        )),
        Guard::Ignored | Guard::NotChecked => None,
    }
}

fn env_refusal(path: &std::path::Path) -> Option<Refusal> {
    match env::check_secret_file(path) {
        Guard::Tracked => Some(Refusal {
            path: "[env].file".to_owned(),
            message: format!(
                "`{}` is tracked by git; secrets in it will be committed. \
                 Run `git rm --cached {}` and add it to `.gitignore`.",
                path.display(),
                path.display()
            ),
        }),
        Guard::NotIgnored => Some(Refusal {
            path: "[env].file".to_owned(),
            message: format!(
                "`{}` is not ignored by git; the next `git add .` will commit it. \
                 Add it to `.gitignore`.",
                path.display()
            ),
        }),
        Guard::Ignored | Guard::NotChecked => None,
    }
}

fn render_table(plan: &Plan) -> String {
    let mut out = format!(
        "PLAN  {}  config {}  ({})\n\n",
        plan.project, plan.config, plan.file
    );
    for step in &plan.steps {
        out.push_str(&format!(
            "  {:<9} {:<9} {:<17} {}\n",
            step.action.as_str(),
            step.resource.as_str(),
            step.name,
            render_detail(&step.detail)
        ));
    }
    out.push_str(&format!(
        "\n{} steps. Run `container-compose up` to apply.\n",
        plan.steps.len()
    ));
    out
}

fn render_commands(plan: &Plan) -> String {
    let mut out = String::new();
    for step in &plan.steps {
        out.push_str(&format!(
            "# {} {} {}\n",
            step.action.as_str(),
            step.resource.as_str(),
            step.name
        ));
        if let Some(argv) = &step.argv {
            out.push_str(&render_argv(argv));
            out.push('\n');
        }
    }
    out
}

/// Group a flag with its value, and keep the leading subcommand words on the
/// first line, the way the command is written by hand.
fn render_argv(argv: &[String]) -> String {
    let mut lines: Vec<String> = vec!["container".to_owned()];
    let mut index = 0;
    let mut seen_flag = false;
    let mut trailing = false;

    while index < argv.len() {
        let argument = &argv[index];
        if argument.starts_with('-') {
            seen_flag = true;
            trailing = false;
            let mut line = argument.clone();
            if takes_value(argument) {
                if let Some(value) = argv.get(index + 1) {
                    line.push(' ');
                    line.push_str(value);
                    index += 2;
                } else {
                    index += 1;
                }
            } else {
                index += 1;
            }
            lines.push(line);
        } else if seen_flag {
            if trailing {
                let line = lines.last_mut().expect("a trailing line");
                line.push(' ');
                line.push_str(argument);
            } else {
                lines.push(argument.clone());
                trailing = true;
            }
            index += 1;
        } else {
            let line = lines.last_mut().expect("the program line");
            line.push(' ');
            line.push_str(argument);
            index += 1;
        }
    }

    lines.join(" \\\n    ")
}

/// The flags the plan emits that take a value. This is presentation only: the
/// argv itself is the builder's, unchanged.
fn takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--name"
            | "--label"
            | "--network"
            | "--env"
            | "--volume"
            | "--publish"
            | "--cpus"
            | "--memory"
            | "--ulimit"
            | "--shm-size"
            | "--tmpfs"
            | "--cap-add"
            | "--cap-drop"
            | "--user"
            | "--workdir"
            | "--entrypoint"
            | "--time"
            | "--opt"
            | "--subnet"
            | "--target"
            | "--build-arg"
            | "-t"
            | "-f"
            | "-n"
            | "--format"
    )
}

fn render_detail(detail: &Detail) -> String {
    let mut parts = Vec::new();
    match detail {
        Detail::Network { mode, subnet } => {
            parts.push(format!("mode {mode}"));
            if let Some(subnet) = subnet {
                parts.push(format!("subnet {subnet}"));
            }
        }
        Detail::Volume { size, journal } => {
            if let Some(size) = size {
                parts.push(format!("size {size}"));
            }
            if let Some(journal) = journal {
                parts.push(format!("journal {journal}"));
            }
        }
        Detail::Container { image, state, .. } => {
            parts.push(image.clone());
            if let Some(state) = state {
                parts.push(format!("state {state}"));
            }
        }
        Detail::Image { .. } => {}
        Detail::Note { message } => parts.push(message.clone()),
    }
    parts.join(", ")
}

/// One row of `ps`. `health` is an object rather than a formatted string,
/// because the table is one rendering of it, not the only consumer.
#[derive(Debug, Serialize)]
struct PsEntry {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    service: Option<String>,
    state: String,
    health: PsHealth,
    image: String,
    ports: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PsHealth {
    state: String,
    checked_at: Option<String>,
    source: String,
}

fn render_ps(entries: &[PsEntry]) -> String {
    let headers = ["NAME", "SERVICE", "STATE", "HEALTH", "IMAGE", "PORTS"];
    let rows: Vec<[String; 6]> = entries
        .iter()
        .map(|entry| {
            [
                entry.name.clone(),
                entry.service.clone().unwrap_or_default(),
                entry.state.clone(),
                if entry.state == "running" {
                    entry.health.state.to_owned()
                } else {
                    "—".to_owned()
                },
                entry.image.clone(),
                entry.ports.join(", "),
            ]
        })
        .collect();

    let mut widths = headers.map(str::len);
    for row in &rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    push_ps_row(&mut out, &headers.map(str::to_owned), &widths);
    for row in &rows {
        push_ps_row(&mut out, row, &widths);
    }
    out
}

fn push_ps_row(out: &mut String, cells: &[String; 6], widths: &[usize; 6]) {
    for (index, cell) in cells.iter().enumerate() {
        if index + 1 == cells.len() {
            out.push_str(cell);
        } else {
            out.push_str(cell);
            let padding = widths[index].saturating_sub(cell.chars().count()) + 2;
            out.push_str(&" ".repeat(padding));
        }
    }
    out.push('\n');
}

fn render_ports(service: &ResolvedService) -> Vec<String> {
    service
        .ports
        .iter()
        .map(|port| {
            let mut rendered = String::new();
            if let Some((address, host_port)) = port.host_binding() {
                rendered.push_str(&format!("{address}:{host_port}->"));
            }
            rendered.push_str(&port.container_port.to_string());
            if port.protocol == Protocol::Udp {
                rendered.push_str("/udp");
            }
            rendered
        })
        .collect()
}

/// Known services in start order, then anything else by name.
fn ps_rank(order: Option<&[String]>, service: Option<&str>, name: &str) -> (usize, String) {
    let rank = match (order, service) {
        (Some(order), Some(service)) => order
            .iter()
            .position(|known| known == service)
            .unwrap_or(order.len()),
        _ => usize::MAX,
    };
    (rank, name.to_owned())
}

#[derive(Debug, Serialize)]
struct ConfigDocument {
    version: u32,
    name: String,
    file: String,
    config_hash: String,
    order: Vec<String>,
    network: NetworkDoc,
    volumes: BTreeMap<String, VolumeDoc>,
    services: BTreeMap<String, ServiceDoc>,
}

#[derive(Debug, Serialize)]
struct NetworkDoc {
    mode: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    subnet: Option<String>,
}

#[derive(Debug, Serialize)]
struct VolumeDoc {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal: Option<JournalDoc>,
}

#[derive(Debug, Serialize)]
struct JournalDoc {
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
}

#[derive(Debug, Serialize)]
struct ServiceDoc {
    image: String,
    config_hash: String,
    pull: String,
    restart: String,
    init: bool,
    read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workdir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entrypoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpus: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shm_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cap_add: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cap_drop: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ulimits: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tmpfs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    labels: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<DependencyDoc>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    volumes: Vec<MountDoc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<BuildDoc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    healthcheck: Option<HealthcheckDoc>,
}

#[derive(Debug, Serialize)]
struct DependencyDoc {
    service: String,
    condition: String,
}

#[derive(Debug, Serialize)]
struct MountDoc {
    kind: String,
    source: String,
    target: String,
    read_only: bool,
}

#[derive(Debug, Serialize)]
struct BuildDoc {
    context: String,
    containerfile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    args: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct HealthcheckDoc {
    exec: Vec<String>,
    interval: String,
    timeout: String,
    start_period: String,
    retries: u32,
}

impl ConfigDocument {
    fn new(stack: &ResolvedStack) -> Self {
        Self {
            version: stack.version,
            name: stack.name.clone(),
            file: stack.file.display().to_string(),
            config_hash: stack.config_hash.clone(),
            order: stack.order.clone(),
            network: NetworkDoc {
                mode: stack.network.mode.as_str().to_owned(),
                name: stack.network.name.clone(),
                subnet: stack.network.subnet.clone(),
            },
            volumes: stack
                .volumes
                .iter()
                .map(|(declared, volume)| {
                    (
                        declared.clone(),
                        VolumeDoc {
                            name: volume.name.clone(),
                            size: volume.size.map(|size| size.to_string()),
                            journal: volume.journal.as_ref().map(|journal| JournalDoc {
                                mode: journal.mode.clone(),
                                size: journal.size.map(|size| size.to_string()),
                            }),
                        },
                    )
                })
                .collect(),
            services: stack
                .services
                .iter()
                .map(|(name, service)| (name.clone(), ServiceDoc::new(service)))
                .collect(),
        }
    }
}

impl ServiceDoc {
    fn new(service: &ResolvedService) -> Self {
        Self {
            image: service.image.clone(),
            config_hash: service.config_hash.clone(),
            pull: service.pull.as_str().to_owned(),
            restart: service.restart.as_str().to_owned(),
            init: service.init,
            read_only: service.read_only,
            user: service.user.clone(),
            workdir: service.workdir.clone(),
            entrypoint: service.entrypoint.clone(),
            cpus: service.cpus,
            memory: service.memory.map(|size| size.to_string()),
            shm_size: service.shm_size.map(|size| size.to_string()),
            command: service.command.clone(),
            cap_add: service.cap_add.clone(),
            cap_drop: service.cap_drop.clone(),
            ulimits: service.ulimits.clone(),
            tmpfs: service.tmpfs.clone(),
            ports: service
                .ports
                .iter()
                .map(|port| port.source.clone())
                .collect(),
            aliases: service.aliases.clone(),
            // The config document is not a secret store: every value is
            // replaced, and the hash above is what callers compare.
            env: service
                .env
                .keys()
                .map(|key| (key.clone(), "<redacted>".to_owned()))
                .collect(),
            labels: service.labels.clone(),
            depends_on: service
                .depends_on
                .iter()
                .map(|dependency| DependencyDoc {
                    service: dependency.service.clone(),
                    condition: dependency.condition.as_str().to_owned(),
                })
                .collect(),
            volumes: service
                .volumes
                .iter()
                .map(|mount| {
                    let (kind, source) = match &mount.source {
                        MountSource::Volume { name, .. } => ("volume", name.clone()),
                        MountSource::Bind(path) => ("bind", path.display().to_string()),
                    };
                    MountDoc {
                        kind: kind.to_owned(),
                        source,
                        target: mount.target.clone(),
                        read_only: mount.read_only,
                    }
                })
                .collect(),
            build: service.build.as_ref().map(|build| BuildDoc {
                context: build.context.display().to_string(),
                containerfile: build.containerfile.display().to_string(),
                target: build.target.clone(),
                args: build.args.clone(),
            }),
            healthcheck: service
                .healthcheck
                .as_ref()
                .map(|healthcheck| HealthcheckDoc {
                    exec: healthcheck.exec.clone(),
                    interval: healthcheck.interval.to_string(),
                    timeout: healthcheck.timeout.to_string(),
                    start_period: healthcheck.start_period.to_string(),
                    retries: healthcheck.retries,
                }),
        }
    }
}
