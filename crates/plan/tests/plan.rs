//! The plan rules, without a runtime.
//!
//! Each test builds an `Observed` world directly and asserts the decision, so a
//! failure names the rule rather than a subprocess.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use container_compose_plan::{plan, Action, Detail, Plan, Resource};
use container_compose_runtime::labels;
use container_compose_runtime::{
    ContainerConfiguration, ContainerImage, ContainerStatus, ImageConfiguration,
    NetworkConfiguration, NetworkStatus, Observed, ObservedContainer, ObservedImage,
    ObservedNetwork, ObservedVolume, VolumeConfiguration,
};
use container_compose_stack::{self as stack, Options, ResolvedStack};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .join("stack.toml")
}

fn load(name: &str) -> ResolvedStack {
    stack::load(&fixture(name), &Options::default()).expect("the fixture loads")
}

fn world() -> Observed {
    Observed {
        networks: vec![ObservedNetwork {
            id: "default".to_owned(),
            configuration: NetworkConfiguration::default(),
            status: Some(NetworkStatus {
                ipv4_subnet: Some("192.168.64.0/24".to_owned()),
            }),
        }],
        ..Observed::default()
    }
}

fn with_image(world: &mut Observed, reference: &str) {
    world.images.push(ObservedImage {
        id: reference.to_owned(),
        configuration: ImageConfiguration {
            name: reference.to_owned(),
        },
    });
}

fn container(
    name: &str,
    state: &str,
    config: Option<&str>,
    project: &str,
    service: &str,
) -> ObservedContainer {
    let mut labels_map = BTreeMap::new();
    labels_map.insert(labels::PROJECT.to_owned(), project.to_owned());
    labels_map.insert(labels::SERVICE.to_owned(), service.to_owned());
    if let Some(config) = config {
        labels_map.insert(labels::CONFIG.to_owned(), config.to_owned());
    }
    ObservedContainer {
        id: name.to_owned(),
        configuration: ContainerConfiguration {
            labels: labels_map,
            image: ContainerImage {
                reference: "img".to_owned(),
            },
        },
        status: ContainerStatus {
            state: state.to_owned(),
            started_date: None,
        },
    }
}

fn actions(plan: &Plan) -> Vec<Action> {
    plan.steps.iter().map(|step| step.action).collect()
}

fn temp_stack(text: &str) -> (TempDir, ResolvedStack) {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().join("project");
    fs::create_dir(&project).expect("create project dir");
    let path = project.join("stack.toml");
    fs::write(&path, text).expect("write stack");
    let stack = stack::load(&path, &Options::default()).expect("the stack loads");
    (dir, stack)
}

#[test]
fn a_fresh_stack_pulls_creates_and_starts() {
    let stack = load("minimal");
    let plan = plan(&stack, &world());

    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    assert_eq!(
        actions(&plan),
        vec![Action::Pull, Action::Keep, Action::Create, Action::Start]
    );

    let create = plan
        .steps
        .iter()
        .find(|step| step.action == Action::Create)
        .expect("a create step");
    let argv = create.argv.as_ref().expect("create has argv");
    assert!(argv
        .windows(2)
        .any(|pair| pair[0] == "--name" && pair[1] == "minimal-echo"));
    assert!(argv.iter().any(|argument| argument == "hello"));

    let start = plan
        .steps
        .iter()
        .find(|step| step.action == Action::Start)
        .expect("a start step");
    assert_eq!(
        start.argv.as_deref(),
        Some(&["start".to_owned(), "minimal-echo".to_owned()][..])
    );
}

#[test]
fn a_matching_running_container_is_kept() {
    let stack = load("minimal");
    let mut world = world();
    with_image(&mut world, &stack.services["echo"].image);
    world.containers.push(container(
        "minimal-echo",
        "running",
        Some(&stack.services["echo"].config_hash),
        "minimal",
        "echo",
    ));

    let plan = plan(&stack, &world);
    assert_eq!(actions(&plan), vec![Action::Keep, Action::Keep]);
}

