//! `up`, `down` and `ps` through the real binary, against the fake runtime.
//!
//! The fake is the `container` on the child's `PATH`, so these tests exercise
//! the same argv, the same polling and the same ownership filters a real runtime
//! would see. What they cannot prove is in `05-testing.md`'s "cannot be faked"
//! table — that is the real-runtime smoke test.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use container_compose_runtime::{
    ContainerConfiguration, ContainerImage, ContainerStatus, Observed, ObservedContainer,
};
use container_compose_supervisor::cache::{write_at, CachedHealth, HealthCache};

#[derive(Debug, Deserialize)]
struct Call {
    program: String,
    argv: Vec<String>,
    /// The values `create` resolved for its `--env` flags, recorded by the fake.
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct PlanDoc {
    config: String,
    steps: Vec<StepDoc>,
}

#[derive(Debug, Deserialize)]
struct StepDoc {
    action: String,
    argv: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct PsDoc {
    name: String,
    service: Option<String>,
    state: String,
    health: HealthDoc,
    image: String,
    ports: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HealthDoc {
    state: String,
    #[serde(default)]
    checked_at: Option<String>,
    source: String,
}

#[derive(Debug, Serialize)]
struct Injection<'a> {
    subcommand: &'a str,
    #[serde(rename = "match")]
    needle: Option<&'a str>,
    exit: i32,
    stderr: &'a str,
}

struct Fake {
    _dir: TempDir,
    bin: PathBuf,
    world: PathBuf,
    cache: PathBuf,
}

impl Fake {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let bin = dir.path().join("bin");
        let world = dir.path().join("world");
        let cache = dir.path().join("cache");
        fs::create_dir(&bin).expect("create bin");
        fs::create_dir(&world).expect("create world");
        fs::create_dir(&cache).expect("create cache");
        symlink(fake_binary(), bin.join("container")).expect("link the fake runtime");
        Self {
            _dir: dir,
            bin,
            world,
            cache,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::cargo_bin("container-compose").expect("the binary builds");
        command
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.bin.display()),
            )
            .env("FAKE_CONTAINER_DIR", &self.world)
            .env("CONTAINER_COMPOSE_CACHE_DIR", &self.cache)
            .args(args);
        command
    }

