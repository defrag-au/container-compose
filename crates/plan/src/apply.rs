//! Executing a plan against the runtime.
//!
//! The plan decides; this module runs the argv the plan carries, in order. It
//! makes only the decisions the plan cannot: which services a subset `up` should
//! start, and the cleanup the model requires when a step fails — stop only what
//! this invocation started, never what was already running.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write;
use std::time::{Duration, Instant};

use container_compose_runtime::{
    argv, labels, observe, probe, Command, Observed, ObservedContainer, ObservedVolume, Probe,
    RuntimeError,
};
use container_compose_stack::model::{Condition, NetworkMode};
use container_compose_stack::resolve::{ResolvedService, ResolvedStack};

use crate::{Action, Detail, Plan, Resource, Step};

/// How long `up` waits for a started container to become `running`. The probe
/// took two to three seconds; thirty is generous without hiding a hung runtime.
const START_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct UpOptions {
    pub runtime: String,
    /// Empty means every service. A named service brings its dependencies.
    pub services: Vec<String>,
    /// Grace period for the stops a failed run issues, in seconds.
    pub timeout: u64,
    /// On failure, leave services this invocation started running.
    pub keep_started: bool,
    /// Wait for each started service with a healthcheck to become healthy.
    pub wait: bool,
    /// Echo each `container` argv before running it.
    pub verbose: bool,
}

#[derive(Debug, Clone)]
pub struct DownOptions {
    pub runtime: String,
    /// Graceful stop window per container, in seconds.
    pub timeout: u64,
    /// Delete this project's volumes after the containers are gone.
    pub volumes: bool,
    pub dry_run: bool,
    pub verbose: bool,
}

#[derive(Debug)]
pub enum ApplyError {
    /// A service named in `up` is not in the stack. Nothing was executed.
    Usage(String),
    /// A runtime command failed. The context names the step it belonged to.
    Runtime {
        context: String,
        error: RuntimeError,
    },
    /// `start` returned but the container never became `running`.
    StartTimeout {
        container: String,
        seconds: u64,
    },
    /// The container runs but its healthcheck did not pass in the budget.
    Readiness {
        service: String,
        message: String,
    },
    Output(std::io::Error),
}

impl ApplyError {
    pub fn exit_code(&self) -> u8 {
        match self {
            ApplyError::Usage(_) => 2,
            ApplyError::Readiness { .. } => 4,
            ApplyError::Runtime { .. }
            | ApplyError::StartTimeout { .. }
            | ApplyError::Output(_) => 1,
        }
    }
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApplyError::Usage(message) => f.write_str(message),
            ApplyError::Runtime { context, error } => write!(f, "{context}: {error}"),
            ApplyError::StartTimeout { container, seconds } => write!(
                f,
                "container `{container}` did not reach `running` within {seconds}s; \
                 run `container-compose logs {container}`"
            ),
            ApplyError::Readiness { service, message } => {
                write!(f, "service `{service}` did not become healthy: {message}")
            }
            ApplyError::Output(error) => write!(f, "cannot write progress: {error}"),
        }
    }
}

impl std::error::Error for ApplyError {}

impl From<std::io::Error> for ApplyError {
    fn from(error: std::io::Error) -> Self {
        ApplyError::Output(error)
    }
}

/// What a successful `up` selected, so a foreground supervisor knows what to
/// watch. A named service brings its dependencies.
#[derive(Debug, Clone)]
pub struct UpOutcome {
    pub selected: BTreeSet<String>,
}

