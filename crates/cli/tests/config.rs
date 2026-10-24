//! The `config` command, through the real binary.
//!
//! `config` is the one command that must work with no runtime and no git, and
//! `--json` is the document everything else is compared against, so it is
//! tested end to end rather than through the library.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde::Deserialize;
use tempfile::TempDir;

/// Fixtures live at the workspace root, so both crates' tests read the same
/// files. `CARGO_MANIFEST_DIR` is `crates/cli`.
fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .join("stack.toml")
        .display()
        .to_string()
}

#[derive(Debug, Deserialize)]
struct Config {
    name: String,
    file: String,
    config_hash: String,
    order: Vec<String>,
    network: Network,
    services: BTreeMap<String, Service>,
}

#[derive(Debug, Deserialize)]
struct Network {
    mode: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct Service {
    image: String,
    config_hash: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

fn config_json(path: &str) -> (Config, String) {
    let output = Command::cargo_bin("container-compose")
        .expect("the binary builds")
        .args(["-f", path, "config", "--json"])
        .output()
        .expect("the command runs");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "config must not write to stderr without a reason: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let config: Config = serde_json::from_str(&stdout).expect("one JSON document");
    (config, stdout)
}

#[test]
fn config_json_is_one_document_with_redacted_env() {
    let (config, stdout) = config_json(&fixture("mitos"));

    assert_eq!(config.name, "mitos");
    assert_eq!(config.network.mode, "default");
    assert_eq!(config.network.name, "default");
    assert_eq!(config.order, vec!["sieve", "ledger"]);
    assert_eq!(config.config_hash.len(), 16);
    assert!(config.file.ends_with("fixtures/mitos/stack.toml"));

    let sieve = &config.services["sieve"];
    assert_eq!(sieve.image, "mitos-local:dev");
    assert_eq!(sieve.env["RUST_LOG"], "<redacted>");
    assert_eq!(sieve.config_hash.len(), 16);

    assert!(
        !stdout.contains("wallet_sieve=debug"),
        "a secret must not be printed: {stdout}"
    );
}

#[test]
fn config_without_json_renders_toml() {
    let path = fixture("minimal");
    let output = Command::cargo_bin("container-compose")
        .expect("the binary builds")
        .args(["-f", path.as_str(), "config"])
        .output()
        .expect("the command runs");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    assert!(stdout.contains("name = \"minimal\""), "{stdout}");
    assert!(stdout.contains("[services.echo]"), "{stdout}");
}

#[test]
fn an_invalid_stack_exits_two_and_names_the_key() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("stack.toml");
    fs::write(
        &path,
        "version = 1\n[services.a]\nimage = \"x\"\nenvs = {}\n",
    )
    .expect("write");

    let output = Command::cargo_bin("container-compose")
        .expect("the binary builds")
        .args(["-f", path.to_str().expect("utf-8"), "config"])
        .output()
        .expect("the command runs");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(stderr.contains("[services.a].envs"), "{stderr}");
    assert!(output.stdout.is_empty(), "nothing belongs on stdout");
}

#[test]
fn a_project_override_changes_the_name() {
    let path = fixture("minimal");
    let output = Command::cargo_bin("container-compose")
        .expect("the binary builds")
        .args(["-f", path.as_str(), "-p", "other", "config", "--json"])
        .output()
        .expect("the command runs");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let config: Config = serde_json::from_str(&stdout).expect("json");
    assert_eq!(config.name, "other");
}

#[test]
fn a_dotenv_value_is_substituted_and_never_printed() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().join("project");
    fs::create_dir(&project).expect("create project dir");
    fs::write(
        project.join(".env"),
        "CONTAINER_COMPOSE_TEST_KEY_7C1=super-secret-value\n",
    )
    .expect("write env file");
    fs::write(
        project.join("stack.toml"),
        "version = 1\n\
         [services.a]\n\
         image = \"x\"\n\
         env = { API_KEY = \"${CONTAINER_COMPOSE_TEST_KEY_7C1}\" }\n",
    )
    .expect("write stack");

    let output = Command::cargo_bin("container-compose")
        .expect("the binary builds")
        .args([
            "-f",
            project.join("stack.toml").to_str().expect("utf-8"),
            "config",
            "--json",
        ])
        .output()
        .expect("the command runs");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    assert!(
        !stdout.contains("super-secret-value"),
        "the value must not be printed: {stdout}"
    );
    assert!(stdout.contains("<redacted>"), "{stdout}");
}
