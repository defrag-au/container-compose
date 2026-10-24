//! One test per rule in `docs/02-stack-file.md`, each asserting that the
//! message names the key path. A rule that fails without naming where is a rule
//! the user cannot act on.

use std::fs;
use std::path::PathBuf;

use container_compose_stack::error::StackError;
use container_compose_stack::{self as stack, Options, ResolvedStack};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let dir = TempDir::new().expect("temp dir");
        let project = dir.path().join("project");
        fs::create_dir(&project).expect("create project dir");
        fs::write(project.join("stack.toml"), text).expect("write stack");
        Self { dir }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("project").join("stack.toml")
    }

    fn load(&self) -> Result<ResolvedStack, Vec<StackError>> {
        stack::load(&self.path(), &Options::default())
    }
}

fn expect_error(fixture: &Fixture, expected: &str) {
    let errors = fixture.load().expect_err("the stack should not load");
    assert!(
        errors.iter().any(|error| error.path.contains(expected)),
        "expected `{expected}` in {errors:#?}"
    );
}

fn expect_error_in(text: &str, expected: &str) {
    expect_error(&Fixture::new(text), expected);
}

#[test]
fn version_two_is_refused() {
    expect_error_in("version = 2\n[services.a]\nimage = \"x\"\n", "version");
}

#[test]
fn a_service_needs_a_name_the_tool_can_turn_into_a_hostname() {
    expect_error_in(
        "version = 1\n[services.Bad_Name]\nimage = \"x\"\n",
        "[services.Bad_Name]",
    );
}

#[test]
fn an_unknown_key_names_its_table() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\nenvs = {}\n",
        "[services.a].envs",
    );
}

#[test]
fn image_and_build_are_exclusive() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\n[services.a.build]\ncontext = \".\"\n",
        "[services.a]",
    );
}

#[test]
fn a_service_needs_an_image_or_a_build() {
    expect_error_in(
        "version = 1\n[services.a]\ncommand = [\"x\"]\n",
        "[services.a]",
    );
}

#[test]
fn at_least_one_service_is_required() {
    expect_error_in("version = 1\n", "[services]");
}

#[test]
fn a_dependency_must_exist() {
    expect_error_in(
        "version = 1\n\
         [services.a]\nimage = \"x\"\n\
         [[services.a.depends_on]]\nservice = \"nope\"\ncondition = \"service_started\"\n",
        "[services.a].depends_on",
    );
}

#[test]
fn a_service_cannot_depend_on_itself() {
    expect_error_in(
        "version = 1\n\
         [services.a]\nimage = \"x\"\n\
         [[services.a.depends_on]]\nservice = \"a\"\ncondition = \"service_started\"\n",
        "[services.a].depends_on",
    );
}

#[test]
fn a_cycle_names_its_members() {
    let fixture = Fixture::new(
        "version = 1\n\
         [services.a]\nimage = \"x\"\n\
         [[services.a.depends_on]]\nservice = \"b\"\ncondition = \"service_started\"\n\
         [services.b]\nimage = \"x\"\n\
         [[services.b.depends_on]]\nservice = \"a\"\ncondition = \"service_started\"\n",
    );
    let errors = fixture.load().expect_err("a cycle is not loadable");
    let cycle = errors
        .iter()
        .find(|error| error.message.contains("cycle"))
        .expect("a cycle error");
    assert!(cycle.message.contains("a -> b -> a"), "{cycle:?}");
}

#[test]
fn service_healthy_needs_a_healthcheck_on_the_dependency() {
    expect_error_in(
        "version = 1\n\
         [services.a]\nimage = \"x\"\n\
         [[services.a.depends_on]]\nservice = \"b\"\ncondition = \"service_healthy\"\n\
         [services.b]\nimage = \"x\"\n",
        "[services.a].depends_on",
    );
}

#[test]
fn a_volume_source_must_be_declared_or_a_host_path() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\nvolumes = [\"spool:/spool\"]\n",
        "[services.a].volumes[0]",
    );
}

#[test]
fn one_volume_cannot_be_mounted_by_two_services() {
    let fixture = Fixture::new(
        "version = 1\n\
         [volumes.data]\nsize = \"1G\"\n\
         [services.a]\nimage = \"x\"\nvolumes = [\"data:/data\"]\n\
         [services.b]\nimage = \"x\"\nvolumes = [\"data:/data:ro\"]\n",
    );
    expect_error(&fixture, "[services.b].volumes[0]");
}