/// Run the plan. Progress goes to `out`; findings stay on stdout.
///
/// The caller refuses a plan with refusals before calling this: nothing here
/// re-checks them, because a refusal must be decided before the first mutation.
pub fn up(
    stack: &ResolvedStack,
    plan: &Plan,
    options: &UpOptions,
    out: &mut dyn Write,
) -> Result<UpOutcome, ApplyError> {
    debug_assert!(
        plan.refusals.is_empty(),
        "the caller must refuse a plan with refusals before applying it"
    );
    let selected = selected(stack, &options.services)?;
    let mut started = Vec::new();

    if let Err(error) = run_steps(stack, plan, options, &selected, &mut started, out) {
        if !options.keep_started
            && !started.is_empty()
            && !matches!(error, ApplyError::Readiness { .. })
        {
            writeln!(
                out,
                "stopping {} service(s) this run started",
                started.len()
            )?;
            for name in started.iter().rev() {
                let stop = argv::stop_container(name, options.timeout);
                // Best effort: the original failure is the one to report, but a
                // stop that also failed is worth saying out loud.
                if let Err(error) = execute(
                    &options.runtime,
                    &stop,
                    BTreeMap::new(),
                    options.verbose,
                    out,
                ) {
                    writeln!(out, "warning: could not stop {name}: {error}")?;
                }
            }
        }
        return Err(error);
    }

    writeln!(
        out,
        "stack {} is up ({} service{})",
        stack.name,
        selected.len(),
        if selected.len() == 1 { "" } else { "s" }
    )?;
    Ok(UpOutcome { selected })
}

fn run_steps(
    stack: &ResolvedStack,
    plan: &Plan,
    options: &UpOptions,
    selected: &BTreeSet<String>,
    started: &mut Vec<String>,
    out: &mut dyn Write,
) -> Result<(), ApplyError> {
    for step in &plan.steps {
        match step.action {
            Action::Keep => report(out, "keep", step)?,
            Action::Warn => {
                if let Detail::Note { message } = &step.detail {
                    writeln!(out, "warning: {message}")?;
                }
            }
            Action::Wait => {}
            Action::Start => {
                let service = service_for(stack, &step.name);
                let Some(service) = service else {
                    return Err(ApplyError::Usage(format!(
                        "the plan starts `{}`, which is not a service in this stack",
                        step.name
                    )));
                };
                if !selected.contains(&service.name) {
                    report(out, "skip", step)?;
                    continue;
                }

                let run_argv = step.argv.as_ref().expect("a start step carries the argv");

                // `service_healthy` gates the start, not the create: the
                // dependency was started earlier in the order.
                for dependency in &service.depends_on {
                    if dependency.condition == Condition::ServiceHealthy {
                        if let Some(resolved) = stack.services.get(&dependency.service) {
                            wait_for_healthy(stack, resolved, &options.runtime, out)?;
                        }
                    }
                }

                execute(
                    &options.runtime,
                    run_argv,
                    BTreeMap::new(),
                    options.verbose,
                    out,
                )
                .map_err(|error| start_failure(step, service, error))?;
                wait_until_running(&options.runtime, &step.name)?;
                started.push(step.name.clone());
                report(out, "running", step)?;

                if options.wait {
                    wait_for_healthy(stack, service, &options.runtime, out)?;
                }
            }
            _ => {
                let run_argv = step
                    .argv
                    .as_ref()
                    .expect("a mutating step carries the argv");
                let env = if step.action == Action::Create && step.resource == Resource::Container {
                    service_for(stack, &step.name)
                        .map(|service| service.env.clone())
                        .unwrap_or_default()
                } else {
                    BTreeMap::new()
                };
                run(
                    out,
                    &options.runtime,
                    run_argv,
                    env,
                    options.verbose,
                    format!(
                        "{} {} {}",
                        step.action.as_str(),
                        step.resource.as_str(),
                        step.name
                    ),
                )?;
                report(out, step.action.as_str(), step)?;
            }
        }
    }
    Ok(())
}