    fn calls(&self) -> Vec<Call> {
        fs::read_to_string(self.world.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a recorded call"))
            .collect()
    }

    fn reset_calls(&self) {
        let _ = fs::remove_file(self.world.join("calls.jsonl"));
    }

    fn observed(&self) -> Observed {
        self.try_observed().expect("the fake wrote a world")
    }

    fn try_observed(&self) -> Option<Observed> {
        let text = fs::read_to_string(self.world.join("world.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Wait until every named container is running, tolerating a poll that
    /// catches `world.json` mid-write.
    fn wait_until_running(&self, names: &[&str], timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.try_observed().is_some_and(|world| {
                names.iter().all(|name| {
                    world
                        .containers
                        .iter()
                        .any(|container| container.id == *name && container.is_running())
                })
            }) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    fn seed(&self, observed: &Observed) {
        fs::write(
            self.world.join("world.json"),
            serde_json::to_string(observed).expect("json"),
        )
        .expect("seed the world");
    }

    fn inject(&self, injection: &Injection<'_>) {
        fs::write(
            self.world.join("fail.json"),
            serde_json::to_string(injection).expect("json"),
        )
        .expect("write the injection");
    }

    fn container(&self, name: &str) -> Option<ObservedContainer> {
        self.observed()
            .containers
            .into_iter()
            .find(|container| container.id == name)
    }
}

fn fake_binary() -> PathBuf {
    static BUILD: std::sync::Once = std::sync::Once::new();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/fake-container");
    // Build unconditionally: an existing binary may predate the fake's current
    // behaviour, and a stale fake fails as a confusing assertion, not a build
    // error. `Once` keeps the parallel tests from queueing on cargo's lock.
    BUILD.call_once(|| {
        let status = std::process::Command::new(env!("CARGO"))
            .args(["build", "-p", "fake-container"])
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building the fake runtime failed");
    });
    path
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .join("stack.toml")
        .display()
        .to_string()
}

/// A stack file in a directory whose name is a valid project slug, so the
/// project-name rule is satisfied whichever name the file declares.
fn stack_file(dir: &TempDir, contents: &str) -> PathBuf {
    let project = dir.path().join("project");
    fs::create_dir(&project).expect("create the project dir");
    let path = project.join("stack.toml");
    fs::write(&path, contents).expect("write the stack");
    path
}

fn is_mutating(argv: &[String]) -> bool {
    match argv.first().map(String::as_str) {
        Some("create" | "start" | "stop" | "delete" | "build") => true,
        Some("image") => matches!(argv.get(1).map(String::as_str), Some("pull")),
        Some("volume" | "network") => {
            matches!(argv.get(1).map(String::as_str), Some("create" | "delete"))
        }
        _ => false,
    }
}

fn mutating(calls: &[Call]) -> Vec<Vec<String>> {
    calls
        .iter()
        .filter(|call| is_mutating(&call.argv))
        .map(|call| call.argv.clone())
        .collect()
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("utf-8")
}

fn two_services() -> &'static str {
    r#"version = 1
name = "two"
[services.a]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
[services.b]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
"#
}

#[test]
fn up_detached_creates_starts_and_reaches_running() {
    let fake = Fake::new();
    let output = fake
        .command(&["-f", &fixture("minimal"), "up", "-d"])
        .output()
        .expect("the command runs");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("stack minimal is up (1 service)"));

    let container = fake
        .container("minimal-echo")
        .expect("the container was created");
    assert_eq!(
        container.label("dev.container-compose.project"),
        Some("minimal")
    );
    assert_eq!(
        container.label("dev.container-compose.service"),
        Some("echo")
    );
    assert!(
        container.is_running(),
        "up polls until the container is running"
    );

    let argv = fake
        .calls()
        .into_iter()
        .map(|call| call.argv)
        .collect::<Vec<_>>();
    assert!(
        argv.contains(&vec![
            "image".to_owned(),
            "pull".to_owned(),
            "docker.io/library/alpine:latest".to_owned()
        ]),
        "the image was pulled before create"
    );
}

#[test]
fn up_issues_the_plan_argv_verbatim() {
    let fake = Fake::new();
    let plan = fake
        .command(&["-f", &fixture("minimal"), "plan", "--json"])
        .output()
        .expect("the command runs");
    assert!(plan.status.success());
    let document: PlanDoc = serde_json::from_slice(&plan.stdout).expect("a plan document");
    let expected: Vec<Vec<String>> = document
        .steps
        .iter()
        .filter(|step| step.action != "keep" && step.action != "warn" && step.action != "wait")
        .filter_map(|step| step.argv.clone())
        .collect();
    assert!(!expected.is_empty());

    fake.reset_calls();
    let up = fake
        .command(&["-f", &fixture("minimal"), "up", "-d"])
        .output()
        .expect("the command runs");
    assert!(up.status.success(), "stderr: {}", stderr(&up));

    assert_eq!(
        mutating(&fake.calls()),
        expected,
        "`up` ran exactly the argv the plan printed"
    );
    for call in fake.calls() {
        // The recorded program is the fake's executable path, not `argv[0]`:
        // canonicalising it is what proves the fake answered, not a real
        // `container` that happened to be on the PATH.
        assert_eq!(
            fs::canonicalize(&call.program).expect("the recorded program exists"),
            fs::canonicalize(fake_binary()).expect("the fake exists")
        );
    }
}

#[test]
fn a_second_up_adopts_a_running_container() {
    let fake = Fake::new();
    let first = fake
        .command(&["-f", &fixture("minimal"), "up", "-d"])
        .output()
        .expect("the command runs");
    assert!(first.status.success());

    fake.reset_calls();
    let second = fake
        .command(&["-f", &fixture("minimal"), "up", "-d"])
        .output()
        .expect("the command runs");
    assert!(second.status.success(), "stderr: {}", stderr(&second));
    assert!(stderr(&second).contains("keep"));
    assert!(
        mutating(&fake.calls()).is_empty(),
        "a matching running container is adopted, never recreated: {:?}",
        mutating(&fake.calls())
    );
}

#[test]
fn a_config_change_stops_deletes_and_recreates() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(
        &dir,
        r#"version = 1
name = "drift"
[services.api]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
"#,
    );
    let path = path.to_str().expect("utf-8").to_owned();

    let first = fake
        .command(&["-f", &path, "up", "-d"])
        .output()
        .expect("runs");
    assert!(first.status.success(), "stderr: {}", stderr(&first));

    fs::write(
        &path,
        r#"version = 1
name = "drift"
[services.api]
image = "docker.io/library/alpine:latest"
command = ["sleep", "60"]
"#,
    )
    .expect("change the stack");

    fake.reset_calls();
    let second = fake
        .command(&["-f", &path, "up", "-d"])
        .output()
        .expect("runs");
    assert!(second.status.success(), "stderr: {}", stderr(&second));

    let calls = mutating(&fake.calls());
    let verbs: Vec<&str> = calls
        .iter()
        .filter_map(|argv| argv.first().map(String::as_str))
        .collect();
    assert_eq!(verbs, ["stop", "delete", "create", "start"], "{calls:?}");
    assert!(
        fake.container("drift-api")
            .is_some_and(|container| container.is_running()),
        "the replacement is running"
    );
}

#[test]
fn no_recreate_refuses_a_config_mismatch() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(
        &dir,
        r#"version = 1
name = "frozen"
[services.api]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
"#,
    );
    let path = path.to_str().expect("utf-8").to_owned();

    let first = fake
        .command(&["-f", &path, "up", "-d"])
        .output()
        .expect("runs");
    assert!(first.status.success(), "stderr: {}", stderr(&first));

    fs::write(
        &path,
        r#"version = 1
name = "frozen"
[services.api]
image = "docker.io/library/alpine:latest"
command = ["sleep", "60"]
"#,
    )
    .expect("change the stack");

    fake.reset_calls();
    let second = fake
        .command(&["-f", &path, "up", "-d", "--no-recreate"])
        .output()
        .expect("runs");

    assert_eq!(second.status.code(), Some(3), "stderr: {}", stderr(&second));
    assert!(
        stderr(&second).contains("--no-recreate"),
        "{}",
        stderr(&second)
    );
    assert!(
        mutating(&fake.calls()).is_empty(),
        "nothing was replaced: {:?}",
        mutating(&fake.calls())
    );
    assert!(
        fake.container("frozen-api")
            .is_some_and(|container| container.is_running()),
        "the original container is untouched"
    );
}

#[test]
fn an_unowned_name_is_refused_before_any_mutation() {
    let fake = Fake::new();
    fake.seed(&Observed {
        containers: vec![ObservedContainer {
            id: "minimal-echo".to_owned(),
            configuration: ContainerConfiguration {
                labels: BTreeMap::new(),
                image: ContainerImage {
                    reference: "docker.io/library/alpine:latest".to_owned(),
                },
            },
            status: ContainerStatus {
                state: "stopped".to_owned(),
                started_date: None,
            },
        }],
        ..Observed::default()
    });

    let output = fake
        .command(&["-f", &fixture("minimal"), "up", "-d"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("not ours"), "{}", stderr(&output));
    assert!(
        mutating(&fake.calls()).is_empty(),
        "a refusal happens before a pull: {:?}",
        mutating(&fake.calls())
    );
}

#[test]
fn a_failed_start_stops_only_what_this_run_started() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(&dir, two_services());
    fake.inject(&Injection {
        subcommand: "start",
        needle: Some("two-b"),
        exit: 1,
        stderr: "Error: cannot start two-b",
    });

    let output = fake
        .command(&["-f", path.to_str().expect("utf-8"), "up", "-d"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("starting two-b"),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("stopping 1 service(s) this run started"),
        "{}",
        stderr(&output)
    );
    assert!(
        fake.container("two-a")
            .is_some_and(|container| !container.is_running()),
        "the service this run started is stopped"
    );
}

#[test]
fn keep_started_leaves_the_earlier_service_running() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(&dir, two_services());
    fake.inject(&Injection {
        subcommand: "start",
        needle: Some("two-b"),
        exit: 1,
        stderr: "Error: cannot start two-b",
    });

    let output = fake
        .command(&[
            "-f",
            path.to_str().expect("utf-8"),
            "up",
            "-d",
            "--keep-started",
        ])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        fake.container("two-a")
            .is_some_and(|container| container.is_running()),
        "--keep-started leaves what this run started running"
    );
}

