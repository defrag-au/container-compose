//! What the runtime reports, parsed.
//!
//! These are the *observed* shapes, not the runtime's full JSON. Only the
//! fields the tool acts on are modelled, and every one of them comes from a
//! captured response in `07-build-plan.md` rather than from the documentation.
//!
//! The fake runtime writes the same types, so a fake world and a real
//! observation cannot drift apart without a compile error.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::command::{Command, RuntimeError};
use crate::labels;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Observed {
    /// The domains `container system dns list` reports. Empty means container
    /// names do not resolve at all — see `docs/04-runtime.md`.
    #[serde(default)]
    pub dns_domains: Vec<String>,
    #[serde(default)]
    pub containers: Vec<ObservedContainer>,
    #[serde(default)]
    pub volumes: Vec<ObservedVolume>,
    #[serde(default)]
    pub networks: Vec<ObservedNetwork>,
    #[serde(default)]
    pub images: Vec<ObservedImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedContainer {
    pub id: String,
    #[serde(default)]
    pub configuration: ContainerConfiguration,
    #[serde(default)]
    pub status: ContainerStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerConfiguration {
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub image: ContainerImage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerImage {
    #[serde(default)]
    pub reference: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerStatus {
    #[serde(default)]
    pub state: String,
    #[serde(rename = "startedDate", default)]
    pub started_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedVolume {
    pub id: String,
    #[serde(default)]
    pub configuration: VolumeConfiguration,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VolumeConfiguration {
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    #[serde(rename = "sizeInBytes", default)]
    pub size_in_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedNetwork {
    pub id: String,
    #[serde(default)]
    pub configuration: NetworkConfiguration,
    #[serde(default)]
    pub status: Option<NetworkStatus>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkConfiguration {
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(rename = "ipv4Subnet", default)]
    pub ipv4_subnet: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkStatus {
    #[serde(rename = "ipv4Subnet", default)]
    pub ipv4_subnet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedImage {
    pub id: String,
    #[serde(default)]
    pub configuration: ImageConfiguration,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageConfiguration {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
struct SystemStatus {
    status: String,
}

/// Read the runtime's world in four calls, gated on `system status`.
pub fn observe(program: &str) -> Result<Observed, RuntimeError> {
    let status: SystemStatus = parse(
        "`container system status`",
        &run(program, &["system", "status", "--format", "json"])?,
    )?;
    if status.status != "running" {
        return Err(RuntimeError::message(format!(
            "the container runtime is not running (status: `{}`); run `container system start`",
            status.status
        )));
    }

    Ok(Observed {
        dns_domains: parse(
            "`container system dns list`",
            &run(program, &["system", "dns", "list", "--format", "json"])?,
        )?,
        containers: parse(
            "`container list`",
            &run(program, &["list", "--all", "--format", "json"])?,
        )?,
        volumes: parse(
            "`container volume list`",
            &run(program, &["volume", "list", "--format", "json"])?,
        )?,
        networks: parse(
            "`container network list`",
            &run(program, &["network", "list", "--format", "json"])?,
        )?,
        images: parse(
            "`container image list`",
            &run(program, &["image", "list", "--format", "json"])?,
        )?,
    })
}

/// `list --all`, parsed. The start phase polls this rather than calling the
/// full `observe`, which is four more subprocesses per poll.
pub fn containers(program: &str) -> Result<Vec<ObservedContainer>, RuntimeError> {
    parse(
        "`container list`",
        &run(program, &["list", "--all", "--format", "json"])?,
    )
}

impl Observed {
    pub fn container(&self, name: &str) -> Option<&ObservedContainer> {
        self.containers
            .iter()
            .find(|container| container.id == name)
    }

    pub fn volume(&self, name: &str) -> Option<&ObservedVolume> {
        self.volumes.iter().find(|volume| volume.id == name)
    }

    pub fn network(&self, name: &str) -> Option<&ObservedNetwork> {
        self.networks.iter().find(|network| network.id == name)
    }

    pub fn image(&self, reference: &str) -> Option<&ObservedImage> {
        let normalized = normalize_image_reference(reference);
        self.images.iter().find(|image| {
            image.configuration.name == reference || image.configuration.name == normalized
        })
    }
}

impl ObservedContainer {
    pub fn label(&self, key: &str) -> Option<&str> {
        self.configuration.labels.get(key).map(String::as_str)
    }

    pub fn project(&self) -> Option<&str> {
        self.label(labels::PROJECT)
    }

    pub fn service(&self) -> Option<&str> {
        self.label(labels::SERVICE)
    }

    pub fn config_hash(&self) -> Option<&str> {
        self.label(labels::CONFIG)
    }

    pub fn is_running(&self) -> bool {
        self.status.state == "running"
    }
}

impl ObservedVolume {
    pub fn project(&self) -> Option<&str> {
        self.configuration
            .labels
            .get(labels::PROJECT)
            .map(String::as_str)
    }
}

impl ObservedNetwork {
    pub fn project(&self) -> Option<&str> {
        self.configuration
            .labels
            .get(labels::PROJECT)
            .map(String::as_str)
    }

    pub fn subnet(&self) -> Option<&str> {
        self.status
            .as_ref()
            .and_then(|status| status.ipv4_subnet.as_deref())
            .or(self.configuration.ipv4_subnet.as_deref())
    }
}

fn run(program: &str, args: &[&str]) -> Result<String, RuntimeError> {
    Ok(Command::new(program)
        .args(args.iter().copied())
        .run()?
        .stdout)
}

fn parse<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> Result<T, RuntimeError> {
    serde_json::from_str(text)
        .map_err(|error| RuntimeError::message(format!("cannot parse {what}: {error}")))
}

/// The reference the runtime records when a short name is pulled.
///
/// `container image pull alpine` stores `docker.io/library/alpine:latest`; the
/// same is true of `midnightntwrk/midnight-node:…` and of a locally built
/// `mitos-local:dev`. Comparing the file's spelling literally makes `pull =
/// "missing"` re-pull forever and makes `pull = "never"` refuse an image that
/// is present.
pub fn normalize_image_reference(reference: &str) -> String {
    let (body, digest) = match reference.split_once('@') {
        Some((body, digest)) => (body, Some(digest)),
        None => (reference, None),
    };

    // A tag is a colon after the last slash; a registry port is before it.
    let last_slash = body.rfind('/');
    let tag_start = body
        .rfind(':')
        .filter(|colon| last_slash.map(|slash| *colon > slash).unwrap_or(true));
    let (name, tag) = match tag_start {
        Some(colon) => (&body[..colon], Some(&body[colon + 1..])),
        None => (body, None),
    };

    let first = name.split('/').next().unwrap_or(name);
    let qualified = first.contains('.') || first.contains(':') || first == "localhost";
    let mut normalized = if qualified {
        name.to_owned()
    } else if name.contains('/') {
        format!("docker.io/{name}")
    } else {
        format!("docker.io/library/{name}")
    };

    if let Some(digest) = digest {
        normalized.push('@');
        normalized.push_str(digest);
    } else {
        normalized.push(':');
        normalized.push_str(tag.unwrap_or("latest"));
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::normalize_image_reference;

    #[test]
    fn short_references_normalize_to_the_runtimes_spelling() {
        assert_eq!(
            normalize_image_reference("alpine"),
            "docker.io/library/alpine:latest"
        );
        assert_eq!(
            normalize_image_reference("alpine:3.22"),
            "docker.io/library/alpine:3.22"
        );
        assert_eq!(
            normalize_image_reference("midnightntwrk/midnight-node:2.1.0-rc.3"),
            "docker.io/midnightntwrk/midnight-node:2.1.0-rc.3"
        );
        assert_eq!(
            normalize_image_reference("mitos-local:dev"),
            "docker.io/library/mitos-local:dev"
        );
    }

    #[test]
    fn a_qualified_reference_is_unchanged() {
        assert_eq!(
            normalize_image_reference("ghcr.io/midnightntwrk/indexer-standalone:4.4.0"),
            "ghcr.io/midnightntwrk/indexer-standalone:4.4.0"
        );
        assert_eq!(
            normalize_image_reference("docker.io/library/alpine:latest"),
            "docker.io/library/alpine:latest"
        );
    }

    #[test]
    fn a_digest_is_preserved_and_no_tag_is_added() {
        assert_eq!(
            normalize_image_reference("docker.io/foo/bar@sha256:abcdef"),
            "docker.io/foo/bar@sha256:abcdef"
        );
    }
}
