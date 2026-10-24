//! The loop: observe, tick, act, until interrupted.
//!
//! Every decision is in [`crate::tick`]; this module only executes it. It
//! installs the signal handlers, runs the probes, writes the health cache and
//! performs the shutdown sequence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use signal_hook::consts::{SIGINT, SIGTERM};

use container_compose_runtime::{argv, observe, probe, Command, ObservedContainer, Probe};
use container_compose_stack::resolve::{ResolvedHealthcheck, ResolvedStack};

use crate::cache::{self, CachedHealth, HealthCache};
use crate::clock;
use crate::tick::{Action, ProbeOutcome, ServiceSpec, Supervisor, Update, View};

#[derive(Debug, Clone)]
pub struct Options {
    pub runtime: String,
    /// How often the runtime is polled.
    pub poll_interval: Duration,
    /// Graceful stop window during shutdown, in seconds.
    pub stop_timeout: u64,
    /// The first interrupt leaves everything running.
    pub no_stop_on_exit: bool,
    pub verbose: bool,
}

#[derive(Debug)]
pub enum SupervisorError {
    Signals(std::io::Error),
    Output(std::io::Error),
    /// The second interrupt skipped the graceful stop.
    Killed,
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SupervisorError::Signals(error) => write!(f, "cannot install signal handlers: {error}"),
            SupervisorError::Output(error) => write!(f, "cannot write progress: {error}"),
            SupervisorError::Killed => f.write_str("interrupted twice; containers were killed"),
        }
    }
}

impl std::error::Error for SupervisorError {}