#[test]
fn up_starts_a_named_service_and_its_dependencies_only() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(
        &dir,
        r#"version = 1
name = "wave"
[services.a]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
[services.b]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
[[services.b.depends_on]]
service = "a"
condition = "service_started"
[services.c]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
"#,
    );

    let output = fake
        .command(&["-f", path.to_str().expect("utf-8"), "up", "-d", "b"])
        .output()
        .expect("the command runs");
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let started: Vec<String> = mutating(&fake.calls())
        .into_iter()
        .filter(|argv| argv.first().map(String::as_str) == Some("start"))
        .filter_map(|argv| argv.last().cloned())
        .collect();
    assert_eq!(started, ["wave-a", "wave-b"]);
    assert!(
        fake.container("wave-c")
            .is_some_and(|container| !container.is_running()),
        "a service outside the selection is planned but not started"
    );
}

#[test]
fn up_detached_with_a_restart_policy_is_refused() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(
        &dir,
        r#"version = 1
name = "policy"
[services.api]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
restart = "on-stop"
"#,
    );

    let output = fake
        .command(&["-f", path.to_str().expect("utf-8"), "up", "-d"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(3));
    assert!(stderr(&output).contains("restart"), "{}", stderr(&output));
    assert!(mutating(&fake.calls()).is_empty());
}