/// Teardown: stop, delete, and — only with `--volumes` — delete the project's
/// volumes. Resources are found by the project label, so an orphan from a
/// service no longer in the file is removed too, and nothing else is touched.
pub fn down(
    stack: &ResolvedStack,
    observed: &Observed,
    options: &DownOptions,
    out: &mut dyn Write,
) -> Result<(), ApplyError> {
    let mut containers: Vec<&ObservedContainer> = observed
        .containers
        .iter()
        .filter(|container| container.project() == Some(stack.name.as_str()))
        // A one-off `run` container is not part of the stack.
        .filter(|container| container.label(labels::RUN).is_none())
        .collect();
    containers.sort_by_key(|container| teardown_rank(stack, container.service()));

    let volumes: Vec<&ObservedVolume> = if options.volumes {
        observed
            .volumes
            .iter()
            .filter(|volume| volume.project() == Some(stack.name.as_str()))
            .collect()
    } else {
        Vec::new()
    };

    let network = if stack.network.mode == NetworkMode::Isolated {
        observed
            .network(&stack.network.name)
            .filter(|network| network.project() == Some(stack.name.as_str()))
    } else {
        None
    };

    if containers.is_empty() && volumes.is_empty() && network.is_none() {
        writeln!(out, "stack {} has nothing to remove", stack.name)?;
        return Ok(());
    }

    for container in containers {
        let mut force = false;
        if container.is_running() {
            let stop = argv::stop_container(&container.id, options.timeout);
            if options.dry_run {
                writeln!(out, "stop     container {}", container.id)?;
            } else if let Err(error) = execute(
                &options.runtime,
                &stop,
                BTreeMap::new(),
                options.verbose,
                out,
            ) {
                writeln!(
                    out,
                    "warning: {} did not stop cleanly ({error}); forcing the delete",
                    container.id
                )?;
                force = true;
            } else {
                writeln!(out, "stopped  container {}", container.id)?;
            }
        }

        let delete = argv::delete_container(&container.id, force);
        if options.dry_run {
            writeln!(out, "delete   container {}", container.id)?;
        } else {
            run(
                out,
                &options.runtime,
                &delete,
                BTreeMap::new(),
                options.verbose,
                format!("deleting container {}", container.id),
            )?;
            writeln!(out, "deleted  container {}", container.id)?;
        }
    }

    for volume in volumes {
        let delete = argv::delete_volume(&volume.id);
        if options.dry_run {
            writeln!(out, "delete   volume {}", volume.id)?;
        } else {
            run(
                out,
                &options.runtime,
                &delete,
                BTreeMap::new(),
                options.verbose,
                format!("deleting volume {}", volume.id),
            )?;
            writeln!(out, "deleted  volume {}", volume.id)?;
        }
    }

    if let Some(network) = network {
        let delete = argv::delete_network(&network.id);
        if options.dry_run {
            writeln!(out, "delete   network {}", network.id)?;
        } else {
            run(
                out,
                &options.runtime,
                &delete,
                BTreeMap::new(),
                options.verbose,
                format!("deleting network {}", network.id),
            )?;
            writeln!(out, "deleted  network {}", network.id)?;
        }
    }

    Ok(())
}

/// Reverse dependency order for known services, then everything unknown, so a
/// dependent stops before what it depends on.
fn teardown_rank(stack: &ResolvedStack, service: Option<&str>) -> usize {
    service
        .and_then(|service| stack.order.iter().position(|name| name == service))
        .map(|index| stack.order.len() - index)
        .unwrap_or(stack.order.len() + 1)
}

