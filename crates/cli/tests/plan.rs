//! `plan` through the real binary, against the fake runtime.
//!
//! The fake is on `PATH` as `container`, so the argv the plan prints is the argv
//! the tool would issue: the process is fake, the command line is not.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde::Deserialize;
use tempfile::TempDir;

#[derive(Debug, Deserialize)]
struct PlanDoc {
    project: String,
    config: String,
    steps: Vec<StepDoc>,
    refusals: Vec<RefusalDoc>,
}

#[derive(Debug, Deserialize)]
struct StepDoc {
    action: String,
    resource: String,
    argv: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RefusalDoc {
    path: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct Call {
    program: String,
    argv: Vec<String>,
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
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .join("stack.toml")
        .display()
        .to_string()
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

#[test]
fn the_minimal_plan_commands_match_the_golden() {
    let fake = Fake::new();
    let output = fake
        .command(&["-f", &fixture("minimal"), "plan", "--commands"])
        .output()
        .expect("the command runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");

    let golden =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden/minimal.commands");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&golden, &stdout).expect("write golden");
    } else {
        let expected = fs::read_to_string(&golden).expect("the golden file exists");
        assert_eq!(
            expected, stdout,
            "the plan changed; regenerate with UPDATE_GOLDEN=1 and read the diff"
        );
    }
}

#[test]
fn the_plan_issues_only_read_commands() {
    let fake = Fake::new();
    let output = fake
        .command(&["-f", &fixture("minimal"), "plan"])
        .output()
        .expect("the command runs");
    assert!(output.status.success());

    let calls = fake.calls();
    assert!(!calls.is_empty(), "the runtime was never observed");
    for call in calls {
        let argv = call.argv.join(" ");
        assert!(
            !argv.starts_with("create")
                && !argv.starts_with("start")
                && !argv.starts_with("stop")
                && !argv.starts_with("delete")
                && !argv.starts_with("image pull"),
            "plan issued a mutating call: {} {}",
            call.program,
            argv
        );
    }
}

#[test]
fn plan_json_is_one_typed_document() {
    let fake = Fake::new();
    let output = fake
        .command(&["-f", &fixture("minimal"), "plan", "--json"])
        .output()
        .expect("the command runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let document: PlanDoc = serde_json::from_slice(&output.stdout).expect("one JSON document");
    assert_eq!(document.project, "minimal");
    assert_eq!(document.config.len(), 16);
    assert!(document.refusals.is_empty());
    assert!(!document.steps.is_empty());
    assert!(document
        .steps
        .iter()
        .any(|step| step.action == "create" && step.resource == "container"));
    assert!(document
        .steps
        .iter()
        .filter(|step| step.action == "start")
        .all(|step| step.argv.is_some()));
}

#[test]
fn a_refusal_prints_the_plan_and_exits_three() {
    let fake = Fake::new();
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().join("project");
    fs::create_dir(&project).expect("create project dir");
    let stack = project.join("stack.toml");
    fs::write(
        &stack,
        "version = 1\n[services.a]\nimage = \"img\"\npull = \"never\"\n",
    )
    .expect("write stack");

    let output = fake
        .command(&["-f", stack.to_str().expect("utf-8"), "plan", "--json"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(stderr.contains("refused"), "{stderr}");
    assert!(stderr.contains("[services.a].pull"), "{stderr}");

    let document: PlanDoc =
        serde_json::from_slice(&output.stdout).expect("the plan is still printed");
    let refusal = document
        .refusals
        .iter()
        .find(|refusal| refusal.path == "[services.a].pull")
        .expect("a pull refusal");
    assert!(refusal.message.contains("not present"), "{refusal:?}");
}

#[test]
fn the_fake_is_the_container_on_path() {
    let fake = Fake::new();
    let output = fake
        .command(&["-f", &fixture("minimal"), "plan"])
        .output()
        .expect("the command runs");
    assert!(output.status.success());

    // `container` on the child's PATH is the fake, and the fake is what wrote
    // the calls. The recorded `program` is argv[0], which is literally
    // `container`; the symlink is the proof of which binary answered.
    let link = fake.bin.join("container");
    let target = fs::read_link(&link).expect("the PATH entry is a symlink");
    assert_eq!(
        fs::canonicalize(target).expect("the target exists"),
        fs::canonicalize(fake_binary()).expect("the fake exists")
    );

    let calls = fake.calls();
    assert!(!calls.is_empty(), "the fake was not invoked");
    for call in calls {
        // `program` is the fake's own executable path. Canonicalising it proves
        // the binary that answered, which an `argv[0]` of `container` could not.
        assert_eq!(
            fs::canonicalize(&call.program).expect("the recorded program exists"),
            fs::canonicalize(fake_binary()).expect("the fake exists")
        );
    }
}