#[test]
fn a_stopped_matching_container_is_started() {
    let stack = load("minimal");
    let mut world = world();
    with_image(&mut world, &stack.services["echo"].image);
    world.containers.push(container(
        "minimal-echo",
        "stopped",
        Some(&stack.services["echo"].config_hash),
        "minimal",
        "echo",
    ));

    let plan = plan(&stack, &world);
    assert_eq!(
        actions(&plan),
        vec![Action::Keep, Action::Keep, Action::Start]
    );
}

#[test]
fn a_config_change_stops_deletes_and_recreates() {
    let stack = load("minimal");
    let mut world = world();
    with_image(&mut world, &stack.services["echo"].image);
    world.containers.push(container(
        "minimal-echo",
        "running",
        Some("old"),
        "minimal",
        "echo",
    ));

    let plan = plan(&stack, &world);
    assert_eq!(
        actions(&plan),
        vec![
            Action::Keep,
            Action::Stop,
            Action::Delete,
            Action::Create,
            Action::Start
        ]
    );
}

#[test]
fn an_unowned_name_is_refused() {
    let stack = load("minimal");
    let mut world = world();
    with_image(&mut world, &stack.services["echo"].image);
    world.containers.push(container(
        "minimal-echo",
        "running",
        Some("old"),
        "someone-else",
        "echo",
    ));

    let plan = plan(&stack, &world);
    assert!(plan
        .refusals
        .iter()
        .any(|refusal| refusal.path == "[services.echo]"));
    assert!(plan
        .refusals
        .iter()
        .any(|refusal| refusal.message.contains("not ours")));
}

#[test]
fn a_volume_size_change_is_refused() {
    let (_dir, stack) = temp_stack(
        "version = 1\n\
         [volumes.data]\nsize = \"2G\"\n\
         [services.a]\nimage = \"img\"\nvolumes = [\"data:/data\"]\n",
    );
    let mut world = world();
    with_image(&mut world, "img");
    let mut labels_map = BTreeMap::new();
    labels_map.insert(labels::PROJECT.to_owned(), "project".to_owned());
    world.volumes.push(ObservedVolume {
        id: "project-data".to_owned(),
        configuration: VolumeConfiguration {
            labels: labels_map,
            options: BTreeMap::new(),
            size_in_bytes: 1 << 30,
        },
    });

    let plan = plan(&stack, &world);
    let refusal = plan
        .refusals
        .iter()
        .find(|refusal| refusal.path == "[volumes.data]")
        .expect("a volume refusal");
    assert!(refusal.message.contains("creation-time"), "{refusal:?}");
}

#[test]
fn restart_and_service_healthy_are_planned_now() {
    let (_dir, stack) = temp_stack(
        "version = 1\n\
         name = \"health\"\n\
         [services.a]\n\
         image = \"img\"\n\
         restart = \"on-stop\"\n\
         [services.a.healthcheck]\n\
         exec = [\"true\"]\n\
         [services.b]\n\
         image = \"img\"\n\
         [[services.b.depends_on]]\n\
         service = \"a\"\n\
         condition = \"service_healthy\"\n",
    );
    let mut world = world();
    with_image(&mut world, "img");

    let plan = plan(&stack, &world);
    assert!(
        plan.refusals.is_empty(),
        "the supervisor honours both now: {:?}",
        plan.refusals
    );
    assert!(plan
        .steps
        .iter()
        .any(|step| step.action == Action::Create && step.name == "health-a"));
    assert!(plan
        .steps
        .iter()
        .any(|step| step.action == Action::Start && step.name == "health-b"));
}

fn network(name: &str, project: Option<&str>, subnet: &str) -> ObservedNetwork {
    let mut labels = BTreeMap::new();
    if let Some(project) = project {
        labels.insert(labels::PROJECT.to_owned(), project.to_owned());
    }
    ObservedNetwork {
        id: name.to_owned(),
        configuration: NetworkConfiguration {
            labels,
            ipv4_subnet: None,
        },
        status: Some(NetworkStatus {
            ipv4_subnet: Some(subnet.to_owned()),
        }),
    }
}

