//! Resource history: a low-resolution record of each service's usage.
//!
//! While a supervisor runs it appends one [`Sample`] every 30 seconds to
//! `~/Library/Application Support/container-compose/history/<project>.jsonl`,
//! one JSON line per sample, and drops lines older than seven days. The
//! readings are the runtime's raw counters; rates are derived when read, so
//! the file stays a plain record of what the runtime said.
//!
//! A gap in the file means no supervisor was running. A service missing from
//! a sample means the runtime gave no reading for it — never zero usage.
//!
//! `CONTAINER_COMPOSE_HISTORY_DIR` overrides the directory; the tests use the
//! `*_at` functions with a temporary path instead.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const HISTORY_DIR_ENV: &str = "CONTAINER_COMPOSE_HISTORY_DIR";
/// How often a supervisor samples.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(30);
/// How long samples are kept.
pub const RETENTION_SECS: u64 = 7 * 86_400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    /// Epoch seconds.
    pub at: u64,
    /// Readings by service name.
    pub services: BTreeMap<String, Reading>,
}

/// One container's counters, as the runtime reported them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    /// The container's start time. A different value from the previous
    /// reading means the counters restarted from zero.
    pub started: Option<String>,
    pub cpu_usec: u64,
    pub mem: u64,
    pub mem_limit: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub blk_read: u64,
    pub blk_write: u64,
    pub pids: u64,
}

pub fn dir() -> Option<PathBuf> {
    if let Some(overridden) = std::env::var_os(HISTORY_DIR_ENV) {
        return Some(PathBuf::from(overridden));
    }
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home).join("Library/Application Support/container-compose/history")
    })
}

pub fn path(project: &str) -> Option<PathBuf> {
    Some(dir()?.join(format!("{project}.jsonl")))
}

pub fn append_at(path: &Path, sample: &Sample) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_vec(sample)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    line.push(b'\n');
    // One `write` of one line: an append-mode write of a few hundred bytes
    // lands whole, so a reader never sees half a sample.
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&line)
}

/// Every sample in the file. A line that does not parse — a truncated last
/// line after a crash — is skipped rather than failing the read.
pub fn read_at(path: &Path) -> Vec<Sample> {
    fs::read_to_string(path)
        .map(|text| parse_lines(&text, false))
        .unwrap_or_default()
}

/// The samples in the last `max_bytes` of the file, for a reader that only
/// wants the recent past and polls often.
pub fn read_tail_at(path: &Path, max_bytes: u64) -> Vec<Sample> {
    let Ok(mut file) = fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
        return Vec::new();
    };
    let start = length.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    // Starting mid-file lands mid-line; the first line is partial.
    parse_lines(&text, start > 0)
}

fn parse_lines(text: &str, skip_first: bool) -> Vec<Sample> {
    text.lines()
        .skip(usize::from(skip_first))
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Drop samples older than `now - retention`, rewriting the file atomically.
pub fn prune_at(path: &Path, now: u64, retention: u64) -> io::Result<()> {
    let samples = read_at(path);
    let cutoff = now.saturating_sub(retention);
    if samples.iter().all(|sample| sample.at >= cutoff) {
        return Ok(());
    }
    let mut text = String::new();
    for sample in samples.iter().filter(|sample| sample.at >= cutoff) {
        let line = serde_json::to_string(sample)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        text.push_str(&line);
        text.push('\n');
    }
    let temporary = path.with_extension("jsonl.tmp");
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path)
}

/// One service at one sample, with rates over the interval since the reading
/// before it. Rates are `None` on the first reading after a start or a gap in
/// the service's readings, where there is nothing to subtract.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub at: u64,
    /// CPU over the interval, in percent of one core (so 4 busy cores is 400).
    pub cpu_pct: Option<f64>,
    pub mem: u64,
    pub mem_limit: u64,
    pub net_rx_per_sec: Option<f64>,
    pub net_tx_per_sec: Option<f64>,
    pub blk_read_per_sec: Option<f64>,
    pub blk_write_per_sec: Option<f64>,
    pub pids: u64,
    /// This reading starts a new run: the container restarted, or it is the
    /// first reading of the service in the samples given.
    pub starts_run: bool,
}

