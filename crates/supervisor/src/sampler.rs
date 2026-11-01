//! The sampler: a thread that records the stack's resource usage while the
//! supervisor runs.
//!
//! It is a thread rather than a step of the supervisor's loop because
//! `container stats --no-stream` blocks for two seconds, which would delay
//! every restart and probe behind it. It owns its state and shares only the
//! interrupt counter; anything worth saying goes back to the loop, which owns
//! the output, as a message.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use container_compose_runtime::{observe, ObservedContainer, ObservedStats};

use crate::clock;
use crate::history::{self, Reading, Sample};

/// Pruning rewrites the file, so it runs at start and then hourly, not per sample.
const PRUNE_EVERY: Duration = Duration::from_secs(3_600);

pub(crate) struct Sampler {
    pub runtime: String,
    pub path: PathBuf,
    /// Container name to service name, for the services being supervised.
    pub containers: BTreeMap<String, String>,
    pub interval: Duration,
    pub interrupts: Arc<AtomicUsize>,
    pub warnings: Sender<String>,
}

impl Sampler {
    pub fn spawn(self) -> JoinHandle<()> {
        thread::spawn(move || self.work())
    }

    fn work(self) {
        let mut last_warning: Option<String> = None;
        let mut pruned_at: Option<Instant> = None;
        // The first sample waits one interval: the start phase is the supervisor's
        // busiest moment, and a reading from the first seconds of a container
        // says little. It also keeps the sampler's `container list` out of short
        // test runs, where the fake runtime's world file has a single writer.
        while self.wait() {
            if pruned_at.is_none_or(|at| at.elapsed() >= PRUNE_EVERY) {
                let now = clock::wall_ms() / 1_000;
                if let Err(error) = history::prune_at(&self.path, now, history::RETENTION_SECS) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        self.warn(
                            &mut last_warning,
                            format!("cannot prune the history: {error}"),
                        );
                    }
                }
                pruned_at = Some(Instant::now());
            }

            match self.sample() {
                Ok(Some(sample)) => match history::append_at(&self.path, &sample) {
                    Ok(()) => last_warning = None,
                    Err(error) => {
                        self.warn(
                            &mut last_warning,
                            format!("cannot write the history: {error}"),
                        );
                    }
                },
                Ok(None) => {}
                Err(error) => self.warn(&mut last_warning, format!("cannot sample usage: {error}")),
            }
        }
    }

    /// Sleep one interval; `false` when an interrupt arrived meanwhile.
    fn wait(&self) -> bool {
        let deadline = Instant::now() + self.interval;
        while Instant::now() < deadline {
            if self.interrupts.load(Ordering::SeqCst) > 0 {
                return false;
            }
            thread::sleep(Duration::from_millis(100));
        }
        self.interrupts.load(Ordering::SeqCst) == 0
    }

    /// One reading per supervised container the runtime reported on; `None`
    /// when it reported on none of them, so the file records no empty lines.
    fn sample(&self) -> Result<Option<Sample>, String> {
        let stats = observe::stats(&self.runtime).map_err(|error| error.to_string())?;
        let containers = observe::containers(&self.runtime).map_err(|error| error.to_string())?;
        let sample = to_sample(
            clock::wall_ms() / 1_000,
            &self.containers,
            &stats,
            &containers,
        );
        Ok((!sample.services.is_empty()).then_some(sample))
    }

    /// Say each distinct problem once, not every 30 seconds.
    fn warn(&self, last: &mut Option<String>, message: String) {
        if last.as_deref() != Some(message.as_str()) {
            let _ = self.warnings.send(message.clone());
            *last = Some(message);
        }
    }
}

fn to_sample(
    at: u64,
    names: &BTreeMap<String, String>,
    stats: &[ObservedStats],
    containers: &[ObservedContainer],
) -> Sample {
    let services = stats
        .iter()
        .filter_map(|reading| {
            let service = names.get(&reading.id)?;
            let started = containers
                .iter()
                .find(|container| container.id == reading.id)
                .and_then(|container| container.status.started_date.clone());
            Some((
                service.clone(),
                Reading {
                    started,
                    cpu_usec: reading.cpu_usage_usec,
                    mem: reading.memory_usage_bytes,
                    mem_limit: reading.memory_limit_bytes,
                    net_rx: reading.network_rx_bytes,
                    net_tx: reading.network_tx_bytes,
                    blk_read: reading.block_read_bytes,
                    blk_write: reading.block_write_bytes,
                    pids: reading.num_processes,
                },
            ))
        })
        .collect();
    Sample { at, services }
}

#[cfg(test)]
mod tests {
    use super::to_sample;
    use container_compose_runtime::{ObservedContainer, ObservedStats};
    use std::collections::BTreeMap;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/../../fixtures/real/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn only_supervised_containers_become_readings() {
        let stats: Vec<ObservedStats> =
            serde_json::from_str(&fixture("container-stats.json")).unwrap();
        let names = BTreeMap::from([
            ("midnight-lab-node".to_owned(), "node".to_owned()),
            ("midnight-lab-absent".to_owned(), "absent".to_owned()),
        ]);
        let sample = to_sample(1_000, &names, &stats, &[]);

        let services: Vec<&str> = sample.services.keys().map(String::as_str).collect();
        assert_eq!(
            services,
            ["node"],
            "unsupervised and unreported are left out"
        );
        let node = &sample.services["node"];
        assert_eq!(node.cpu_usec, 6_532_864);
        assert_eq!(node.mem, 332_886_016);
        assert_eq!(node.started, None, "no container listed, so no start time");
    }

    #[test]
    fn the_start_time_comes_from_the_container_list() {
        let stats: Vec<ObservedStats> =
            serde_json::from_str(&fixture("container-stats.json")).unwrap();
        // The captured list belongs to a different capture; reuse its `api`
        // container's shape under the node's name.
        let mut containers: Vec<ObservedContainer> =
            serde_json::from_str(&fixture("container-list.json")).unwrap();
        containers.retain(|container| container.status.started_date.is_some());
        containers[0].id = "midnight-lab-node".to_owned();
        let started = containers[0].status.started_date.clone();

        let names = BTreeMap::from([("midnight-lab-node".to_owned(), "node".to_owned())]);
        let sample = to_sample(1_000, &names, &stats, &containers);
        assert_eq!(sample.services["node"].started, started);
    }
}