fn selected(stack: &ResolvedStack, requested: &[String]) -> Result<BTreeSet<String>, ApplyError> {
    if requested.is_empty() {
        return Ok(stack.order.iter().cloned().collect());
    }

    for name in requested {
        if !stack.services.contains_key(name) {
            return Err(ApplyError::Usage(format!(
                "no service named `{name}`; the stack has {}",
                stack
                    .order
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    let mut selected = BTreeSet::new();
    for name in requested {
        add_with_dependencies(stack, name, &mut selected);
    }
    Ok(selected)
}

fn add_with_dependencies(stack: &ResolvedStack, name: &str, selected: &mut BTreeSet<String>) {
    if !selected.insert(name.to_owned()) {
        return;
    }
    let Some(service) = stack.services.get(name) else {
        return;
    };
    for dependency in &service.depends_on {
        add_with_dependencies(stack, &dependency.service, selected);
    }
}

fn service_for<'a>(stack: &'a ResolvedStack, container: &str) -> Option<&'a ResolvedService> {
    stack
        .services
        .values()
        .find(|service| argv::container_name(&stack.name, &service.name) == container)
}

fn run(
    out: &mut dyn Write,
    runtime: &str,
    run_argv: &[String],
    env: BTreeMap<String, String>,
    verbose: bool,
    context: String,
) -> Result<(), ApplyError> {
    execute(runtime, run_argv, env, verbose, out)
        .map_err(|error| ApplyError::Runtime { context, error })
}

fn execute(
    runtime: &str,
    run_argv: &[String],
    env: BTreeMap<String, String>,
    verbose: bool,
    out: &mut dyn Write,
) -> Result<(), RuntimeError> {
    if verbose {
        let _ = writeln!(out, "$ {runtime} {}", run_argv.join(" "));
    }
    Command::new(runtime)
        .args(run_argv.iter().cloned())
        .envs(env)
        .run()
        .map(|_| ())
}

fn start_failure(step: &Step, service: &ResolvedService, error: RuntimeError) -> ApplyError {
    let mut context = format!("starting {}", step.name);
    if error.stderr.contains("Address already in use") && !service.ports.is_empty() {
        let ports = service
            .ports
            .iter()
            .map(|port| port.source.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        context.push_str(&format!(" (publishes {ports})"));
    }
    ApplyError::Runtime { context, error }
}

/// Wait for a service's healthcheck to pass. The budget is the model's:
/// `start_period + retries × interval`. A missing probe binary can never pass,
/// so it fails at once with a message naming it.
fn wait_for_healthy(
    stack: &ResolvedStack,
    service: &ResolvedService,
    runtime: &str,
    out: &mut dyn Write,
) -> Result<(), ApplyError> {
    let Some(check) = &service.healthcheck else {
        return Ok(());
    };
    let container = argv::container_name(&stack.name, &service.name);
    let budget = check.start_period.as_duration() + check.interval.as_duration() * check.retries;
    let deadline = Instant::now() + budget;
    writeln!(out, "waiting  {container}  for the healthcheck")?;

    loop {
        match probe::probe(
            runtime,
            &container,
            &check.exec,
            check.timeout.as_duration(),
        ) {
            Ok(Probe::Healthy) => {
                writeln!(out, "healthy  {container}")?;
                return Ok(());
            }
            Ok(Probe::Failed { stderr }) => {
                if !stderr.is_empty() {
                    writeln!(out, "probe    {container}  {stderr}")?;
                }
            }
            Ok(Probe::Unknown { reason }) => {
                return Err(ApplyError::Runtime {
                    context: format!("waiting for {container}"),
                    error: RuntimeError::message(reason),
                });
            }
            Ok(Probe::MissingBinary { binary, .. }) => {
                return Err(ApplyError::Readiness {
                    service: container,
                    message: format!("the probe binary `{binary}` does not exist in the container"),
                });
            }
            Err(error) => {
                return Err(ApplyError::Runtime {
                    context: format!("probing {container}"),
                    error,
                });
            }
        }

        if Instant::now() >= deadline {
            return Err(ApplyError::Readiness {
                service: container,
                message: format!("not healthy within {}ms", budget.as_millis()),
            });
        }
        std::thread::sleep(
            check
                .interval
                .as_duration()
                .min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// `start` is asynchronous: poll until the container is `running`. A
/// `depends_on: service_started` edge is satisfied by this, not by `start`
/// returning.
fn wait_until_running(runtime: &str, container: &str) -> Result<(), ApplyError> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let containers = list_containers(runtime)?;
        if containers
            .iter()
            .any(|found| found.id == container && found.is_running())
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ApplyError::StartTimeout {
                container: container.to_owned(),
                seconds: START_TIMEOUT.as_secs(),
            });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn list_containers(runtime: &str) -> Result<Vec<ObservedContainer>, ApplyError> {
    observe::containers(runtime).map_err(|error| ApplyError::Runtime {
        context: "observing the runtime".to_owned(),
        error,
    })
}

fn report(out: &mut dyn Write, verb: &str, step: &Step) -> Result<(), ApplyError> {
    writeln!(out, "{verb:<8} {:<9} {}", step.resource.as_str(), step.name)?;
    Ok(())
}
