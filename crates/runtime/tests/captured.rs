//! The captured real-runtime responses parse into the adapter's types.
//!
//! `fixtures/real/` is written by `scripts/capture-runtime-fixtures.sh` against a
//! real `container` 1.6.0. These tests are the compatibility contract: a field the
//! adapter relies on that the runtime stops sending fails here, without a runtime
//! being present at test time.
//!
//! Assertions find the capture's own resources by the `dev.container-compose.project`
//! label rather than counting rows, so a re-capture on a machine with unrelated
//! containers or volumes still passes.

use std::fs;
use std::path::PathBuf;

use container_compose_runtime::{
    ObservedContainer, ObservedImage, ObservedNetwork, ObservedVolume,
};
use serde::Deserialize;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/real")
        .join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn parse<T: serde::de::DeserializeOwned>(name: &str) -> T {
    serde_json::from_str(&fixture(name))
        .unwrap_or_else(|error| panic!("cannot parse {name} into the adapter's shape: {error}"))
}

fn is_capture(project: Option<&str>) -> bool {
    project.is_some_and(|project| project.starts_with("ccfixtures-"))
}

/// The gate `observe` reads before any other call. Mirrored here because the
/// adapter's own struct is private.
#[derive(Deserialize)]
struct SystemStatus {
    status: String,
}

#[test]
fn the_captured_runtime_is_running() {
    let status: SystemStatus = parse("system-status.json");
    assert_eq!(status.status, "running");
}

/// The shape, not the contents: a re-capture on a host with no DNS domain
/// registered legitimately yields `[]`.
#[test]
fn the_captured_dns_domains_are_a_list_of_names() {
    let domains: Vec<String> = parse("dns-list.json");
    assert!(
        domains
            .iter()
            .all(|domain| !domain.is_empty() && !domain.contains(' ')),
        "{domains:?}"
    );
}

#[test]
fn the_captured_containers_parse_and_keep_their_labels() {
    let containers: Vec<ObservedContainer> = parse("container-list.json");

    let api = containers
        .iter()
        .find(|container| is_capture(container.project()) && container.service() == Some("api"))
        .expect("the captured running container");
    assert!(api.is_running(), "`api` was started before the capture");
    assert!(
        api.status.started_date.is_some(),
        "a running container has a startedDate"
    );
    assert_eq!(api.config_hash(), Some("capture"));
    assert_eq!(
        api.configuration.image.reference,
        "docker.io/library/alpine:latest"
    );

    let worker = containers
        .iter()
        .find(|container| is_capture(container.project()) && container.service() == Some("worker"))
        .expect("the captured stopped container");
    assert!(
        !worker.is_running(),
        "`worker` was created but never started"
    );

    // `inspect` returns the same object as one element of `list`, which is why
    // the adapter never needs it. Nothing in the capture has them disagreeing.
    let inspected: Vec<ObservedContainer> = parse("container-inspect.json");
    assert_eq!(inspected.len(), 2);
    for container in &inspected {
        let listed = containers
            .iter()
            .find(|listed| listed.id == container.id)
            .unwrap_or_else(|| panic!("{} is in inspect but not in list", container.id));
        assert_eq!(
            listed.label("dev.container-compose.config"),
            container.label("dev.container-compose.config")
        );
    }
}

#[test]
fn the_captured_volumes_parse() {
    let volumes: Vec<ObservedVolume> = parse("volume-list.json");
    let volume = volumes
        .iter()
        .find(|volume| is_capture(volume.project()))
        .expect("the captured volume");
    assert_eq!(volume.configuration.size_in_bytes, 1024 * 1024 * 1024);
    assert_eq!(
        volume.configuration.options.get("size").map(String::as_str),
        Some("1G"),
        "`options.size` is the string that was given; `sizeInBytes` is the capacity"
    );

    let inspected: Vec<ObservedVolume> = parse("volume-inspect.json");
    assert_eq!(
        inspected.len(),
        1,
        "inspect was scoped to the captured volume"
    );
    assert_eq!(inspected[0].id, volume.id);
    assert_eq!(
        inspected[0].configuration.size_in_bytes,
        volume.configuration.size_in_bytes
    );
}

#[test]
fn the_captured_networks_parse() {
    let networks: Vec<ObservedNetwork> = parse("network-list.json");

    let network = networks
        .iter()
        .find(|network| is_capture(network.project()))
        .expect("the captured network");
    assert!(
        network.subnet().is_some(),
        "`list` already carries status.ipv4Subnet, so reconcile does not inspect"
    );

    let default = networks
        .iter()
        .find(|network| network.id == "default")
        .expect("the builtin default network");
    assert!(default.subnet().is_some());
    assert_eq!(default.project(), None, "the builtin network is not ours");
}

#[test]
fn the_captured_images_parse() {
    let images: Vec<ObservedImage> = parse("image-list.json");
    let alpine = images
        .iter()
        .find(|image| image.configuration.name == "docker.io/library/alpine:latest")
        .expect("the image the capture used");
    assert!(!alpine.id.is_empty());
}
