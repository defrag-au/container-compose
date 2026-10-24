//! The real-runtime smoke test.
//!
//! Ignored by default, because it creates real containers, a volume and a
//! network:
//!
//! ```sh
//! cargo test -p container-compose-cli --test smoke -- --ignored --nocapture
//! ```
//!
//! It needs `container system start`. When the runtime is not running it prints
//! why and returns: a machine without the runtime should not fail a design.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;

const STACK: &str = r#"version = 1
[network]
mode = "isolated"
[volumes.data]
size = "1G"
[services.api]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
[services.api.healthcheck]
exec = ["true"]
interval = "1s"
timeout = "2s"
start_period = "0s"
retries = 2
[services.store]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
volumes = ["data:/data"]
"#;

struct Smoke {
    _dir: TempDir,
    stack: PathBuf,
    cache: PathBuf,
    project: String,
}

impl Smoke {
    /// `None` means the runtime is not available; the caller prints and returns.
    fn new() -> Option<Self> {
        let status = Command::new("container")
            .args(["system", "status"])
            .output()
            .ok()?;
        if !status.status.success() {
            return None;
        }

        let dir = TempDir::new().expect("temp dir");
        let project_dir = dir.path().join("project");
        fs::create_dir(&project_dir).expect("create the project dir");
        let stack = project_dir.join("stack.toml");
        fs::write(&stack, STACK).expect("write the stack");
        let cache = dir.path().join("cache");
        fs::create_dir(&cache).expect("create the cache dir");

        Some(Self {
            _dir: dir,
            stack,
            cache,
            project: format!("ccsmoke-{}", std::process::id()),
        })
    }

    fn command(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_container-compose"))
            .env("CONTAINER_COMPOSE_CACHE_DIR", &self.cache)
            .args([
                "-f",
                self.stack.to_str().expect("utf-8"),
                "-p",
                self.project.as_str(),
            ])
            .args(args)
            .output()
            .expect("the binary runs")
    }

    fn network(&self) -> String {
        format!("{}-net", self.project)
    }

    fn volume(&self) -> String {
        format!("{}-data", self.project)
    }

    fn container(&self, service: &str) -> String {
        format!("{}-{service}", self.project)
    }
}

impl Drop for Smoke {
    fn drop(&mut self) {
        let _ = self.command(&["down", "--volumes"]);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn runtime_list(args: &[&str]) -> String {
    Command::new("container")
        .args(args)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
}

#[test]
#[ignore = "creates real containers, a volume and a network"]
fn a_real_stack_round_trips_through_up_exec_run_ps_probe_and_down() {
    let Some(smoke) = Smoke::new() else {
        eprintln!("skipped: the container runtime is not running (`container system status`)");
        return;
    };

    let up = smoke.command(&["up", "-d", "--wait"]);
    assert!(
        up.status.success(),
        "up failed: stdout={} stderr={}",
        stdout(&up),
        stderr(&up)
    );
    assert!(
        stderr(&up).contains("apple/container#1809"),
        "the isolated-network caveat is printed: {}",
        stderr(&up)
    );

    let networks = runtime_list(&["network", "list", "--format", "json"]);
    assert!(
        networks.contains(&smoke.network()),
        "the isolated network exists: {networks}"
    );
    let volumes = runtime_list(&["volume", "list", "--format", "json"]);
    assert!(
        volumes.contains(&smoke.volume()),
        "the volume exists: {volumes}"
    );

    let probe = smoke.command(&["ps", "--probe", "--json"]);
    assert!(
        probe.status.success(),
        "ps --probe failed: {}",
        stderr(&probe)
    );
    let listing = stdout(&probe);
    assert!(listing.contains("\"healthy\""), "{listing}");
    assert!(listing.contains("\"source\": \"probe\""), "{listing}");

    let exec = smoke.command(&["exec", "api", "--", "true"]);
    assert!(exec.status.success(), "exec failed: {}", stderr(&exec));
    let exec = smoke.command(&["exec", "api", "--", "sh", "-c", "echo smoke-ok"]);
    assert!(exec.status.success(), "exec failed: {}", stderr(&exec));
    assert!(stdout(&exec).contains("smoke-ok"), "{}", stdout(&exec));

    let one_off = smoke.command(&["run", "api", "--", "sh", "-c", "echo run-ok"]);
    assert!(
        one_off.status.success(),
        "run failed: stdout={} stderr={}",
        stdout(&one_off),
        stderr(&one_off)
    );
    assert!(stdout(&one_off).contains("run-ok"), "{}", stdout(&one_off));

    let logs = smoke.command(&["logs", "api"]);
    assert!(logs.status.success(), "logs failed: {}", stderr(&logs));

    let down = smoke.command(&["down", "--volumes"]);
    assert!(
        down.status.success(),
        "down failed: stdout={} stderr={}",
        stdout(&down),
        stderr(&down)
    );

    let containers = runtime_list(&["list", "--all", "--format", "json"]);
    assert!(
        !containers.contains(&smoke.container("api")),
        "the container is gone: {containers}"
    );
    let volumes = runtime_list(&["volume", "list", "--format", "json"]);
    assert!(
        !volumes.contains(&smoke.volume()),
        "the volume is gone: {volumes}"
    );
    let networks = runtime_list(&["network", "list", "--format", "json"]);
    assert!(
        !networks.contains(&smoke.network()),
        "the network is gone: {networks}"
    );
}