#[test]
fn an_unknown_service_is_a_usage_error() {
    let fake = Fake::new();
    let output = fake
        .command(&["-f", &fixture("minimal"), "up", "-d", "nope"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("no service named `nope`"));
    assert!(mutating(&fake.calls()).is_empty());
}

#[test]
fn a_dotenv_value_reaches_the_child_without_appearing_in_argv() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(
        &dir,
        r#"version = 1
name = "secrets"
[services.api]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
env = { API_KEY = "${CONTAINER_COMPOSE_TEST_SECRET}" }
"#,
    );
    fs::write(
        path.parent().expect("a parent").join(".env"),
        "CONTAINER_COMPOSE_TEST_SECRET=s3cret-value\n",
    )
    .expect("write the env file");

    let output = fake
        .command(&[
            "-f",
            path.to_str().expect("utf-8"),
            "--no-env-check",
            "up",
            "-d",
        ])
        .output()
        .expect("the command runs");
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let create = fake
        .calls()
        .into_iter()
        .find(|call| call.argv.first().map(String::as_str) == Some("create"))
        .expect("a create call");
    assert!(
        create
            .argv
            .windows(2)
            .any(|pair| pair == ["--env", "API_KEY"]),
        "the argv carries the key only: {:?}",
        create.argv
    );
    assert!(
        !create.argv.join(" ").contains("s3cret-value"),
        "the value never reaches the argv"
    );
    assert_eq!(
        create.env.get("API_KEY").map(String::as_str),
        Some("s3cret-value"),
        "the value reached the child process"
    );
}

#[test]
fn a_port_conflict_names_the_publish_spec() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(
        &dir,
        r#"version = 1
name = "port"
[services.api]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
ports = ["127.0.0.1:18184:8184"]
"#,
    );
    fake.inject(&Injection {
        subcommand: "start",
        needle: Some("port-api"),
        exit: 1,
        stderr: "Error: failed to start container (cause: Address already in use (errno: 48))",
    });

    let output = fake
        .command(&["-f", path.to_str().expect("utf-8"), "up", "-d"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("publishes 127.0.0.1:18184:8184"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn down_removes_exactly_the_projects_containers() {
    let fake = Fake::new();
    let first = fake
        .command(&["-f", &fixture("minimal"), "up", "-d"])
        .output()
        .expect("the command runs");
    assert!(first.status.success());

    let mut world = fake.observed();
    world.containers.push(ObservedContainer {
        id: "stranger".to_owned(),
        configuration: ContainerConfiguration {
            labels: BTreeMap::new(),
            image: ContainerImage {
                reference: "docker.io/library/alpine:latest".to_owned(),
            },
        },
        status: ContainerStatus {
            state: "running".to_owned(),
            started_date: None,
        },
    });
    fake.seed(&world);
    fake.reset_calls();

    let down = fake
        .command(&["-f", &fixture("minimal"), "down"])
        .output()
        .expect("the command runs");
    assert!(down.status.success(), "stderr: {}", stderr(&down));

    let calls = mutating(&fake.calls());
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0][0], "stop");
    assert_eq!(calls[1][0], "delete");
    assert!(
        fake.container("minimal-echo").is_none(),
        "the owned container is gone"
    );
    assert!(
        fake.container("stranger").is_some(),
        "another project's container is never touched"
    );

    let again = fake
        .command(&["-f", &fixture("minimal"), "down"])
        .output()
        .expect("the command runs");
    assert!(again.status.success(), "down is idempotent");
    assert!(stderr(&again).contains("nothing to remove"));
}

#[test]
fn down_removes_volumes_only_with_the_flag() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(
        &dir,
        r#"version = 1
name = "vols"
[volumes.data]
size = "1G"
[services.api]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
volumes = ["data:/data"]
"#,
    );

    let up = fake
        .command(&[
            "-f",
            path.to_str().expect("utf-8"),
            "--no-env-check",
            "up",
            "-d",
        ])
        .output()
        .expect("the command runs");
    assert!(up.status.success(), "stderr: {}", stderr(&up));
    assert_eq!(fake.observed().volumes.len(), 1);

    let down = fake
        .command(&["-f", path.to_str().expect("utf-8"), "down"])
        .output()
        .expect("the command runs");
    assert!(down.status.success(), "stderr: {}", stderr(&down));
    assert_eq!(
        fake.observed().volumes.len(),
        1,
        "a plain `down` keeps the volume"
    );

    fake.reset_calls();
    let down = fake
        .command(&["-f", path.to_str().expect("utf-8"), "down", "--volumes"])
        .output()
        .expect("the command runs");
    assert!(down.status.success(), "stderr: {}", stderr(&down));
    assert!(fake.observed().volumes.is_empty());
    assert_eq!(
        mutating(&fake.calls()),
        vec![vec!["volume", "delete", "vols-data"]]
    );
}