const ISOLATED: &str = "version = 1\nname = \"iso\"\n[network]\nmode = \"isolated\"\nsubnet = \"192.168.77.0/24\"\n[services.a]\nimage = \"img\"\n";

#[test]
fn an_absent_isolated_network_is_created_and_warns_about_dns() {
    let (_dir, stack) = temp_stack(ISOLATED);
    let mut world = world();
    with_image(&mut world, "img");

    let plan = plan(&stack, &world);
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);

    let create = plan
        .steps
        .iter()
        .find(|step| step.action == Action::Create && step.resource == Resource::Network)
        .expect("the network is created");
    assert_eq!(create.name, "iso-net");
    let argv = create.argv.as_ref().expect("create carries argv");
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--subnet", "192.168.77.0/24"]),
        "{argv:?}"
    );

    let warning = plan
        .steps
        .iter()
        .find(|step| step.action == Action::Warn)
        .expect("the DNS caveat is a warning");
    match &warning.detail {
        Detail::Note { message } => assert!(message.contains("apple/container#1809")),
        other => panic!("expected a note, got {other:?}"),
    }
}

#[test]
fn an_isolated_network_we_own_with_the_declared_subnet_is_kept() {
    let (_dir, stack) = temp_stack(ISOLATED);
    let mut world = world();
    with_image(&mut world, "img");
    world
        .networks
        .push(network("iso-net", Some("iso"), "192.168.77.0/24"));

    let plan = plan(&stack, &world);
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    assert!(plan
        .steps
        .iter()
        .any(|step| step.action == Action::Keep && step.resource == Resource::Network));
}

#[test]
fn an_isolated_network_with_a_different_subnet_is_refused() {
    let (_dir, stack) = temp_stack(ISOLATED);
    let mut world = world();
    with_image(&mut world, "img");
    world
        .networks
        .push(network("iso-net", Some("iso"), "192.168.99.0/24"));

    let plan = plan(&stack, &world);
    assert!(
        plan.refusals
            .iter()
            .any(|refusal| refusal.path == "[network].subnet"),
        "{:?}",
        plan.refusals
    );
}

#[test]
fn a_foreign_network_is_attached_only_when_its_subnet_matches() {
    let (_dir, stack) = temp_stack(ISOLATED);
    let mut world = world();
    with_image(&mut world, "img");
    world
        .networks
        .push(network("iso-net", None, "192.168.77.0/24"));
    let matching = plan(&stack, &world);
    assert!(matching.refusals.is_empty(), "{:?}", matching.refusals);

    world.networks.pop();
    world
        .networks
        .push(network("iso-net", None, "192.168.99.0/24"));
    let mismatched = plan(&stack, &world);
    assert!(mismatched
        .refusals
        .iter()
        .any(|refusal| refusal.path == "[network]"));
}

#[test]
fn a_pull_never_service_without_its_image_is_refused() {
    let stack = load("mitos");
    let plan = plan(&stack, &world());

    assert!(plan
        .refusals
        .iter()
        .any(|refusal| refusal.path == "[services.sieve].pull"));
}

#[test]
fn an_orphan_is_reported_not_deleted() {
    let stack = load("minimal");
    let mut world = world();
    with_image(&mut world, &stack.services["echo"].image);
    world.containers.push(container(
        "minimal-gone",
        "running",
        Some("old"),
        "minimal",
        "gone",
    ));

    let plan = plan(&stack, &world);
    let warning = plan
        .steps
        .iter()
        .find(|step| step.action == Action::Warn && step.name == "minimal-gone")
        .expect("an orphan warning");
    assert!(matches!(
        &warning.detail,
        container_compose_plan::Detail::Note { message } if message.contains("orphan")
    ));
}
