//! The health cache: the only thing `ps` can read.
//!
//! Health belongs to a live supervisor, so the supervisor publishes it:
//! `~/Library/Caches/container-compose/<project>.json`, holding its pid, the
//! stack config hash and the last result per service. A missing file, a dead
//! pid or a config-hash mismatch is `unknown` — never a stale "healthy".
//!
//! `CONTAINER_COMPOSE_CACHE_DIR` overrides the directory; the tests use the
//! `*_at` functions with a temporary path instead of the process environment.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

pub const CACHE_DIR_ENV: &str = "CONTAINER_COMPOSE_CACHE_DIR";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCache {
    pub pid: u32,
    pub project: String,
    /// The stack's config hash when the supervisor started.
    pub config: String,
    pub updated_at: String,
    pub services: BTreeMap<String, CachedHealth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedHealth {
    pub state: String,
    pub checked_at: Option<String>,
    /// Restarts this supervisor has issued for the service since it started.
    /// Unlike the tick's attempt counter, this never resets on stable uptime:
    /// it is history for a reader, not backoff state. Absent in a cache
    /// written before it existed, which reads as zero.
    #[serde(default)]
    pub restarts: u32,
    #[serde(default)]
    pub last_restart_at: Option<String>,
    /// The supervisor stopped restarting the service (`max_retries` reached).
    #[serde(default)]
    pub gave_up: bool,
}

pub fn dir() -> Option<PathBuf> {
    if let Some(overridden) = std::env::var_os(CACHE_DIR_ENV) {
        return Some(PathBuf::from(overridden));
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Caches/container-compose"))
}

pub fn path(project: &str) -> Option<PathBuf> {
    Some(dir()?.join(format!("{project}.json")))
}

pub fn write(cache: &HealthCache) -> io::Result<()> {
    let path = path(&cache.project).ok_or_else(no_directory)?;
    write_at(&path, cache)
}

/// Write atomically, so a reader never sees half a document.
pub fn write_at(path: &Path, cache: &HealthCache) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let document = serde_json::to_vec_pretty(cache)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, document)?;
    std::fs::rename(&temporary, path)
}

pub fn read(project: &str) -> Option<HealthCache> {
    read_at(&path(project)?)
}

/// Remove a project's cache. `down` calls this: the supervisor is gone, and a
/// cache whose pid is dead is only clutter.
pub fn remove(project: &str) {
    if let Some(path) = path(project) {
        remove_at(&path);
    }
}

pub fn remove_at(path: &Path) {
    let _ = std::fs::remove_file(path);
}

pub fn read_at(path: &Path) -> Option<HealthCache> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The cache, but only when it can be trusted: it names this project, the
/// writer is alive, and — when the caller has a stack — the config hash still
/// matches.
pub fn live(project: &str, config: Option<&str>) -> Option<HealthCache> {
    trusted(read(project)?, project, config)
}

pub fn trusted(cache: HealthCache, project: &str, config: Option<&str>) -> Option<HealthCache> {
    if cache.project != project || !is_alive(cache.pid) {
        return None;
    }
    if let Some(config) = config {
        if cache.config != config {
            return None;
        }
    }
    Some(cache)
}

pub fn is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn no_directory() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "neither CONTAINER_COMPOSE_CACHE_DIR nor HOME is set",
    )
}

#[cfg(test)]
mod tests {
    use super::{is_alive, read_at, remove_at, trusted, write_at, CachedHealth, HealthCache};
    use std::collections::BTreeMap;

    fn cache(project: &str, pid: u32) -> HealthCache {
        HealthCache {
            pid,
            project: project.to_owned(),
            config: "abc123".to_owned(),
            updated_at: "2026-10-24T00:00:00Z".to_owned(),
            services: BTreeMap::from([(
                "api".to_owned(),
                CachedHealth {
                    state: "healthy".to_owned(),
                    checked_at: Some("2026-10-24T00:00:01Z".to_owned()),
                    restarts: 0,
                    last_restart_at: None,
                    gave_up: false,
                },
            )]),
        }
    }

    #[test]
    fn a_cache_written_before_restart_history_still_reads() {
        // The document this crate wrote before `restarts`, `last_restart_at`
        // and `gave_up` existed, as captured from a live midnight-lab
        // supervisor's cache file.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("midnight-lab.json");
        std::fs::write(
            &path,
            r#"{
  "pid": 43564,
  "project": "midnight-lab",
  "config": "2a9ac5706fb80bbd",
  "updated_at": "2026-10-26T01:18:08Z",
  "services": {
    "indexer": { "state": "unknown", "checked_at": null },
    "node": { "state": "healthy", "checked_at": "2026-10-26T01:18:08Z" }
  }
}"#,
        )
        .unwrap();

        let read_back = read_at(&path).expect("an older cache still parses");
        let node = &read_back.services["node"];
        assert_eq!(node.state, "healthy");
        assert_eq!(node.restarts, 0);
        assert_eq!(node.last_restart_at, None);
        assert!(!node.gave_up);
    }

    #[test]
    fn a_cache_round_trips_and_leaves_no_temporary_behind() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("project.json");
        write_at(&path, &cache("project", std::process::id())).unwrap();

        let read_back = read_at(&path).expect("the cache is readable");
        assert_eq!(read_back.project, "project");
        assert_eq!(read_back.services["api"].state, "healthy");
        assert!(!dir.path().join("project.json.tmp").exists());
    }

    #[test]
    fn a_dead_pid_is_not_trusted() {
        // A pid that cannot be running (well above macOS's maximum).
        assert!(!is_alive(999_999));
        assert!(trusted(cache("dead", 999_999), "dead", Some("abc123")).is_none());
    }

    #[test]
    fn a_config_mismatch_is_not_trusted() {
        let trusted_cache = trusted(cache("same", std::process::id()), "same", Some("abc123"));
        assert!(trusted_cache.is_some());
        assert!(trusted(cache("same", std::process::id()), "same", Some("different")).is_none());
        assert!(trusted(cache("same", std::process::id()), "other", None).is_none());
    }

    #[test]
    fn removing_a_cache_removes_only_that_project() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.json");
        write_at(&first, &cache("first", std::process::id())).unwrap();
        write_at(&second, &cache("second", std::process::id())).unwrap();

        remove_at(&first);
        assert!(read_at(&first).is_none());
        assert!(read_at(&second).is_some());
    }
}