/// The service's readings as points, oldest first.
pub fn series(samples: &[Sample], service: &str) -> Vec<Point> {
    let mut points = Vec::new();
    let mut previous: Option<(u64, &Reading)> = None;
    for sample in samples {
        let Some(reading) = sample.services.get(service) else {
            // No reading: the next one cannot be differenced across the hole.
            previous = None;
            continue;
        };
        let continuous = previous.filter(|(at, before)| {
            *at < sample.at
                && before.started == reading.started
                && reading.cpu_usec >= before.cpu_usec
                && reading.net_rx >= before.net_rx
                && reading.net_tx >= before.net_tx
                && reading.blk_read >= before.blk_read
                && reading.blk_write >= before.blk_write
        });
        let rate = |now: u64, then: u64| -> Option<f64> {
            continuous.map(|(at, _)| (now - then) as f64 / (sample.at - at) as f64)
        };
        let before = continuous.map(|(_, before)| before);
        points.push(Point {
            at: sample.at,
            cpu_pct: before.and_then(|before| {
                rate(reading.cpu_usec, before.cpu_usec).map(|usec_per_sec| usec_per_sec / 10_000.0)
            }),
            mem: reading.mem,
            mem_limit: reading.mem_limit,
            net_rx_per_sec: before.and_then(|before| rate(reading.net_rx, before.net_rx)),
            net_tx_per_sec: before.and_then(|before| rate(reading.net_tx, before.net_tx)),
            blk_read_per_sec: before.and_then(|before| rate(reading.blk_read, before.blk_read)),
            blk_write_per_sec: before.and_then(|before| rate(reading.blk_write, before.blk_write)),
            pids: reading.pids,
            starts_run: continuous.is_none(),
        });
        previous = Some((sample.at, reading));
    }
    points
}

/// A service's usage over the samples given.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub service: String,
    pub readings: usize,
    pub first_at: u64,
    pub last_at: u64,
    pub cpu_avg_pct: Option<f64>,
    pub cpu_peak_pct: Option<f64>,
    pub mem_avg: u64,
    pub mem_peak: u64,
    pub mem_limit: u64,
    /// Bytes moved within the window, summed over continuous intervals.
    pub net_rx: u64,
    pub net_tx: u64,
    pub blk_read: u64,
    pub blk_write: u64,
    /// Restarts visible in the samples: a reading whose start time changed.
    pub restarts: usize,
}

pub fn summarize(samples: &[Sample], service: &str) -> Option<Summary> {
    let points = series(samples, service);
    let first = points.first()?;
    let last = points.last()?;

    let cpu: Vec<f64> = points.iter().filter_map(|point| point.cpu_pct).collect();
    let mem_total: u128 = points.iter().map(|point| u128::from(point.mem)).sum();
    let moved = |value: fn(&Point) -> Option<f64>| -> u64 {
        points
            .windows(2)
            .filter_map(|pair| value(&pair[1]).map(|rate| rate * (pair[1].at - pair[0].at) as f64))
            .sum::<f64>()
            .round() as u64
    };

    let readings: Vec<&Reading> = samples
        .iter()
        .filter_map(|sample| sample.services.get(service))
        .collect();
    let restarts = readings
        .windows(2)
        .filter(|pair| pair[0].started != pair[1].started)
        .count();

    Some(Summary {
        service: service.to_owned(),
        readings: points.len(),
        first_at: first.at,
        last_at: last.at,
        cpu_avg_pct: (!cpu.is_empty()).then(|| cpu.iter().sum::<f64>() / cpu.len() as f64),
        cpu_peak_pct: cpu.iter().copied().reduce(f64::max),
        mem_avg: u64::try_from(mem_total / points.len() as u128).unwrap_or(u64::MAX),
        mem_peak: points.iter().map(|point| point.mem).max().unwrap_or(0),
        mem_limit: last.mem_limit,
        net_rx: moved(|point| point.net_rx_per_sec),
        net_tx: moved(|point| point.net_tx_per_sec),
        blk_read: moved(|point| point.blk_read_per_sec),
        blk_write: moved(|point| point.blk_write_per_sec),
        restarts,
    })
}