#[test]
fn ps_reports_the_projects_containers_with_unknown_health() {
    let fake = Fake::new();
    let up = fake
        .command(&["-f", &fixture("minimal"), "up", "-d"])
        .output()
        .expect("the command runs");
    assert!(up.status.success());

    let table = fake
        .command(&["-f", &fixture("minimal"), "ps"])
        .output()
        .expect("the command runs");
    assert!(table.status.success(), "stderr: {}", stderr(&table));
    let text = stdout(&table);
    assert!(text.starts_with("NAME"), "{text}");
    assert!(text.contains("minimal-echo"), "{text}");
    assert!(text.contains("running"), "{text}");
    assert!(text.contains("unknown"), "{text}");
    assert!(text.contains("docker.io/library/alpine:latest"), "{text}");

    let document = fake
        .command(&["-f", &fixture("minimal"), "ps", "--json"])
        .output()
        .expect("the command runs");
    assert!(document.status.success());
    let entries: Vec<PsDoc> = serde_json::from_slice(&document.stdout).expect("typed records");
    let entry = entries
        .iter()
        .find(|entry| entry.name == "minimal-echo")
        .expect("the container is listed");
    assert_eq!(entry.service.as_deref(), Some("echo"));
    assert_eq!(entry.state, "running");
    assert_eq!(entry.health.state, "unknown");
    assert_eq!(entry.health.checked_at, None);
    assert_eq!(entry.health.source, "none");
    assert_eq!(entry.image, "docker.io/library/alpine:latest");
    assert!(entry.ports.is_empty());
}

#[test]
fn ps_works_without_the_stack_file() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(
        &dir,
        r#"version = 1
name = "ghost"
[services.api]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
"#,
    );
    let up = fake
        .command(&["-f", path.to_str().expect("utf-8"), "up", "-d"])
        .output()
        .expect("the command runs");
    assert!(up.status.success());
    fs::remove_file(&path).expect("remove the stack file");

    let output = fake
        .command(&[
            "-f",
            path.to_str().expect("utf-8"),
            "-p",
            "ghost",
            "ps",
            "--json",
        ])
        .output()
        .expect("the command runs");
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let entries: Vec<PsDoc> = serde_json::from_slice(&output.stdout).expect("typed records");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "ghost-api");
    assert!(entries[0].ports.is_empty(), "no stack means no port specs");
}

fn gated_stack() -> &'static str {
    r#"version = 1
name = "gate"
[services.a]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
[services.a.healthcheck]
exec = ["true"]
interval = "10ms"
timeout = "1s"
start_period = "0s"
retries = 1
[services.b]
image = "docker.io/library/alpine:latest"
command = ["sleep", "120"]
[[services.b.depends_on]]
service = "a"
condition = "service_healthy"
"#
}

