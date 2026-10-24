//! The hash is a rule, not an implementation detail: the tests pin the
//! exclusions and the order sensitivity that the design promised.

use std::fs;

use container_compose_stack::{self as stack, Options, ResolvedStack};
use tempfile::TempDir;

fn load(text: &str) -> ResolvedStack {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().join("project");
    fs::create_dir(&project).expect("create project dir");
    let path = project.join("stack.toml");
    fs::write(&path, text).expect("write stack");
    stack::load(&path, &Options::default()).expect("the stack loads")
}

fn service_hash(stack: &ResolvedStack, service: &str) -> String {
    stack.services[service].config_hash.clone()
}

#[test]
fn reordering_keys_and_changing_comments_does_not_change_the_hash() {
    let left = load(
        "version = 1\n\
         [services.a]\n\
         image = \"x\"\n\
         cpus = 2\n\
         env = { B = \"2\", A = \"1\" }\n",
    );
    let right = load(
        "# the same stack, written differently\n\
         version = 1\n\
         [services.a]\n\
         env = { A = \"1\", B = \"2\" }\n\
         cpus = 2\n\
         image = \"x\"\n",
    );
    assert_eq!(
        left.services["a"].config_hash,
        right.services["a"].config_hash
    );
    assert_eq!(left.config_hash, right.config_hash);
}

#[test]
fn one_env_value_changes_the_hash() {
    let left = load("version = 1\n[services.a]\nimage = \"x\"\nenv = { RUST_LOG = \"info\" }\n");
    let right = load("version = 1\n[services.a]\nimage = \"x\"\nenv = { RUST_LOG = \"debug\" }\n");
    assert_ne!(service_hash(&left, "a"), service_hash(&right, "a"));
}

#[test]
fn depends_on_does_not_change_the_service_hash() {
    let left = load(
        "version = 1\n\
         [services.a]\nimage = \"x\"\n\
         [services.b]\nimage = \"y\"\n",
    );
    let right = load(
        "version = 1\n\
         [services.a]\nimage = \"x\"\n\
         [services.b]\nimage = \"y\"\n\
         [[services.b.depends_on]]\nservice = \"a\"\ncondition = \"service_started\"\n",
    );
    assert_eq!(service_hash(&left, "b"), service_hash(&right, "b"));
}

#[test]
fn a_port_change_changes_the_hash() {
    let left = load("version = 1\n[services.a]\nimage = \"x\"\nports = [\"8080:80\"]\n");
    let right = load("version = 1\n[services.a]\nimage = \"x\"\nports = [\"8080:81\"]\n");
    assert_ne!(service_hash(&left, "a"), service_hash(&right, "a"));
}

#[test]
fn a_declared_volume_changes_the_stack_hash_but_not_the_service_hash() {
    let left = load("version = 1\n[volumes.data]\nsize = \"1G\"\n[services.a]\nimage = \"x\"\n");
    let right = load("version = 1\n[volumes.data]\nsize = \"2G\"\n[services.a]\nimage = \"x\"\n");
    assert_eq!(service_hash(&left, "a"), service_hash(&right, "a"));
    assert_ne!(left.config_hash, right.config_hash);
}

#[test]
fn a_project_override_does_not_change_an_image_service() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().join("project");
    fs::create_dir(&project).expect("create project dir");
    let path = project.join("stack.toml");
    fs::write(&path, "version = 1\n[services.a]\nimage = \"x\"\n").expect("write");

    let left = stack::load(&path, &Options::default()).expect("loads");
    let right = stack::load(
        &path,
        &Options {
            project_name: Some("other".to_owned()),
        },
    )
    .expect("loads");

    assert_eq!(service_hash(&left, "a"), service_hash(&right, "a"));
    assert_eq!(right.name, "other");
    assert_eq!(left.name, "project");
}

#[test]
fn a_relative_bind_resolves_against_the_stack_file() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("data")).expect("create data");
    let path = project.join("stack.toml");
    fs::write(
        &path,
        "version = 1\n[services.a]\nimage = \"x\"\nvolumes = [\"./data:/data\"]\n",
    )
    .expect("write");

    let stack = stack::load(&path, &Options::default()).expect("loads");
    let mount = &stack.services["a"].volumes[0];
    let container_compose_stack::resolve::MountSource::Bind(source) = &mount.source else {
        panic!("expected a bind mount");
    };
    assert!(source.is_absolute());
    assert!(source.ends_with("data"), "{}", source.display());
    let expected = fs::canonicalize(&project).expect("canonical project");
    assert_eq!(source.parent(), Some(expected.as_path()));
}