/// `309 MiB`, `1.0 GiB`, `512 B`: binary units, one decimal from GiB up.
pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value >= KIB * KIB * KIB {
        format!("{:.1} GiB", value / (KIB * KIB * KIB))
    } else if value >= KIB * KIB {
        format!("{:.0} MiB", value / (KIB * KIB))
    } else if value >= KIB {
        format!("{:.0} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// `0.7%`, `12%`, `250%`: one decimal below 10.
pub fn format_pct(pct: f64) -> String {
    if pct < 10.0 {
        format!("{pct:.1}%")
    } else {
        format!("{pct:.0}%")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        append_at, format_bytes, format_pct, prune_at, read_at, read_tail_at, series, summarize,
        Reading, Sample,
    };
    use std::collections::BTreeMap;

    fn reading(started: &str, cpu_usec: u64, mem: u64, net_rx: u64) -> Reading {
        Reading {
            started: Some(started.to_owned()),
            cpu_usec,
            mem,
            mem_limit: 1 << 30,
            net_rx,
            net_tx: 0,
            blk_read: 0,
            blk_write: 0,
            pids: 4,
        }
    }

    fn sample(at: u64, node: Option<Reading>) -> Sample {
        Sample {
            at,
            services: node
                .into_iter()
                .map(|reading| ("node".to_owned(), reading))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    const START: &str = "2026-10-26T02:00:00Z";
    const RESTART: &str = "2026-10-26T02:01:10Z";

    #[test]
    fn cpu_is_the_counter_difference_over_the_interval() {
        // 15 s of CPU in 30 s is half a core.
        let samples = [
            sample(1_000, Some(reading(START, 10_000_000, 100, 0))),
            sample(1_030, Some(reading(START, 25_000_000, 200, 3_000))),
        ];
        let points = series(&samples, "node");
        assert_eq!(points[0].cpu_pct, None, "nothing to subtract from");
        assert!(points[0].starts_run);
        assert_eq!(points[1].cpu_pct, Some(50.0));
        assert_eq!(points[1].net_rx_per_sec, Some(100.0));
        assert_eq!(points[1].mem, 200);
        assert!(!points[1].starts_run);
    }

    #[test]
    fn a_restart_or_a_missing_reading_breaks_the_series() {
        let samples = [
            sample(1_000, Some(reading(START, 10_000_000, 100, 0))),
            sample(1_030, Some(reading(START, 11_000_000, 100, 0))),
            // Restarted: the counters are small again, and the start moved.
            sample(1_060, Some(reading(RESTART, 500_000, 50, 0))),
            sample(1_090, Some(reading(RESTART, 800_000, 50, 0))),
            sample(1_120, None),
            sample(1_150, Some(reading(RESTART, 1_100_000, 50, 0))),
        ];
        let points = series(&samples, "node");
        let rates: Vec<Option<f64>> = points.iter().map(|point| point.cpu_pct).collect();
        assert_eq!(
            rates,
            [
                None,
                Some(1_000_000.0 / 30.0 / 10_000.0),
                None,
                Some(1.0),
                None
            ]
        );

        let summary = summarize(&samples, "node").unwrap();
        assert_eq!(summary.restarts, 1);
        assert_eq!(summary.readings, 5);
    }

    #[test]
    fn a_summary_averages_and_peaks_and_sums_what_moved() {
        let samples = [
            sample(1_000, Some(reading(START, 0, 100, 0))),
            sample(1_030, Some(reading(START, 3_000_000, 300, 6_000))),
            sample(1_060, Some(reading(START, 9_000_000, 200, 9_000))),
        ];
        let summary = summarize(&samples, "node").unwrap();
        assert_eq!(summary.cpu_avg_pct, Some(15.0));
        assert_eq!(summary.cpu_peak_pct, Some(20.0));
        assert_eq!(summary.mem_avg, 200);
        assert_eq!(summary.mem_peak, 300);
        assert_eq!(summary.net_rx, 9_000);
        assert_eq!(summary.first_at, 1_000);
        assert_eq!(summary.last_at, 1_060);
        assert!(summarize(&samples, "absent").is_none());
    }

    #[test]
    fn appended_samples_read_back_and_a_torn_line_is_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("lab.jsonl");
        append_at(&path, &sample(1_000, Some(reading(START, 1, 2, 3)))).unwrap();
        append_at(&path, &sample(1_030, Some(reading(START, 4, 5, 6)))).unwrap();
        // A crash mid-write leaves a partial line at the end.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(r#"{"at":1060,"servi"#);
        std::fs::write(&path, text).unwrap();

        let samples = read_at(&path);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].services["node"].cpu_usec, 4);
    }

    #[test]
    fn a_tail_read_drops_the_partial_first_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("lab.jsonl");
        for at in 0..10 {
            append_at(
                &path,
                &sample(1_000 + at * 30, Some(reading(START, at, 0, 0))),
            )
            .unwrap();
        }
        let whole = std::fs::metadata(&path).unwrap().len();
        let line = whole / 10;

        let tail = read_tail_at(&path, line * 3 + line / 2);
        let times: Vec<u64> = tail.iter().map(|sample| sample.at).collect();
        assert_eq!(times, [1_210, 1_240, 1_270]);
        assert_eq!(read_tail_at(&path, whole * 2).len(), 10);
    }

    #[test]
    fn pruning_keeps_only_the_retention_window() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("lab.jsonl");
        for at in [100, 200, 300, 400] {
            append_at(&path, &sample(at, Some(reading(START, 0, 0, 0)))).unwrap();
        }
        prune_at(&path, 450, 200).unwrap();
        let times: Vec<u64> = read_at(&path).iter().map(|sample| sample.at).collect();
        assert_eq!(times, [300, 400]);
        assert!(!dir.path().join("lab.jsonl.tmp").exists());
    }

    #[test]
    fn units_read_at_a_glance() {
        assert_eq!(format_bytes(332_886_016), "317 MiB");
        assert_eq!(format_bytes(1 << 30), "1.0 GiB");
        assert_eq!(format_bytes(2_048), "2 KiB");
        assert_eq!(format_bytes(12), "12 B");
        assert_eq!(format_pct(0.74), "0.7%");
        assert_eq!(format_pct(37.4), "37%");
    }
}