#[test]
fn wait_waits_for_a_healthy_started_service() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(&dir, gated_stack());

    let output = fake
        .command(&["-f", path.to_str().expect("utf-8"), "up", "-d", "--wait"])
        .output()
        .expect("the command runs");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        fake.calls()
            .iter()
            .any(|call| call.argv.first().map(String::as_str) == Some("exec")),
        "readiness ran a probe"
    );
    assert!(fake
        .container("gate-b")
        .is_some_and(|container| container.is_running()));
}

#[test]
fn a_failed_healthcheck_exits_four_and_leaves_the_container_running() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(&dir, gated_stack());
    fake.inject(&Injection {
        subcommand: "exec",
        needle: Some("gate-a"),
        exit: 1,
        stderr: "Error: probe failed",
    });

    let output = fake
        .command(&["-f", path.to_str().expect("utf-8"), "up", "-d", "--wait"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(4), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("did not become healthy"),
        "{}",
        stderr(&output)
    );
    assert!(
        fake.container("gate-a")
            .is_some_and(|container| container.is_running()),
        "exit 4 means the container is running but unhealthy"
    );
}

#[test]
fn service_healthy_holds_the_dependent_start() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(&dir, gated_stack());
    fake.inject(&Injection {
        subcommand: "exec",
        needle: Some("gate-a"),
        exit: 1,
        stderr: "Error: probe failed",
    });

    let output = fake
        .command(&["-f", path.to_str().expect("utf-8"), "up", "-d"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(4), "stderr: {}", stderr(&output));
    let started: Vec<String> = mutating(&fake.calls())
        .into_iter()
        .filter(|argv| argv.first().map(String::as_str) == Some("start"))
        .filter_map(|argv| argv.last().cloned())
        .collect();
    assert_eq!(
        started,
        ["gate-a"],
        "b waits for a's health, and never starts"
    );
    assert!(
        fake.container("gate-b")
            .is_some_and(|container| !container.is_running()),
        "b exists because creates run before starts, but it was not started"
    );
}

#[test]
fn logs_prefix_multiple_services_and_pass_one_through() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(&dir, two_services());
    let path = path.to_str().expect("utf-8").to_owned();
    let up = fake
        .command(&["-f", &path, "up", "-d"])
        .output()
        .expect("runs");
    assert!(up.status.success(), "stderr: {}", stderr(&up));

    let both = fake.command(&["-f", &path, "logs"]).output().expect("runs");
    assert!(both.status.success(), "stderr: {}", stderr(&both));
    let text = stdout(&both);
    assert!(text.contains("a | log line from two-a"), "{text}");
    assert!(text.contains("b | log line from two-b"), "{text}");

    let one = fake
        .command(&["-f", &path, "logs", "a"])
        .output()
        .expect("runs");
    assert!(one.status.success(), "stderr: {}", stderr(&one));
    assert_eq!(stdout(&one).trim(), "log line from two-a");
}

