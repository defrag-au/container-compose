//! The argv for each `container` invocation.
//!
//! These are pure functions of the resolved stack. `plan --commands` prints
//! exactly what these return, and `up` executes exactly the same vectors, so a
//! difference between the two is a bug rather than a formatting choice.

use container_compose_stack::resolve::{
    MountSource, ResolvedBuild, ResolvedMount, ResolvedService, ResolvedStack, ResolvedVolume,
};

use crate::labels;

pub fn container_name(project: &str, service: &str) -> String {
    format!("{project}-{service}")
}

pub fn create_container(stack: &ResolvedStack, service: &ResolvedService) -> Vec<String> {
    let mut args = vec!["create".to_owned()];
    args.extend(container_flags(
        stack,
        service,
        &container_name(&stack.name, &service.name),
        false,
    ));
    args.push(service.image.clone());
    if let Some(command) = &service.command {
        args.extend(command.iter().cloned());
    }
    args
}

/// A one-off container for `run`: the service's configuration, a distinct
/// name, attached to the terminal, and `--rm` unless the caller wants to keep
/// it. `command` replaces the service's command when given.
pub fn run_container(
    stack: &ResolvedStack,
    service: &ResolvedService,
    command: Option<&[String]>,
    interactive: bool,
    tty: bool,
    remove: bool,
) -> Vec<String> {
    let mut args = vec!["run".to_owned()];
    if interactive {
        args.push("-i".to_owned());
    }
    if tty {
        args.push("-t".to_owned());
    }
    if remove {
        args.push("--rm".to_owned());
    }
    args.extend(container_flags(
        stack,
        service,
        &run_name(&stack.name, &service.name),
        true,
    ));
    args.push(service.image.clone());
    let command = command.or(service.command.as_deref());
    if let Some(command) = command {
        args.extend(command.iter().cloned());
    }
    args
}

/// Everything between the subcommand and the image: name, labels, network,
/// env, mounts, ports, resources and process options.
fn container_flags(
    stack: &ResolvedStack,
    service: &ResolvedService,
    name: &str,
    run: bool,
) -> Vec<String> {
    let mut args = vec!["--name".to_owned(), name.to_owned()];

    let mut labels = vec![
        (labels::PROJECT, stack.name.as_str()),
        (labels::SERVICE, service.name.as_str()),
        (labels::CONFIG, service.config_hash.as_str()),
        (labels::SCHEMA, labels::SCHEMA_VERSION),
    ];
    if run {
        labels.push((labels::RUN, "1"));
    }
    for (key, value) in labels {
        args.push("--label".to_owned());
        args.push(format!("{key}={value}"));
    }

    let mut network = stack.network.name.clone();
    for alias in &service.aliases {
        network.push_str(",alias=");
        network.push_str(alias);
    }
    args.push("--network".to_owned());
    args.push(network);

    for key in service.env.keys() {
        args.push("--env".to_owned());
        args.push(key.clone());
    }
    for mount in &service.volumes {
        args.push("--volume".to_owned());
        args.push(render_mount(mount));
    }
    for port in &service.ports {
        args.push("--publish".to_owned());
        args.push(port.source.clone());
    }
    if let Some(cpus) = service.cpus {
        args.push("--cpus".to_owned());
        args.push(render_cpus(cpus));
    }
    if let Some(memory) = service.memory {
        args.push("--memory".to_owned());
        args.push(memory.to_string());
    }
    for ulimit in &service.ulimits {
        args.push("--ulimit".to_owned());
        args.push(ulimit.clone());
    }
    if service.init {
        args.push("--init".to_owned());
    }
    if let Some(size) = service.shm_size {
        args.push("--shm-size".to_owned());
        args.push(size.to_string());
    }
    for tmpfs in &service.tmpfs {
        args.push("--tmpfs".to_owned());
        args.push(tmpfs.clone());
    }
    if service.read_only {
        args.push("--read-only".to_owned());
    }
    for capability in &service.cap_add {
        args.push("--cap-add".to_owned());
        args.push(capability.clone());
    }
    for capability in &service.cap_drop {
        args.push("--cap-drop".to_owned());
        args.push(capability.clone());
    }
    if let Some(user) = &service.user {
        args.push("--user".to_owned());
        args.push(user.clone());
    }
    if let Some(workdir) = &service.workdir {
        args.push("--workdir".to_owned());
        args.push(workdir.clone());
    }
    if let Some(entrypoint) = &service.entrypoint {
        args.push("--entrypoint".to_owned());
        args.push(entrypoint.clone());
    }

    args
}

pub fn run_name(project: &str, service: &str) -> String {
    format!("{project}-{service}-run")
}

pub fn start_container(name: &str) -> Vec<String> {
    vec!["start".to_owned(), name.to_owned()]
}

pub fn stop_container(name: &str, grace_secs: u64) -> Vec<String> {
    vec![
        "stop".to_owned(),
        "--time".to_owned(),
        grace_secs.to_string(),
        name.to_owned(),
    ]
}

pub fn delete_container(name: &str, force: bool) -> Vec<String> {
    let mut args = vec!["delete".to_owned()];
    if force {
        args.push("--force".to_owned());
    }
    args.push(name.to_owned());
    args
}

