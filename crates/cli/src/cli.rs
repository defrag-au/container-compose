//! The command line.
//!
//! M1 ships `config` only. The other commands in `docs/03-cli.md` arrive with
//! the runtime adapter, and until then clap refuses them rather than accepting
//! a flag it would ignore.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde::Serialize;

use container_compose_stack::env::{self, Guard};
use container_compose_stack::resolve::{MountSource, ResolvedService, ResolvedStack};
use container_compose_stack::{self as stack, Options};

use crate::error::Error;

#[derive(Debug, Parser)]
#[command(
    name = "container-compose",
    version,
    about = "A composition layer over Apple's container runtime"
)]
pub struct Cli {
    /// The stack file. There is no upward search.
    #[arg(short = 'f', long, default_value = "stack.toml", global = true)]
    pub file: PathBuf,

    /// Override the project name.
    #[arg(short = 'p', long, global = true)]
    pub project_name: Option<String>,

    /// The `container` binary to invoke.
    #[arg(long, default_value = "container", global = true)]
    pub runtime: String,

    /// Echo every `container` argv before running it.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Disable ANSI colour.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Skip the git check on the env file.
    #[arg(long, global = true)]
    pub no_env_check: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the resolved stack, without contacting the runtime.
    Config {
        /// Emit the machine-readable document.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(cli: Cli) -> Result<(), Error> {
    match &cli.command {
        Command::Config { json } => config(&cli, *json),
    }
}

fn config(cli: &Cli, json: bool) -> Result<(), Error> {
    let options = Options {
        project_name: cli.project_name.clone(),
    };
    let stack = stack::load(&cli.file, &options)?;

    if !cli.no_env_check {
        if let Some(path) = &stack.env_file {
            match env::check_secret_file(path) {
                Guard::Tracked => eprintln!(
                    "warning: `{}` is tracked by git; secrets in it will be committed. \
                     Run `git rm --cached {}` and add it to `.gitignore`.",
                    path.display(),
                    path.display()
                ),
                Guard::NotIgnored => eprintln!(
                    "warning: `{}` is not ignored by git; the next `git add .` will commit it. \
                     Add it to `.gitignore`.",
                    path.display()
                ),
                Guard::Ignored | Guard::NotChecked => {}
            }
        }
    }

    let document = ConfigDocument::new(&stack);
    let rendered = if json {
        serde_json::to_string_pretty(&document)
            .map_err(|error| Error::Internal(format!("cannot render the config: {error}")))?
    } else {
        toml::to_string_pretty(&document)
            .map_err(|error| Error::Internal(format!("cannot render the config: {error}")))?
    };

    print!("{rendered}");
    if !rendered.ends_with('\n') {
        println!();
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ConfigDocument {
    version: u32,
    name: String,
    file: String,
    config_hash: String,
    order: Vec<String>,
    network: NetworkDoc,
    volumes: BTreeMap<String, VolumeDoc>,
    services: BTreeMap<String, ServiceDoc>,
}

#[derive(Debug, Serialize)]
struct NetworkDoc {
    mode: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    subnet: Option<String>,
}

#[derive(Debug, Serialize)]
struct VolumeDoc {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal: Option<JournalDoc>,
}

#[derive(Debug, Serialize)]
struct JournalDoc {
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
}

#[derive(Debug, Serialize)]
struct ServiceDoc {
    image: String,
    config_hash: String,
    pull: String,
    restart: String,
    init: bool,
    read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workdir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entrypoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpus: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shm_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cap_add: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cap_drop: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ulimits: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tmpfs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    labels: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<DependencyDoc>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    volumes: Vec<MountDoc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<BuildDoc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    healthcheck: Option<HealthcheckDoc>,
}

#[derive(Debug, Serialize)]
struct DependencyDoc {
    service: String,
    condition: String,
}

#[derive(Debug, Serialize)]
struct MountDoc {
    kind: String,
    source: String,
    target: String,
    read_only: bool,
}

#[derive(Debug, Serialize)]
struct BuildDoc {
    context: String,
    containerfile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    args: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct HealthcheckDoc {
    exec: Vec<String>,
    interval: String,
    timeout: String,
    start_period: String,
    retries: u32,
}

impl ConfigDocument {
    fn new(stack: &ResolvedStack) -> Self {
        Self {
            version: stack.version,
            name: stack.name.clone(),
            file: stack.file.display().to_string(),
            config_hash: stack.config_hash.clone(),
            order: stack.order.clone(),
            network: NetworkDoc {
                mode: stack.network.mode.as_str().to_owned(),
                name: stack.network.name.clone(),
                subnet: stack.network.subnet.clone(),
            },
            volumes: stack
                .volumes
                .iter()
                .map(|(declared, volume)| {
                    (
                        declared.clone(),
                        VolumeDoc {
                            name: volume.name.clone(),
                            size: volume.size.map(|size| size.to_string()),
                            journal: volume.journal.as_ref().map(|journal| JournalDoc {
                                mode: journal.mode.clone(),
                                size: journal.size.map(|size| size.to_string()),
                            }),
                        },
                    )
                })
                .collect(),
            services: stack
                .services
                .iter()
                .map(|(name, service)| (name.clone(), ServiceDoc::new(service)))
                .collect(),
        }
    }
}

impl ServiceDoc {
    fn new(service: &ResolvedService) -> Self {
        Self {
            image: service.image.clone(),
            config_hash: service.config_hash.clone(),
            pull: service.pull.as_str().to_owned(),
            restart: service.restart.as_str().to_owned(),
            init: service.init,
            read_only: service.read_only,
            user: service.user.clone(),
            workdir: service.workdir.clone(),
            entrypoint: service.entrypoint.clone(),
            cpus: service.cpus,
            memory: service.memory.map(|size| size.to_string()),
            shm_size: service.shm_size.map(|size| size.to_string()),
            command: service.command.clone(),
            cap_add: service.cap_add.clone(),
            cap_drop: service.cap_drop.clone(),
            ulimits: service.ulimits.clone(),
            tmpfs: service.tmpfs.clone(),
            ports: service
                .ports
                .iter()
                .map(|port| port.source.clone())
                .collect(),
            aliases: service.aliases.clone(),
            // The config document is not a secret store: every value is
            // replaced, and the hash above is what callers compare.
            env: service
                .env
                .keys()
                .map(|key| (key.clone(), "<redacted>".to_owned()))
                .collect(),
            labels: service.labels.clone(),
            depends_on: service
                .depends_on
                .iter()
                .map(|dependency| DependencyDoc {
                    service: dependency.service.clone(),
                    condition: dependency.condition.as_str().to_owned(),
                })
                .collect(),
            volumes: service
                .volumes
                .iter()
                .map(|mount| {
                    let (kind, source) = match &mount.source {
                        MountSource::Volume { name, .. } => ("volume", name.clone()),
                        MountSource::Bind(path) => ("bind", path.display().to_string()),
                    };
                    MountDoc {
                        kind: kind.to_owned(),
                        source,
                        target: mount.target.clone(),
                        read_only: mount.read_only,
                    }
                })
                .collect(),
            build: service.build.as_ref().map(|build| BuildDoc {
                context: build.context.display().to_string(),
                containerfile: build.containerfile.display().to_string(),
                target: build.target.clone(),
                args: build.args.clone(),
            }),
            healthcheck: service
                .healthcheck
                .as_ref()
                .map(|healthcheck| HealthcheckDoc {
                    exec: healthcheck.exec.clone(),
                    interval: healthcheck.interval.to_string(),
                    timeout: healthcheck.timeout.to_string(),
                    start_period: healthcheck.start_period.to_string(),
                    retries: healthcheck.retries,
                }),
        }
    }
}