#[test]
fn down_refuses_while_a_supervisor_holds_the_lock() {
    let fake = Fake::new();
    fs::write(
        fake.cache.join("minimal.lock"),
        std::process::id().to_string(),
    )
    .expect("write the lock");

    let output = fake
        .command(&["-f", &fixture("minimal"), "down"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("supervisor is running"),
        "{}",
        stderr(&output)
    );
    assert!(mutating(&fake.calls()).is_empty(), "nothing was stopped");
}

#[test]
fn a_second_up_is_refused_while_the_lock_is_held() {
    let fake = Fake::new();
    fs::write(
        fake.cache.join("minimal.lock"),
        std::process::id().to_string(),
    )
    .expect("write the lock");

    let output = fake
        .command(&["-f", &fixture("minimal"), "up", "-d"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(mutating(&fake.calls()).is_empty());
}

#[test]
fn ps_reads_a_live_supervisors_health_cache() {
    let fake = Fake::new();
    let up = fake
        .command(&["-f", &fixture("minimal"), "up", "-d"])
        .output()
        .expect("runs");
    assert!(up.status.success());

    let plan = fake
        .command(&["-f", &fixture("minimal"), "plan", "--json"])
        .output()
        .expect("runs");
    let document: PlanDoc = serde_json::from_slice(&plan.stdout).expect("a plan");
    let path = fake.cache.join("minimal.json");
    write_at(
        &path,
        &HealthCache {
            pid: std::process::id(),
            project: "minimal".to_owned(),
            config: document.config.clone(),
            updated_at: "2026-10-24T00:00:00Z".to_owned(),
            services: BTreeMap::from([(
                "echo".to_owned(),
                CachedHealth {
                    state: "healthy".to_owned(),
                    checked_at: Some("2026-10-24T00:00:01Z".to_owned()),
                },
            )]),
        },
    )
    .expect("write the cache");

    let listed = fake
        .command(&["-f", &fixture("minimal"), "ps", "--json"])
        .output()
        .expect("runs");
    let entries: Vec<PsDoc> = serde_json::from_slice(&listed.stdout).expect("typed records");
    assert_eq!(entries[0].health.state, "healthy");
    assert_eq!(entries[0].health.source, "supervisor");

    write_at(
        &path,
        &HealthCache {
            pid: std::process::id(),
            project: "minimal".to_owned(),
            config: "stale".to_owned(),
            updated_at: "2026-10-24T00:00:00Z".to_owned(),
            services: BTreeMap::from([(
                "echo".to_owned(),
                CachedHealth {
                    state: "healthy".to_owned(),
                    checked_at: None,
                },
            )]),
        },
    )
    .expect("rewrite the cache");
    let listed = fake
        .command(&["-f", &fixture("minimal"), "ps", "--json"])
        .output()
        .expect("runs");
    let entries: Vec<PsDoc> = serde_json::from_slice(&listed.stdout).expect("typed records");
    assert_eq!(
        entries[0].health.state, "unknown",
        "a config mismatch is never a stale healthy"
    );
    assert_eq!(entries[0].health.source, "none");
}

fn spawn_up(fake: &Fake, path: &str, extra: &[&str]) -> std::process::Child {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_container-compose"));
    command
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", fake.bin.display()),
        )
        .env("FAKE_CONTAINER_DIR", &fake.world)
        .env("CONTAINER_COMPOSE_CACHE_DIR", &fake.cache)
        .args(["-f", path, "up"])
        .args(extra)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command.spawn().expect("the binary starts")
}

fn interrupt(child: &std::process::Child) {
    let status = std::process::Command::new("kill")
        .arg("-INT")
        .arg(child.id().to_string())
        .status()
        .expect("kill runs");
    assert!(status.success(), "the signal was delivered");
}

fn wait_with_deadline(
    mut child: std::process::Child,
    deadline: std::time::Duration,
) -> std::process::Output {
    let limit = std::time::Instant::now() + deadline;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().expect("the output is readable"),
            Ok(None) if std::time::Instant::now() < limit => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                panic!("the supervisor did not exit within {deadline:?}");
            }
        }
    }
}

#[test]
fn ctrl_c_stops_in_reverse_order_and_releases_the_lock() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(&dir, two_services());
    let path_string = path.to_str().expect("utf-8").to_owned();

    let child = spawn_up(&fake, &path_string, &[]);
    assert!(
        fake.wait_until_running(&["two-a", "two-b"], std::time::Duration::from_secs(10)),
        "both containers started before the interrupt"
    );
    // The start phase finishes before the supervisor installs its handlers;
    // SIGINT in that window would take the default action.
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    assert!(
        fake.cache.join("two.lock").exists(),
        "the supervisor holds the lock"
    );

    interrupt(&child);
    let output = wait_with_deadline(child, std::time::Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let text = stderr(&output);
    let b = text.find("stopping two-b").expect("b is stopped first");
    let a = text.find("stopping two-a").expect("a is stopped second");
    assert!(b < a, "reverse dependency order:\n{text}");
    assert!(
        !fake.cache.join("two.lock").exists(),
        "the lock is released"
    );
}

#[test]
fn no_stop_on_exit_leaves_containers_running() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let path = stack_file(&dir, two_services());

    let child = spawn_up(&fake, path.to_str().expect("utf-8"), &["--no-stop-on-exit"]);
    assert!(
        fake.wait_until_running(&["two-a", "two-b"], std::time::Duration::from_secs(10)),
        "both containers started before the interrupt"
    );
    std::thread::sleep(std::time::Duration::from_millis(1_500));

    interrupt(&child);
    let output = wait_with_deadline(child, std::time::Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("leaving"), "{}", stderr(&output));
    assert!(
        fake.container("two-a")
            .is_some_and(|container| container.is_running())
            && fake
                .container("two-b")
                .is_some_and(|container| container.is_running()),
        "the first interrupt was asked to leave them"
    );
}
