//! The resolved model: defaults filled in, env values substituted, host paths
//! absolute, dependency order computed, hashes assigned.
//!
//! Nothing below this point re-reads the file. `config`, `plan` and `up` all
//! work from these types, which is what makes the hash and the argv agree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::StackError;

use super::duration::Span;
use super::env::{self, EnvSource};
use super::hash;
use super::model::{
    BuildConfig, Condition, HealthcheckConfig, NetworkMode, PullPolicy, RestartPolicy,
    ServiceConfig, StackFile, VolumeConfig,
};
use super::parse;
use super::ports::PortSpec;
use super::size::Size;
use super::validate::{self, is_slug, start_order};

#[derive(Debug, Default, Clone)]
pub struct Options {
    pub project_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedStack {
    pub version: u32,
    pub name: String,
    /// The stack file, absolute and canonical.
    pub file: PathBuf,
    /// The directory holding it; relative bind mounts resolve against this.
    pub dir: PathBuf,
    pub network: ResolvedNetwork,
    /// Keyed by the name in the file; the value carries the resource name.
    pub volumes: BTreeMap<String, ResolvedVolume>,
    pub services: BTreeMap<String, ResolvedService>,
    /// Start order, dependencies first, ties broken by name.
    pub order: Vec<String>,
    /// The header hash; per-service hashes are on each service.
    pub config_hash: String,
    pub env_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ResolvedNetwork {
    pub mode: NetworkMode,
    /// `default`, or `<project>-net`.
    pub name: String,
    pub subnet: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedVolume {
    pub name: String,
    pub size: Option<Size>,
    pub journal: Option<Journal>,
}

#[derive(Debug, Clone)]
pub struct Journal {
    pub mode: String,
    pub size: Option<Size>,
}

#[derive(Debug, Clone)]
pub struct ResolvedService {
    pub name: String,
    pub image: String,
    pub build: Option<ResolvedBuild>,
    pub pull: PullPolicy,
    pub command: Option<Vec<String>>,
    pub entrypoint: Option<String>,
    pub env: BTreeMap<String, String>,
    pub user: Option<String>,
    pub workdir: Option<String>,
    pub init: bool,
    pub shm_size: Option<Size>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub ulimits: Vec<String>,
    pub volumes: Vec<ResolvedMount>,
    pub tmpfs: Vec<String>,
    pub read_only: bool,
    pub ports: Vec<PortSpec>,
    pub aliases: Vec<String>,
    pub depends_on: Vec<ResolvedDependency>,
    pub restart: RestartPolicy,
    pub max_retries: u32,
    pub healthcheck: Option<ResolvedHealthcheck>,
    pub labels: BTreeMap<String, String>,
    pub cpus: Option<f64>,
    pub memory: Option<Size>,
    pub config_hash: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedBuild {
    /// Absolute, and the argument to `container build`.
    pub context: PathBuf,
    /// Absolute; passed as `-f`.
    pub containerfile: PathBuf,
    pub target: Option<String>,
    pub args: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum MountSource {
    Volume {
        /// `<project>-<declared>`.
        name: String,
        declared: String,
    },
    Bind(PathBuf),
}

#[derive(Debug, Clone)]
pub struct ResolvedMount {
    pub source: MountSource,
    pub target: String,
    pub read_only: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedDependency {
    pub service: String,
    pub condition: Condition,
}

#[derive(Debug, Clone)]
pub struct ResolvedHealthcheck {
    pub exec: Vec<String>,
    pub interval: Span,
    pub timeout: Span,
    pub start_period: Span,
    pub retries: u32,
}

struct Context<'a> {
    project: &'a str,
    dir: &'a Path,
    declared_volumes: &'a BTreeMap<String, VolumeConfig>,
    env: &'a EnvSource,
    /// Resolved volume name -> (declared name, service that mounts it).
    volume_users: &'a mut BTreeMap<String, (String, String)>,
    /// Host binding -> (service, spec).
    bindings: &'a mut BTreeMap<(String, u16), (String, String)>,
}

pub fn load(path: &Path, options: &Options) -> Result<ResolvedStack, Vec<StackError>> {
    let raw = parse::parse_file(path)?;
    let errors = validate::validate(&raw);
    if !errors.is_empty() {
        return Err(errors);
    }
    resolve(raw, path, options)
}

pub fn resolve(
    raw: StackFile,
    path: &Path,
    options: &Options,
) -> Result<ResolvedStack, Vec<StackError>> {
    let file = std::fs::canonicalize(path).map_err(|error| {
        one(
            path.display().to_string(),
            format!("cannot resolve the stack file: {error}"),
        )
    })?;
    let dir = file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let project = options
        .project_name
        .clone()
        .or_else(|| raw.name.clone())
        .unwrap_or_else(|| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("stack")
                .to_owned()
        });
    if !is_slug(&project) {
        return Err(one(
            "name",
            format!("`{project}` must match [a-z0-9][a-z0-9-]{{0,30}}"),
        ));
    }

    let env = env::load(raw.env.as_ref().and_then(|env| env.file.as_deref()), &dir)
        .map_err(|error| vec![error])?;

    let network = resolve_network(&raw, &project);
    let volumes = resolve_volumes(&raw, &project)?;

    let mut volume_users = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    let mut services = BTreeMap::new();

    for (name, config) in &raw.services {
        let mut context = Context {
            project: &project,
            dir: &dir,
            declared_volumes: &raw.volumes,
            env: &env,
            volume_users: &mut volume_users,
            bindings: &mut bindings,
        };
        let service = resolve_service(name, config, &mut context)?;
        services.insert(name.clone(), service);
    }

    let order = start_order(&raw);

    for service in services.values_mut() {
        let digest = hash::service_hash(service);
        service.config_hash = digest;
    }
    let config_hash = hash::stack_hash(&network, &volumes, &services);

    Ok(ResolvedStack {
        version: raw.version,
        name: project,
        file,
        dir,
        network,
        volumes,
        services,
        order,
        config_hash,
        env_file: env.path,
    })
}

fn resolve_network(raw: &StackFile, project: &str) -> ResolvedNetwork {
    let mode = raw
        .network
        .as_ref()
        .and_then(|network| network.mode)
        .unwrap_or(NetworkMode::Default);
    let subnet = raw
        .network
        .as_ref()
        .and_then(|network| network.subnet.clone());
    let name = match mode {
        NetworkMode::Default => "default".to_owned(),
        NetworkMode::Isolated => format!("{project}-net"),
    };
    ResolvedNetwork { mode, name, subnet }
}

fn resolve_volumes(
    raw: &StackFile,
    project: &str,
) -> Result<BTreeMap<String, ResolvedVolume>, Vec<StackError>> {
    let mut volumes = BTreeMap::new();
    for (name, config) in &raw.volumes {
        let path = format!("[volumes.{name}]");
        let size = match &config.size {
            Some(value) => Some(parse_size(value, &format!("{path}.size"))?),
            None => None,
        };
        let journal = match &config.journal {
            Some(value) => Some(parse_journal(value, &format!("{path}.journal"))?),
            None => None,
        };
        volumes.insert(
            name.clone(),
            ResolvedVolume {
                name: format!("{project}-{name}"),
                size,
                journal,
            },
        );
    }
    Ok(volumes)
}

fn resolve_service(
    name: &str,
    config: &ServiceConfig,
    context: &mut Context<'_>,
) -> Result<ResolvedService, Vec<StackError>> {
    let path = format!("[services.{name}]");

    let image = match (&config.image, &config.build) {
        (Some(image), _) => image.clone(),
        (None, Some(_)) => format!("{}-{name}:local", context.project),
        (None, None) => unreachable!("validated: one of image or build is present"),
    };

    let build = match &config.build {
        Some(build) => Some(resolve_build(build, context.dir, &path)?),
        None => None,
    };

    let env = resolve_env(config.env.as_ref(), &path, &context.env.values)?;

    let mut mounts = Vec::new();
    if let Some(specs) = &config.volumes {
        for (index, spec) in specs.iter().enumerate() {
            let mount_path = format!("{path}.volumes[{index}]");
            let mount = resolve_mount(
                spec,
                context.dir,
                context.declared_volumes,
                context.project,
                &mount_path,
            )?;
            if let MountSource::Volume {
                name: resolved,
                declared,
            } = &mount.source
            {
                if let Some((previous, service)) = context.volume_users.get(resolved) {
                    return Err(one(
                        mount_path,
                        format!(
                            "`{declared}` is already mounted by `{service}` (`{previous}`); \
                             this runtime cannot attach one volume to two running containers, \
                             even read-only"
                        ),
                    ));
                }
                context
                    .volume_users
                    .insert(resolved.clone(), (declared.clone(), name.to_owned()));
            }
            mounts.push(mount);
        }
    }

    let mut ports = Vec::new();
    if let Some(specs) = &config.ports {
        for (index, spec) in specs.iter().enumerate() {
            let port_path = format!("{path}.ports[{index}]");
            let port = PortSpec::parse(spec).map_err(|message| one(&port_path, message))?;
            if let Some(binding) = port.host_binding() {
                if let Some((other_service, other_spec)) = context.bindings.get(&binding) {
                    return Err(one(
                        port_path,
                        format!(
                            "host port {}:{} is already published by `{other_service}` as `{other_spec}`",
                            binding.0, binding.1
                        ),
                    ));
                }
                context
                    .bindings
                    .insert(binding, (name.to_owned(), port.source.clone()));
            }
            ports.push(port);
        }
    }

    let tmpfs = config.tmpfs.clone().unwrap_or_default();
    for (index, entry) in tmpfs.iter().enumerate() {
        if !entry.starts_with('/') {
            return Err(one(
                format!("{path}.tmpfs[{index}]"),
                "a tmpfs target must be an absolute path, such as `/tmp`",
            ));
        }
    }

    let cpus = config.cpus.map(|cpus| cpus.0);
    if let Some(value) = cpus {
        if !value.is_finite() || value <= 0.0 {
            return Err(one(
                format!("{path}.cpus"),
                "`cpus` must be greater than zero",
            ));
        }
    }

    let memory = match &config.memory {
        Some(value) => Some(parse_size(value, &format!("{path}.memory"))?),
        None => None,
    };
    let shm_size = match &config.shm_size {
        Some(value) => Some(parse_size(value, &format!("{path}.shm_size"))?),
        None => None,
    };

    let healthcheck = match &config.healthcheck {
        Some(healthcheck) => Some(resolve_healthcheck(healthcheck, &path)?),
        None => None,
    };

    let depends_on = config
        .depends_on
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|dependency| ResolvedDependency {
            service: dependency.service,
            condition: dependency.condition,
        })
        .collect();

    Ok(ResolvedService {
        name: name.to_owned(),
        image,
        build,
        pull: config.pull.unwrap_or(PullPolicy::Missing),
        command: config.command.clone(),
        entrypoint: config.entrypoint.clone(),
        env,
        user: config.user.clone(),
        workdir: config.workdir.clone(),
        init: config.init.unwrap_or(false),
        shm_size,
        cap_add: config.cap_add.clone().unwrap_or_default(),
        cap_drop: config.cap_drop.clone().unwrap_or_default(),
        ulimits: config.ulimits.clone().unwrap_or_default(),
        volumes: mounts,
        tmpfs,
        read_only: config.read_only.unwrap_or(false),
        ports,
        aliases: config.aliases.clone().unwrap_or_default(),
        depends_on,
        restart: config.restart.unwrap_or(RestartPolicy::No),
        max_retries: config.max_retries.unwrap_or(5),
        healthcheck,
        labels: config.labels.clone().unwrap_or_default(),
        cpus,
        memory,
        config_hash: String::new(),
    })
}

fn resolve_build(
    config: &BuildConfig,
    dir: &Path,
    path: &str,
) -> Result<ResolvedBuild, Vec<StackError>> {
    let context_name = config.context.as_deref().unwrap_or(".");
    let context = std::fs::canonicalize(dir.join(context_name)).map_err(|_| {
        one(
            format!("{path}.build.context"),
            format!("`{context_name}` does not exist, relative to the stack file"),
        )
    })?;
    if !context.is_dir() {
        return Err(one(
            format!("{path}.build.context"),
            format!("`{}` is not a directory", context.display()),
        ));
    }

    let containerfile_name = config
        .containerfile
        .clone()
        .unwrap_or_else(|| "Containerfile".to_owned());
    let containerfile = if Path::new(&containerfile_name).is_absolute() {
        PathBuf::from(&containerfile_name)
    } else {
        context.join(&containerfile_name)
    };
    if !containerfile.is_file() {
        return Err(one(
            format!("{path}.build.containerfile"),
            format!("`{}` does not exist", containerfile.display()),
        ));
    }

    Ok(ResolvedBuild {
        context,
        containerfile,
        target: config.target.clone(),
        args: config.args.clone(),
    })
}

fn resolve_env(
    raw: Option<&BTreeMap<String, String>>,
    path: &str,
    file: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, Vec<StackError>> {
    let mut resolved = BTreeMap::new();
    if let Some(raw) = raw {
        for (key, value) in raw {
            let value = env::substitute(value, file)
                .map_err(|message| one(format!("{path}.env.{key}"), message))?;
            resolved.insert(key.clone(), value);
        }
    }
    Ok(resolved)
}

fn resolve_mount(
    spec: &str,
    dir: &Path,
    declared_volumes: &BTreeMap<String, VolumeConfig>,
    project: &str,
    path: &str,
) -> Result<ResolvedMount, Vec<StackError>> {
    let parts: Vec<&str> = spec.split(':').collect();
    let (source, target, read_only) = match parts.as_slice() {
        [source, target] => (*source, *target, false),
        [source, target, "ro"] => (*source, *target, true),
        [_, _, other] => {
            return Err(one(
                path,
                format!("unknown mount option `{other}`; only `ro` is supported"),
            ));
        }
        _ => {
            return Err(one(path, format!("`{spec}` is not `source:target[:ro]`")));
        }
    };

    if !target.starts_with('/') {
        return Err(one(
            path,
            format!("`{target}` must be an absolute container path"),
        ));
    }
    if source.is_empty() {
        return Err(one(path, "the source cannot be empty"));
    }
    if source.starts_with('~') {
        return Err(one(
            path,
            "`~` is not expanded; write `./…` or an absolute path",
        ));
    }

    let source = if source.starts_with('/') || source.starts_with('.') {
        let candidate = if source.starts_with('/') {
            PathBuf::from(source)
        } else {
            dir.join(source)
        };
        let resolved = std::fs::canonicalize(&candidate).map_err(|_| {
            one(
                path,
                format!(
                    "bind source `{}` does not exist; a missing path is an error, \
                     not an empty directory",
                    candidate.display()
                ),
            )
        })?;
        MountSource::Bind(resolved)
    } else if declared_volumes.contains_key(source) {
        MountSource::Volume {
            name: format!("{project}-{source}"),
            declared: source.to_owned(),
        }
    } else {
        return Err(one(
            path,
            format!(
                "`{source}` is neither a declared volume nor a host path; \
                 a relative host path starts with `./`"
            ),
        ));
    };

    Ok(ResolvedMount {
        source,
        target: target.to_owned(),
        read_only,
    })
}

fn resolve_healthcheck(
    config: &HealthcheckConfig,
    path: &str,
) -> Result<ResolvedHealthcheck, Vec<StackError>> {
    let interval = parse_span(
        config.interval.as_deref().unwrap_or("30s"),
        &format!("{path}.healthcheck.interval"),
    )?;
    let timeout = parse_span(
        config.timeout.as_deref().unwrap_or("5s"),
        &format!("{path}.healthcheck.timeout"),
    )?;
    let start_period = parse_span(
        config.start_period.as_deref().unwrap_or("0s"),
        &format!("{path}.healthcheck.start_period"),
    )?;
    let retries = config.retries.unwrap_or(3);

    if interval.is_zero() {
        return Err(one(
            format!("{path}.healthcheck.interval"),
            "`interval` must be greater than zero",
        ));
    }
    if timeout.is_zero() {
        return Err(one(
            format!("{path}.healthcheck.timeout"),
            "`timeout` must be greater than zero",
        ));
    }
    if retries == 0 {
        return Err(one(
            format!("{path}.healthcheck.retries"),
            "`retries` must be at least 1",
        ));
    }

    Ok(ResolvedHealthcheck {
        exec: config.exec.clone(),
        interval,
        timeout,
        start_period,
        retries,
    })
}

fn parse_span(value: &str, path: &str) -> Result<Span, Vec<StackError>> {
    Span::parse(value).map_err(|message| one(path, message))
}

fn parse_size(value: &str, path: &str) -> Result<Size, Vec<StackError>> {
    let size = Size::parse(value).map_err(|message| one(path, message))?;
    if size.bytes() < 1 << 20 {
        return Err(one(
            path,
            "must be at least 1 MiB, which is the runtime's granularity",
        ));
    }
    Ok(size)
}

fn parse_journal(value: &str, path: &str) -> Result<Journal, Vec<StackError>> {
    let (mode, size) = match value.split_once(':') {
        Some((mode, size)) => (
            mode,
            Some(Size::parse(size).map_err(|message| one(path, message))?),
        ),
        None => (value, None),
    };
    match mode {
        "ordered" | "writeback" | "journal" => Ok(Journal {
            mode: mode.to_owned(),
            size,
        }),
        other => Err(one(
            path,
            format!("`{other}` is not a journal mode; use `ordered`, `writeback` or `journal`"),
        )),
    }
}

fn one(path: impl Into<String>, message: impl Into<String>) -> Vec<StackError> {
    vec![StackError::new(path, message)]
}