#[test]
fn a_size_must_parse() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\nmemory = \"8 GiB\"\n",
        "[services.a].memory",
    );
}

#[test]
fn cpus_must_be_positive() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\ncpus = 0\n",
        "[services.a].cpus",
    );
}

#[test]
fn retries_must_be_at_least_one() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\n\
         [services.a.healthcheck]\nexec = [\"true\"]\nretries = 0\n",
        "[services.a].healthcheck.retries",
    );
}

#[test]
fn a_port_must_parse() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\nports = [\"127.0.0.1:8184:8184:1\"]\n",
        "[services.a].ports[0]",
    );
}

#[test]
fn two_services_cannot_publish_the_same_host_port() {
    let fixture = Fixture::new(
        "version = 1\n\
         [services.a]\nimage = \"x\"\nports = [\"8080:80\"]\n\
         [services.b]\nimage = \"x\"\nports = [\"8080:81\"]\n",
    );
    expect_error(&fixture, "[services.b].ports[0]");
}

#[test]
fn an_env_key_must_be_a_variable_name() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\nenv = { \"1BAD\" = \"x\" }\n",
        "[services.a].env",
    );
}

#[test]
fn a_reserved_label_is_refused() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\n\
         labels = { \"dev.container-compose.project\" = \"x\" }\n",
        "[services.a].labels",
    );
}

#[test]
fn a_subnet_only_applies_to_an_isolated_network() {
    expect_error_in(
        "version = 1\n[network]\nsubnet = \"192.168.100.0/24\"\n[services.a]\nimage = \"x\"\n",
        "[network].subnet",
    );
}

#[test]
fn a_duration_must_have_a_unit() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\n\
         [services.a.healthcheck]\nexec = [\"true\"]\ninterval = \"30\"\n",
        "[services.a].healthcheck.interval",
    );
}

#[test]
fn an_unset_substitution_names_the_key() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\n\
         env = { API_KEY = \"${CONTAINER_COMPOSE_TEST_UNSET_9F3A}\" }\n",
        "[services.a].env.API_KEY",
    );
}

#[test]
fn a_declared_env_file_must_exist() {
    expect_error_in(
        "version = 1\n[env]\nfile = \"missing.env\"\n[services.a]\nimage = \"x\"\n",
        "[env].file",
    );
}

#[test]
fn an_entrypoint_array_is_a_type_error() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\nentrypoint = [\"/bin/echo\"]\n",
        "[services.a].entrypoint",
    );
}

#[test]
fn max_retries_only_applies_to_on_stop() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\nrestart = \"always\"\nmax_retries = 3\n",
        "[services.a].max_retries",
    );
}

#[test]
fn a_wildcard_alias_is_rejected() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\naliases = [\"*.test\"]\n",
        "[services.a].aliases",
    );
}

#[test]
fn a_bind_source_must_exist() {
    expect_error_in(
        "version = 1\n[services.a]\nimage = \"x\"\nvolumes = [\"./missing:/data\"]\n",
        "[services.a].volumes[0]",
    );
}

#[test]
fn a_bind_source_that_exists_loads() {
    let fixture =
        Fixture::new("version = 1\n[services.a]\nimage = \"x\"\nvolumes = [\"./data:/data:ro\"]\n");
    fs::create_dir(fixture.dir.path().join("project").join("data")).expect("create data");
    let stack = fixture.load().expect("the bind source exists");
    assert_eq!(stack.services["a"].volumes.len(), 1);
}

#[test]
fn the_mitos_fixture_loads() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mitos/stack.toml");
    let stack = stack::load(&path, &Options::default()).expect("the fixture loads");
    assert_eq!(stack.name, "mitos");
    assert_eq!(stack.order, vec!["sieve", "ledger"]);
    assert_eq!(stack.services.len(), 2);
    assert_eq!(stack.volumes.len(), 3);
    assert_eq!(stack.config_hash.len(), 16);
}

#[test]
fn the_minimal_fixture_loads() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/minimal/stack.toml");
    let stack = stack::load(&path, &Options::default()).expect("the fixture loads");
    assert_eq!(stack.order, vec!["echo"]);
    assert_eq!(stack.network.name, "default");
}