impl From<std::io::Error> for SupervisorError {
    fn from(error: std::io::Error) -> Self {
        SupervisorError::Output(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnInterrupt {
    Leave,
    Stop,
    Kill,
}

/// The one decision a signal carries, kept pure so a test can assert it
/// without sending the test process a signal.
pub fn interrupt_action(interrupts: usize, no_stop_on_exit: bool) -> OnInterrupt {
    if interrupts >= 2 {
        OnInterrupt::Kill
    } else if no_stop_on_exit {
        OnInterrupt::Leave
    } else {
        OnInterrupt::Stop
    }
}

/// Supervise `services` until an interrupt. `Ok(())` means the containers were
/// stopped (or deliberately left running); `Killed` is the second interrupt.
pub fn run(
    stack: &ResolvedStack,
    services: &BTreeSet<String>,
    options: &Options,
    out: &mut dyn Write,
) -> Result<(), SupervisorError> {
    let interrupts = install_signals()?;
    let mut supervisor = Supervisor::new(
        services
            .iter()
            .filter_map(|name| stack.services.get(name))
            .map(ServiceSpec::from_service)
            .collect(),
    );
    let start = Instant::now();

    writeln!(
        out,
        "supervising {} service{}; press Ctrl-C to stop",
        services.len(),
        if services.len() == 1 { "" } else { "s" }
    )?;
    publish(stack, &supervisor, out);

    loop {
        if interrupted(&interrupts) {
            break;
        }

        let now_ms = elapsed(start);
        let containers = match observe::containers(&options.runtime) {
            Ok(containers) => containers,
            Err(error) => {
                let _ = writeln!(out, "warning: cannot observe the runtime: {error}");
                sleep(options.poll_interval, &interrupts);
                continue;
            }
        };

        let views = views_for(stack, services, &containers);
        let update: Update = supervisor.tick(now_ms, clock::wall_ms(), &views);
        let mut cache_dirty = !update.changed.is_empty();
        for (service, health) in &update.changed {
            let _ = writeln!(out, "health   {service}  {}", health.as_str());
        }

        for action in update.actions {
            if interrupted(&interrupts) {
                break;
            }
            match action {
                Action::Schedule {
                    service,
                    attempt,
                    delay_ms,
                } => {
                    let _ = writeln!(
                        out,
                        "restart  {service}  attempt {attempt} in {}",
                        render_delay(delay_ms)
                    );
                }
                Action::GiveUp { service, attempts } => {
                    let _ = writeln!(
                        out,
                        "give up  {service}  after {attempts} restart{}",
                        if attempts == 1 { "" } else { "s" }
                    );
                }
                Action::Restart { service, attempt } => {
                    let name = argv::container_name(&stack.name, &service);
                    let _ = writeln!(out, "restart  {name}  attempt {attempt}");
                    if let Err(error) = Command::new(&options.runtime)
                        .args(argv::start_container(&name))
                        .run()
                    {
                        let _ = writeln!(out, "warning: cannot restart {name}: {error}");
                    }
                }
                Action::Probe {
                    service,
                    timeout_ms,
                } => {
                    let Some(resolved) = stack.services.get(&service) else {
                        continue;
                    };
                    let Some(check) = &resolved.healthcheck else {
                        continue;
                    };
                    let name = argv::container_name(&stack.name, &service);
                    let outcome = run_probe(&options.runtime, &name, check, timeout_ms, out);
                    let now = elapsed(start);
                    if let Some(health) =
                        supervisor.record_probe(now, clock::wall_ms(), &service, outcome)
                    {
                        let _ = writeln!(out, "health   {service}  {}", health.as_str());
                        cache_dirty = true;
                    }
                }
            }
        }

        if cache_dirty {
            publish(stack, &supervisor, out);
        }
        sleep(options.poll_interval, &interrupts);
    }

    shutdown(stack, services, &interrupts, options, out)
}

fn install_signals() -> Result<Arc<AtomicUsize>, SupervisorError> {
    let interrupts = Arc::new(AtomicUsize::new(0));
    for signal in [SIGINT, SIGTERM] {
        let counter = interrupts.clone();
        // SAFETY: the handler only increments an atomic, which is
        // async-signal-safe. The handler is process-lifetime by design.
        unsafe {
            signal_hook::low_level::register(signal, move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
        }
        .map_err(SupervisorError::Signals)?;
    }
    Ok(interrupts)
}

fn run_probe(
    runtime: &str,
    container: &str,
    check: &ResolvedHealthcheck,
    timeout_ms: u128,
    out: &mut dyn Write,
) -> ProbeOutcome {
    let timeout = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(u64::MAX));
    match probe::probe(runtime, container, &check.exec, timeout) {
        Ok(Probe::Healthy) => ProbeOutcome::Healthy,
        Ok(Probe::Failed { .. }) => ProbeOutcome::Failed,
        Ok(Probe::Unknown { .. }) => ProbeOutcome::Unknown,
        Ok(Probe::MissingBinary { binary, .. }) => {
            let _ = writeln!(
                out,
                "warning: {container}: probe binary `{binary}` does not exist; \
                 the healthcheck can never pass"
            );
            ProbeOutcome::Failed
        }
        Err(error) => {
            let _ = writeln!(out, "warning: cannot probe {container}: {error}");
            ProbeOutcome::Unknown
        }
    }
}

fn views_for(
    stack: &ResolvedStack,
    services: &BTreeSet<String>,
    containers: &[ObservedContainer],
) -> BTreeMap<String, View> {
    services
        .iter()
        .map(|service| {
            let name = argv::container_name(&stack.name, service);
            let view = containers
                .iter()
                .find(|container| container.id == name)
                .map(|container| View {
                    running: container.is_running(),
                    started_at_ms: container
                        .status
                        .started_date
                        .as_deref()
                        .and_then(clock::parse_rfc3339_seconds)
                        .map(|seconds| seconds.saturating_mul(1_000)),
                })
                .unwrap_or_default();
            (service.clone(), view)
        })
        .collect()
}

fn publish(stack: &ResolvedStack, supervisor: &Supervisor, out: &mut dyn Write) {
    let services = supervisor
        .health_snapshot()
        .into_iter()
        .map(|(name, health)| {
            let checked_at = supervisor
                .checked_at(&name)
                .map(|millis| clock::format_rfc3339_seconds(millis / 1_000));
            (
                name,
                CachedHealth {
                    state: health.as_str().to_owned(),
                    checked_at,
                },
            )
        })
        .collect();
    let cache = HealthCache {
        pid: std::process::id(),
        project: stack.name.clone(),
        config: stack.config_hash.clone(),
        updated_at: clock::format_rfc3339_seconds(clock::wall_ms() / 1_000),
        services,
    };
    if let Err(error) = cache::write(&cache) {
        let _ = writeln!(out, "warning: cannot write the health cache: {error}");
    }
}

fn shutdown(
    stack: &ResolvedStack,
    services: &BTreeSet<String>,
    interrupts: &Arc<AtomicUsize>,
    options: &Options,
    out: &mut dyn Write,
) -> Result<(), SupervisorError> {
    let order = stop_order(stack, services);
    match interrupt_action(interrupts.load(Ordering::SeqCst), options.no_stop_on_exit) {
        OnInterrupt::Leave => {
            writeln!(out, "leaving {} container(s) running", order.len())?;
            Ok(())
        }
        OnInterrupt::Kill => {
            kill_all(&order, options, out);
            Err(SupervisorError::Killed)
        }
        OnInterrupt::Stop => {
            for name in &order {
                if interrupts.load(Ordering::SeqCst) >= 2 {
                    kill_all(&order, options, out);
                    return Err(SupervisorError::Killed);
                }
                writeln!(out, "stopping {name}")?;
                if let Err(error) = Command::new(&options.runtime)
                    .args(argv::stop_container(name, options.stop_timeout))
                    .run()
                {
                    let _ = writeln!(out, "warning: cannot stop {name}: {error}");
                }
            }
            Ok(())
        }
    }
}

fn kill_all(order: &[String], options: &Options, out: &mut dyn Write) {
    for name in order {
        let _ = writeln!(out, "killing {name}");
        if let Err(error) = Command::new(&options.runtime)
            .args(argv::kill_container(name))
            .run()
        {
            let _ = writeln!(out, "warning: cannot kill {name}: {error}");
        }
    }
}

/// Reverse dependency order: a dependent stops before what it depends on.
fn stop_order(stack: &ResolvedStack, services: &BTreeSet<String>) -> Vec<String> {
    stack
        .order
        .iter()
        .rev()
        .filter(|name| services.contains(*name))
        .map(|name| argv::container_name(&stack.name, name))
        .collect()
}

fn sleep(duration: Duration, interrupts: &AtomicUsize) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if interrupts.load(Ordering::SeqCst) > 0 {
            return;
        }
        std::thread::sleep(
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn interrupted(interrupts: &AtomicUsize) -> bool {
    interrupts.load(Ordering::SeqCst) > 0
}

fn elapsed(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn render_delay(delay_ms: u128) -> String {
    if delay_ms >= 1_000 {
        format!("{}s", delay_ms / 1_000)
    } else {
        format!("{delay_ms}ms")
    }
}

#[cfg(test)]
mod tests {
    use super::{interrupt_action, render_delay, stop_order, OnInterrupt};
    use container_compose_stack::model::NetworkMode;
    use container_compose_stack::resolve::{ResolvedNetwork, ResolvedStack, ResolvedVolume};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    fn stack() -> ResolvedStack {
        ResolvedStack {
            version: 1,
            name: "demo".to_owned(),
            file: PathBuf::from("stack.toml"),
            dir: PathBuf::from("."),
            network: ResolvedNetwork {
                mode: NetworkMode::Default,
                name: "default".to_owned(),
                subnet: None,
            },
            volumes: BTreeMap::<String, ResolvedVolume>::new(),
            services: BTreeMap::new(),
            order: vec!["one".to_owned(), "two".to_owned(), "three".to_owned()],
            config_hash: "abc".to_owned(),
            env_file: None,
        }
    }

    #[test]
    fn shutdown_stops_in_reverse_dependency_order() {
        let services = BTreeSet::from(["one".to_owned(), "two".to_owned(), "three".to_owned()]);
        assert_eq!(
            stop_order(&stack(), &services),
            ["demo-three", "demo-two", "demo-one"]
        );
    }

    #[test]
    fn a_second_interrupt_kills_and_one_interrupt_stops() {
        assert_eq!(interrupt_action(1, false), OnInterrupt::Stop);
        assert_eq!(interrupt_action(1, true), OnInterrupt::Leave);
        assert_eq!(interrupt_action(2, false), OnInterrupt::Kill);
        assert_eq!(interrupt_action(2, true), OnInterrupt::Kill);
    }

    #[test]
    fn delays_are_rendered_in_seconds() {
        assert_eq!(render_delay(1_000), "1s");
        assert_eq!(render_delay(30_000), "30s");
        assert_eq!(render_delay(250), "250ms");
    }
}
