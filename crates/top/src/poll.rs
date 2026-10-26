//! The poller: a worker thread that owns every runtime call.
//!
//! `container list` takes a few hundred milliseconds and `container logs`
//! longer on a busy service, so they run here and the UI thread only draws.
//! The UI sends [`Request`]s; the worker sends [`Update`]s back. Dropping the
//! request sender ends the worker.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use container_compose_runtime::{argv, observe, probe, Command, ObservedContainer, Probe};
use container_compose_supervisor::cache::{self, HealthCache};
use container_compose_supervisor::clock;
use container_compose_supervisor::history::{self, Sample};

/// Where to look, and how often.
#[derive(Debug, Clone)]
pub struct Source {
    pub runtime: String,
    pub project: String,
    pub config_hash: Option<String>,
    /// Each declared healthcheck's command and timeout, for an on-demand probe.
    pub healthchecks: BTreeMap<String, (Vec<String>, Duration)>,
    pub interval: Duration,
    /// Log lines fetched for the selected service.
    pub log_lines: u32,
    /// The supervisor's resource history for this project, if there is a home
    /// directory to find it in.
    pub history: Option<PathBuf>,
}

/// The history changes every 30 seconds, so re-reading it every poll is waste.
const HISTORY_EVERY: Duration = Duration::from_secs(10);
/// Enough of the file's end for the trend window: an hour of 30-second samples
/// is 120 lines, a few hundred bytes each for a handful of services.
const HISTORY_TAIL_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The service whose logs to fetch; `None` fetches none. Polls at once.
    Select(Option<String>),
    /// Run the service's healthcheck now, as `ps --probe` does.
    Probe(String),
    Refresh,
}

#[derive(Debug)]
pub enum Update {
    Snapshot(Snapshot),
    Probed { service: String, outcome: String },
}

#[derive(Debug)]
pub struct Snapshot {
    pub at_secs: u64,
    /// The runtime's container list, or why it could not be read.
    pub containers: Result<Vec<ObservedContainer>, String>,
    /// The health cache, only when [`cache::live`] trusts it.
    pub cache: Option<HealthCache>,
    pub logs: Option<Logs>,
    /// The recent history, when this poll re-read it; `None` keeps the last.
    pub history: Option<Vec<Sample>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logs {
    pub service: String,
    pub lines: Vec<String>,
    pub error: Option<String>,
}

pub fn spawn(source: Source) -> (Sender<Request>, Receiver<Update>) {
    let (requests, inbox) = mpsc::channel();
    let (outbox, updates) = mpsc::channel();
    thread::spawn(move || work(&source, &inbox, &outbox));
    (requests, updates)
}

fn work(source: &Source, inbox: &Receiver<Request>, outbox: &Sender<Update>) {
    let mut selected: Option<String> = None;
    let mut due = Instant::now();
    let mut history_read: Option<Instant> = None;
    loop {
        match inbox.recv_timeout(due.saturating_duration_since(Instant::now())) {
            Ok(Request::Select(service)) => {
                selected = service;
                due = Instant::now();
                continue;
            }
            Ok(Request::Refresh) => {
                due = Instant::now();
                continue;
            }
            Ok(Request::Probe(service)) => {
                let outcome = probe_now(source, &service);
                if outbox.send(Update::Probed { service, outcome }).is_err() {
                    return;
                }
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        let mut snapshot = capture(source, selected.as_deref());
        if history_read.is_none_or(|at| at.elapsed() >= HISTORY_EVERY) {
            snapshot.history = Some(
                source
                    .history
                    .as_deref()
                    .map(|path| history::read_tail_at(path, HISTORY_TAIL_BYTES))
                    .unwrap_or_default(),
            );
            history_read = Some(Instant::now());
        }
        if outbox.send(Update::Snapshot(snapshot)).is_err() {
            return;
        }
        due = Instant::now() + source.interval;
    }
}

fn capture(source: &Source, selected: Option<&str>) -> Snapshot {
    // `containers`, not `observe`: it is one subprocess rather than five, and it
    // does not gate on `system status`, which answers `unregistered` to a
    // launchd session other than the one that started the runtime (an SSH
    // login, typically) while `list` works from anywhere.
    let containers = observe::containers(&source.runtime).map_err(|error| error.to_string());
    let cache = cache::live(&source.project, source.config_hash.as_deref());

    let logs = selected.and_then(|service| {
        let name = argv::container_name(&source.project, service);
        let exists = containers
            .as_ref()
            .is_ok_and(|list| list.iter().any(|container| container.id == name));
        exists.then(|| fetch_logs(source, service, &name))
    });

    Snapshot {
        at_secs: clock::wall_ms() / 1_000,
        containers,
        cache,
        logs,
        history: None,
    }
}

fn fetch_logs(source: &Source, service: &str, container: &str) -> Logs {
    match Command::new(&source.runtime)
        .args(argv::logs(container, false, Some(source.log_lines)))
        .run()
    {
        Ok(output) => Logs {
            service: service.to_owned(),
            lines: output.stdout.lines().map(str::to_owned).collect(),
            error: None,
        },
        Err(error) => Logs {
            service: service.to_owned(),
            lines: Vec::new(),
            error: Some(error.to_string()),
        },
    }
}

fn probe_now(source: &Source, service: &str) -> String {
    let Some((exec, timeout)) = source.healthchecks.get(service) else {
        return "declares no healthcheck".to_owned();
    };
    let container = argv::container_name(&source.project, service);
    match probe(&source.runtime, &container, exec, *timeout) {
        Ok(Probe::Healthy) => "healthy".to_owned(),
        Ok(Probe::Failed { stderr }) => format!("unhealthy: {}", first_line(&stderr)),
        Ok(Probe::Unknown { reason }) => format!("unknown: {}", first_line(&reason)),
        Ok(Probe::MissingBinary { binary, .. }) => {
            format!("unhealthy: `{binary}` does not exist in the container")
        }
        Err(error) => format!("cannot probe: {}", first_line(&error.to_string())),
    }
}

fn first_line(text: &str) -> &str {
    text.trim().lines().next().unwrap_or("")
}
