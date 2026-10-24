//! The plan itself.

use std::collections::BTreeMap;

use container_compose_runtime::{argv, Observed};
use container_compose_stack::model::{NetworkMode, PullPolicy};
use container_compose_stack::resolve::{Journal, ResolvedService, ResolvedStack, ResolvedVolume};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Keep,
    Create,
    Recreate,
    Start,
    Stop,
    Delete,
    Wait,
    Pull,
    Build,
    Warn,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Keep => "keep",
            Action::Create => "create",
            Action::Recreate => "recreate",
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Delete => "delete",
            Action::Wait => "wait",
            Action::Pull => "pull",
            Action::Build => "build",
            Action::Warn => "warn",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Resource {
    Network,
    Volume,
    Container,
    Image,
}

impl Resource {
    pub fn as_str(self) -> &'static str {
        match self {
            Resource::Network => "network",
            Resource::Volume => "volume",
            Resource::Container => "container",
            Resource::Image => "image",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Detail {
    Network {
        mode: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        subnet: Option<String>,
    },
    Volume {
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        journal: Option<String>,
    },
    Container {
        image: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        config: Option<String>,
    },
    Image {
        reference: String,
        action: String,
    },
    Note {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub action: Action,
    pub resource: Resource,
    pub name: String,
    pub detail: Detail,
    pub argv: Option<Vec<String>>,
}

impl Step {
    fn new(
        action: Action,
        resource: Resource,
        name: impl Into<String>,
        detail: Detail,
        argv: Option<Vec<String>>,
    ) -> Self {
        Self {
            action,
            resource,
            name: name.into(),
            detail,
            argv,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Refusal {
    pub path: String,
    pub message: String,
}

impl Refusal {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub project: String,
    pub config: String,
    pub file: String,
    pub steps: Vec<Step>,
    /// Non-empty means `plan` exits 3 and `up` must refuse; the steps are still
    /// printed so the whole picture is visible.
    pub refusals: Vec<Refusal>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// A config-hash mismatch refuses instead of replacing the container.
    pub no_recreate: bool,
}

pub fn plan(stack: &ResolvedStack, observed: &Observed) -> Plan {
    plan_with(stack, observed, &Options::default())
}

pub fn plan_with(stack: &ResolvedStack, observed: &Observed, options: &Options) -> Plan {
    let mut steps = Vec::new();
    let mut refusals = Vec::new();
    let mut warnings = Vec::new();
    let mut starts: Vec<(String, String)> = Vec::new();

    plan_images(stack, observed, &mut steps, &mut refusals);
    plan_network(stack, observed, &mut steps, &mut refusals);
    plan_volumes(stack, observed, &mut steps, &mut refusals);

    for name in &stack.order {
        let service = stack
            .services
            .get(name)
            .expect("the start order names a service");
        let container_name = argv::container_name(&stack.name, name);
        let service_path = format!("[services.{name}]");

        match observed.container(&container_name) {
            None => {
                steps.push(create_step(stack, service, &container_name));
                starts.push((container_name, service.image.clone()));
            }
            Some(found) => {
                if found.project() != Some(stack.name.as_str()) {
                    refusals.push(Refusal::new(
                        service_path,
                        format!(
                            "`{container_name}` exists and is not ours (labels: {})",
                            render_labels(&found.configuration.labels)
                        ),
                    ));
                    continue;
                }

                if found.config_hash() == Some(service.config_hash.as_str()) {
                    steps.push(Step::new(
                        Action::Keep,
                        Resource::Container,
                        container_name.clone(),
                        Detail::Container {
                            image: service.image.clone(),
                            state: Some(found.status.state.clone()),
                            config: Some(service.config_hash.clone()),
                        },
                        None,
                    ));
                    if !found.is_running() {
                        starts.push((container_name, service.image.clone()));
                    }
                } else if options.no_recreate {
                    refusals.push(Refusal::new(
                        service_path,
                        format!(
                            "`{container_name}` matches by name but its config differs; \
                             `--no-recreate` refuses to replace it"
                        ),
                    ));
                } else {
                    if found.is_running() {
                        steps.push(Step::new(
                            Action::Stop,
                            Resource::Container,
                            container_name.clone(),
                            Detail::Container {
                                image: service.image.clone(),
                                state: Some(found.status.state.clone()),
                                config: found.config_hash().map(str::to_owned),
                            },
                            Some(argv::stop_container(&container_name, 15)),
                        ));
                    }
                    steps.push(Step::new(
                        Action::Delete,
                        Resource::Container,
                        container_name.clone(),
                        Detail::Container {
                            image: service.image.clone(),
                            state: Some(found.status.state.clone()),
                            config: found.config_hash().map(str::to_owned),
                        },
                        Some(argv::delete_container(&container_name, false)),
                    ));
                    steps.push(create_step(stack, service, &container_name));
                    starts.push((container_name, service.image.clone()));
                }
            }
        }
    }

    for (container_name, image) in starts {
        steps.push(Step::new(
            Action::Start,
            Resource::Container,
            container_name.clone(),
            Detail::Container {
                image,
                state: None,
                config: None,
            },
            Some(argv::start_container(&container_name)),
        ));
    }

    plan_orphans(stack, observed, &mut warnings);
    steps.extend(warnings);

    Plan {
        project: stack.name.clone(),
        config: stack.config_hash.clone(),
        file: stack.file.display().to_string(),
        steps,
        refusals,
    }
}

fn plan_images(
    stack: &ResolvedStack,
    observed: &Observed,
    steps: &mut Vec<Step>,
    refusals: &mut Vec<Refusal>,
) {
    for name in &stack.order {
        let service = stack
            .services
            .get(name)
            .expect("the start order names a service");

        if let Some(build) = &service.build {
            steps.push(Step::new(
                Action::Build,
                Resource::Image,
                service.image.clone(),
                Detail::Image {
                    reference: service.image.clone(),
                    action: "build".to_owned(),
                },
                Some(argv::build_image(&service.image, build)),
            ));
            continue;
        }

        let pull = match service.pull {
            PullPolicy::Always => true,
            PullPolicy::Missing => observed.image(&service.image).is_none(),
            PullPolicy::Never => {
                if observed.image(&service.image).is_none() {
                    refusals.push(Refusal::new(
                        format!("[services.{name}].pull"),
                        format!(
                            "`pull = \"never\"` but `{}` is not present locally",
                            service.image
                        ),
                    ));
                }
                false
            }
        };

        if pull {
            steps.push(Step::new(
                Action::Pull,
                Resource::Image,
                service.image.clone(),
                Detail::Image {
                    reference: service.image.clone(),
                    action: "pull".to_owned(),
                },
                Some(argv::pull_image(&service.image)),
            ));
        }
    }
}

fn plan_network(
    stack: &ResolvedStack,
    observed: &Observed,
    steps: &mut Vec<Step>,
    refusals: &mut Vec<Refusal>,
) {
    match stack.network.mode {
        NetworkMode::Default => {
            let subnet = observed
                .network("default")
                .and_then(|network| network.subnet())
                .map(str::to_owned);
            steps.push(Step::new(
                Action::Keep,
                Resource::Network,
                "default",
                Detail::Network {
                    mode: "default".to_owned(),
                    subnet,
                },
                None,
            ));
        }
        NetworkMode::Isolated => {
            refusals.push(Refusal::new(
                "[network].mode",
                "isolated networks are not implemented yet (M5); use `mode = \"default\"`",
            ));
        }
    }
}

fn plan_volumes(
    stack: &ResolvedStack,
    observed: &Observed,
    steps: &mut Vec<Step>,
    refusals: &mut Vec<Refusal>,
) {
    for (declared, volume) in &stack.volumes {
        let detail = Detail::Volume {
            size: volume.size.map(|size| size.to_string()),
            journal: volume.journal.as_ref().map(render_journal),
        };

        match observed.volume(&volume.name) {
            None => steps.push(Step::new(
                Action::Create,
                Resource::Volume,
                volume.name.clone(),
                detail,
                Some(argv::create_volume(stack, volume)),
            )),
            Some(found) => {
                if found.project() != Some(stack.name.as_str()) {
                    refusals.push(Refusal::new(
                        format!("[volumes.{declared}]"),
                        format!(
                            "`{}` exists and is not ours (labels: {})",
                            volume.name,
                            render_labels(&found.configuration.labels)
                        ),
                    ));
                    continue;
                }

                let drift = volume_drift(volume, found);
                if drift.is_empty() {
                    steps.push(Step::new(
                        Action::Keep,
                        Resource::Volume,
                        volume.name.clone(),
                        detail,
                        None,
                    ));
                } else {
                    refusals.push(Refusal::new(
                        format!("[volumes.{declared}]"),
                        format!(
                            "{}; a volume's size and journal are creation-time properties, \
                             and applying them would delete the data",
                            drift.join("; ")
                        ),
                    ));
                }
            }
        }
    }
}

fn volume_drift(
    volume: &ResolvedVolume,
    found: &container_compose_runtime::ObservedVolume,
) -> Vec<String> {
    let mut drift = Vec::new();
    if let Some(size) = volume.size {
        if found.configuration.size_in_bytes != 0
            && found.configuration.size_in_bytes != size.bytes()
        {
            drift.push(format!(
                "declared size {size}, volume is {} bytes",
                found.configuration.size_in_bytes
            ));
        }
    }
    if let Some(journal) = &volume.journal {
        let declared = render_journal(journal);
        if let Some(observed_journal) = found.configuration.options.get("journal") {
            if observed_journal != &declared {
                drift.push(format!(
                    "declared journal {declared}, volume is {observed_journal}"
                ));
            }
        }
    }
    drift
}

fn plan_orphans(stack: &ResolvedStack, observed: &Observed, warnings: &mut Vec<Step>) {
    for container in &observed.containers {
        if container.project() != Some(stack.name.as_str()) {
            continue;
        }
        match container.service() {
            Some(service) if stack.services.contains_key(service) => {}
            Some(service) => warnings.push(Step::new(
                Action::Warn,
                Resource::Container,
                container.id.clone(),
                Detail::Note {
                    message: format!(
                        "orphan: service `{service}` is no longer in the stack; `down` removes it"
                    ),
                },
                None,
            )),
            None => warnings.push(Step::new(
                Action::Warn,
                Resource::Container,
                container.id.clone(),
                Detail::Note {
                    message: "labelled with this project but has no service label; \
                              `down` removes it"
                        .to_owned(),
                },
                None,
            )),
        }
    }
}

fn create_step(stack: &ResolvedStack, service: &ResolvedService, container_name: &str) -> Step {
    Step::new(
        Action::Create,
        Resource::Container,
        container_name,
        Detail::Container {
            image: service.image.clone(),
            state: None,
            config: Some(service.config_hash.clone()),
        },
        Some(argv::create_container(stack, service)),
    )
}

fn render_journal(journal: &Journal) -> String {
    let mut value = journal.mode.clone();
    if let Some(size) = journal.size {
        value.push(':');
        value.push_str(&size.to_string());
    }
    value
}

fn render_labels(labels: &BTreeMap<String, String>) -> String {
    if labels.is_empty() {
        return "none".to_owned();
    }
    labels
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(", ")
}