pub fn kill_container(name: &str) -> Vec<String> {
    vec!["kill".to_owned(), name.to_owned()]
}

pub fn create_volume(stack: &ResolvedStack, volume: &ResolvedVolume) -> Vec<String> {
    let mut args = vec!["volume".to_owned(), "create".to_owned()];
    for (key, value) in [
        (labels::PROJECT, stack.name.as_str()),
        (labels::SCHEMA, labels::SCHEMA_VERSION),
    ] {
        args.push("--label".to_owned());
        args.push(format!("{key}={value}"));
    }
    if let Some(size) = volume.size {
        args.push("--opt".to_owned());
        args.push(format!("size={size}"));
    }
    if let Some(journal) = &volume.journal {
        let mut value = journal.mode.clone();
        if let Some(size) = journal.size {
            value.push(':');
            value.push_str(&size.to_string());
        }
        args.push("--opt".to_owned());
        args.push(format!("journal={value}"));
    }
    args.push(volume.name.clone());
    args
}

pub fn delete_volume(name: &str) -> Vec<String> {
    vec!["volume".to_owned(), "delete".to_owned(), name.to_owned()]
}

pub fn create_network(stack: &ResolvedStack) -> Vec<String> {
    let mut args = vec!["network".to_owned(), "create".to_owned()];
    if let Some(subnet) = &stack.network.subnet {
        args.push("--subnet".to_owned());
        args.push(subnet.clone());
    }
    for (key, value) in [
        (labels::PROJECT, stack.name.as_str()),
        (labels::SCHEMA, labels::SCHEMA_VERSION),
    ] {
        args.push("--label".to_owned());
        args.push(format!("{key}={value}"));
    }
    args.push(stack.network.name.clone());
    args
}

pub fn delete_network(name: &str) -> Vec<String> {
    vec!["network".to_owned(), "delete".to_owned(), name.to_owned()]
}

pub fn pull_image(reference: &str) -> Vec<String> {
    vec!["image".to_owned(), "pull".to_owned(), reference.to_owned()]
}

pub fn build_image(image: &str, build: &ResolvedBuild, no_cache: bool) -> Vec<String> {
    let mut args = vec![
        "build".to_owned(),
        "-t".to_owned(),
        image.to_owned(),
        "-f".to_owned(),
        build.containerfile.display().to_string(),
    ];
    if no_cache {
        args.push("--no-cache".to_owned());
    }
    if let Some(target) = &build.target {
        args.push("--target".to_owned());
        args.push(target.clone());
    }
    for (key, value) in &build.args {
        args.push("--build-arg".to_owned());
        args.push(format!("{key}={value}"));
    }
    args.push(build.context.display().to_string());
    args
}

pub fn logs(name: &str, follow: bool, lines: Option<u32>) -> Vec<String> {
    let mut args = vec!["logs".to_owned()];
    if follow {
        args.push("--follow".to_owned());
    }
    if let Some(lines) = lines {
        args.push("-n".to_owned());
        args.push(lines.to_string());
    }
    args.push(name.to_owned());
    args
}

pub fn exec(name: &str, command: &[String]) -> Vec<String> {
    let mut args = vec!["exec".to_owned(), name.to_owned()];
    args.extend(command.iter().cloned());
    args
}

/// `exec` with the terminal flags the CLI chose.
pub fn exec_attached(name: &str, command: &[String], interactive: bool, tty: bool) -> Vec<String> {
    let mut args = vec!["exec".to_owned()];
    if interactive {
        args.push("-i".to_owned());
    }
    if tty {
        args.push("-t".to_owned());
    }
    args.push(name.to_owned());
    args.extend(command.iter().cloned());
    args
}

pub fn system_status() -> Vec<String> {
    vec![
        "system".to_owned(),
        "status".to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
    ]
}

pub fn system_version() -> Vec<String> {
    vec![
        "system".to_owned(),
        "version".to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
    ]
}

fn render_mount(mount: &ResolvedMount) -> String {
    let source = match &mount.source {
        MountSource::Volume { name, .. } => name.clone(),
        MountSource::Bind(path) => path.display().to_string(),
    };
    let mut spec = format!("{source}:{}", mount.target);
    if mount.read_only {
        spec.push_str(":ro");
    }
    spec
}

fn render_cpus(cpus: f64) -> String {
    if cpus.fract() == 0.0 {
        format!("{}", cpus as u64)
    } else {
        format!("{cpus}")
    }
}

#[cfg(test)]
mod tests {
    use super::{container_name, render_cpus, start_container};

    #[test]
    fn names_are_project_dash_service() {
        assert_eq!(container_name("mitos", "sieve"), "mitos-sieve");
    }

    #[test]
    fn whole_cpu_counts_have_no_decimal() {
        assert_eq!(render_cpus(4.0), "4");
        assert_eq!(render_cpus(1.5), "1.5");
    }

    #[test]
    fn start_takes_one_name() {
        assert_eq!(start_container("mitos-sieve"), vec!["start", "mitos-sieve"]);
    }
}
