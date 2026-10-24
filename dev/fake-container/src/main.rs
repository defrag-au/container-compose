//! A test double for `container`.
//!
//! It reads and writes one directory, `FAKE_CONTAINER_DIR`:
//!
//! | file | contents |
//! |---|---|
//! | `world.json` | the simulated resources, in the runtime's own shapes |
//! | `calls.jsonl` | one record per invocation: program, argv, cwd, seq |
//! | `fail.json` | optional injection: `{subcommand, match, exit, stderr}` |
//!
//! The rules that keep it honest:
//!
//! - **Unknown flags fail loudly** (exit 64). Every subcommand parses against
//!   an allowlist, so an adapter that invents a flag fails here rather than on a
//!   user's machine.
//! - **It never pretends to do work.** No images, no processes, no networking.
//!   `start` is asynchronous in the same way the real one is: it returns with
//!   the state still `stopped` and flips on a later `list`.
//! - It never ships in a release build.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};

use container_compose_runtime::{
    ContainerConfiguration, ContainerImage, ContainerStatus, ImageConfiguration,
    NetworkConfiguration, NetworkStatus, Observed, ObservedContainer, ObservedImage,
    ObservedNetwork, ObservedVolume, VolumeConfiguration,
};

#[derive(Debug, Default, Serialize, Deserialize)]
struct World {
    #[serde(flatten)]
    observed: Observed,
    /// Containers whose `start` has returned but which are not `running` yet.
    #[serde(default)]
    pending_start: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Call<'a> {
    program: &'a str,
    argv: &'a [String],
    cwd: String,
    seq: usize,
    /// The values `create` resolved for its `--env`/`-e` flags. The argv carries
    /// the key only; this is what proves the value reached the child process
    /// without being printed anywhere.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct FailureInjection {
    subcommand: String,
    #[serde(default, rename = "match")]
    needle: Option<String>,
    exit: i32,
    #[serde(default)]
    stderr: String,
}

#[derive(Debug)]
struct Failure {
    code: u8,
    message: String,
}

impl Failure {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 64,
            message: message.into(),
        }
    }

    fn domain(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }
}

#[derive(Debug, Default)]
struct Parsed {
    values: BTreeMap<String, Vec<String>>,
    flags: BTreeSet<String>,
    positionals: Vec<String>,
}

impl Parsed {
    fn last(&self, flag: &str) -> Option<&str> {
        self.values
            .get(flag)
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    fn all(&self, flags: &[&str]) -> Vec<&str> {
        flags
            .iter()
            .flat_map(|flag| self.values.get(*flag).into_iter().flatten())
            .map(String::as_str)
            .collect()
    }

    fn has(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }
}

fn main() -> ExitCode {
    let dir = match std::env::var("FAKE_CONTAINER_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            eprintln!("Error: FAKE_CONTAINER_DIR is not set");
            return ExitCode::from(64);
        }
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    record(&dir, &args);

    if let Some(injection) = injection(&dir, &args) {
        if !injection.stderr.is_empty() {
            eprintln!("{}", injection.stderr);
        }
        return ExitCode::from(u8::try_from(injection.exit).unwrap_or(1));
    }

    let mut world = load_world(&dir);
    let changed = mutates(&args);
    match dispatch(&args, &mut world) {
        Ok(()) => {
            if changed {
                if let Err(error) = save_world(&dir, &world) {
                    eprintln!("Error: cannot write world.json: {error}");
                    return ExitCode::from(1);
                }
            }
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("Error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

fn dispatch(args: &[String], world: &mut World) -> Result<(), Failure> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(Failure::usage("no command"));
    };
    let rest = &args[1..];

    match command {
        "list" | "ls" => list(rest, world),
        "create" => create(rest, world),
        "start" => start(rest, world),
        "stop" => stop(rest, world),
        "delete" | "rm" => delete(rest, world),
        "inspect" => inspect(rest, world),
        "logs" => logs(rest, world),
        "exec" => exec(rest, world),
        "volume" => match rest.first().map(String::as_str) {
            Some("create") => volume_create(&rest[1..], world),
            Some("delete" | "rm") => volume_delete(&rest[1..], world),
            Some("list" | "ls") => volume_list(&rest[1..], world),
            _ => Err(Failure::usage("unknown `volume` subcommand")),
        },
        "network" => match rest.first().map(String::as_str) {
            Some("create") => network_create(&rest[1..], world),
            Some("delete" | "rm") => network_delete(&rest[1..], world),
            Some("list" | "ls") => network_list(&rest[1..], world),
            _ => Err(Failure::usage("unknown `network` subcommand")),
        },
        "image" => match rest.first().map(String::as_str) {
            Some("pull") => image_pull(&rest[1..], world),
            Some("list" | "ls") => image_list(&rest[1..], world),
            _ => Err(Failure::usage("unknown `image` subcommand")),
        },
        "system" => match rest.first().map(String::as_str) {
            Some("status") => system_status(&rest[1..]),
            Some("version") => system_version(&rest[1..]),
            _ => Err(Failure::usage("unknown `system` subcommand")),
        },
        other => Err(Failure::usage(format!("unknown command `{other}`"))),
    }
}

fn mutates(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("create" | "start" | "stop" | "delete" | "rm" | "list" | "ls") => true,
        Some("volume" | "network") => matches!(
            args.get(1).map(String::as_str),
            Some("create" | "delete" | "rm")
        ),
        Some("image") => matches!(args.get(1).map(String::as_str), Some("pull")),
        _ => false,
    }
}

fn list(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(
        args,
        &["--format"],
        &["--all", "-a", "--quiet", "-q", "--debug"],
    )?;
    if !parsed.positionals.is_empty() {
        return Err(Failure::usage("list takes no arguments"));
    }
    let all = parsed.has("--all") || parsed.has("-a");
    let output: Vec<&ObservedContainer> = world
        .observed
        .containers
        .iter()
        .filter(|container| all || container.is_running())
        .collect();
    println!("{}", serde_json::to_string_pretty(&output).expect("json"));

    // The deferred half of `start`: this response shows the old state, and the
    // next one shows `running`, which is what the real runtime does.
    let pending = std::mem::take(&mut world.pending_start);
    for name in pending {
        if let Some(container) = world
            .observed
            .containers
            .iter_mut()
            .find(|container| container.id == name)
        {
            container.status.state = "running".to_owned();
        }
    }
    Ok(())
}

fn create(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(
        args,
        &[
            "--name",
            "--label",
            "-l",
            "--network",
            "--env",
            "-e",
            "--volume",
            "-v",
            "--publish",
            "-p",
            "--cpus",
            "-c",
            "--memory",
            "-m",
            "--ulimit",
            "--tmpfs",
            "--shm-size",
            "--cap-add",
            "--cap-drop",
            "--user",
            "-u",
            "--workdir",
            "-w",
            "--entrypoint",
            "--debug",
        ],
        &["--init", "--read-only"],
    )?;

    let name = parsed
        .last("--name")
        .ok_or_else(|| Failure::usage("`--name` is required"))?;
    let image = parsed
        .positionals
        .first()
        .ok_or_else(|| Failure::usage("an image is required"))?;
    if world
        .observed
        .containers
        .iter()
        .any(|container| container.id == name)
    {
        return Err(Failure::domain(format!("container already exists: {name}")));
    }

    let mut labels = BTreeMap::new();
    for pair in parsed.all(&["--label", "-l"]) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| Failure::usage(format!("`{pair}` is not key=value")))?;
        labels.insert(key.to_owned(), value.to_owned());
    }

    world.observed.containers.push(ObservedContainer {
        id: name.to_owned(),
        configuration: ContainerConfiguration {
            labels,
            image: ContainerImage {
                reference: image.clone(),
            },
        },
        status: ContainerStatus {
            state: "stopped".to_owned(),
            started_date: None,
        },
    });
    println!("{name}");
    Ok(())
}

fn start(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(
        args,
        &["--debug"],
        &["--attach", "-a", "--interactive", "-i"],
    )?;
    let [name] = parsed.positionals.as_slice() else {
        return Err(Failure::usage(
            "usage: container start [--attach] [--interactive] <container-id>",
        ));
    };
    let container = world
        .observed
        .containers
        .iter_mut()
        .find(|container| container.id == *name)
        .ok_or_else(|| Failure::domain(format!("container not found: {name}")))?;
    if !container.is_running() {
        world.pending_start.push(name.clone());
    }
    println!("{name}");
    Ok(())
}

fn stop(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(
        args,
        &["--time", "-t", "--signal", "-s", "--debug"],
        &["--all", "-a"],
    )?;
    let name = parsed
        .positionals
        .first()
        .ok_or_else(|| Failure::usage("a container id is required"))?;
    let container = world
        .observed
        .containers
        .iter_mut()
        .find(|container| container.id == *name)
        .ok_or_else(|| Failure::domain(format!("container not found: {name}")))?;
    container.status.state = "stopped".to_owned();
    world.pending_start.retain(|pending| pending != name);
    println!("{name}");
    Ok(())
}

fn delete(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(args, &["--debug"], &["--force", "-f", "--all", "-a"])?;
    let force = parsed.has("--force") || parsed.has("-f");
    let name = parsed
        .positionals
        .first()
        .ok_or_else(|| Failure::usage("a container id is required"))?;
    let index = world
        .observed
        .containers
        .iter()
        .position(|container| container.id == *name)
        .ok_or_else(|| Failure::domain(format!("container not found: {name}")))?;
    if world.observed.containers[index].is_running() && !force {
        return Err(Failure::domain(format!(
            "container {name} is running and can not be deleted"
        )));
    }
    world.observed.containers.remove(index);
    world.pending_start.retain(|pending| pending != name);
    println!("{name}");
    Ok(())
}

fn inspect(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(args, &["--debug"], &[])?;
    let names = &parsed.positionals;
    if names.is_empty() {
        return Err(Failure::usage("a container id is required"));
    }
    let mut found = Vec::new();
    for name in names {
        let container = world
            .observed
            .containers
            .iter()
            .find(|container| container.id == *name)
            .ok_or_else(|| Failure::domain(format!("container not found: {name}")))?;
        found.push(container);
    }
    println!("{}", serde_json::to_string_pretty(&found).expect("json"));
    Ok(())
}

fn logs(args: &[String], _world: &mut World) -> Result<(), Failure> {
    let parsed = parse(args, &["-n", "--debug"], &["--boot", "--follow", "-f"])?;
    if parsed.positionals.len() != 1 {
        return Err(Failure::usage("a container id is required"));
    }
    Ok(())
}

fn exec(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(
        args,
        &[
            "--env",
            "-e",
            "--env-file",
            "--gid",
            "-u",
            "--user",
            "--uid",
            "--workdir",
            "-w",
            "--debug",
        ],
        &["--detach", "-d", "--interactive", "-i", "--tty", "-t"],
    )?;
    let Some(name) = parsed.positionals.first() else {
        return Err(Failure::usage("a container id is required"));
    };
    let container = world
        .observed
        .containers
        .iter()
        .find(|container| container.id == *name)
        .ok_or_else(|| Failure::domain(format!("container not found: {name}")))?;
    if !container.is_running() {
        return Err(Failure::domain(format!("container {name} is not running")));
    }
    Ok(())
}

fn volume_create(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(args, &["--label", "-l", "--opt", "-s", "--debug"], &[])?;
    let name = parsed
        .positionals
        .first()
        .ok_or_else(|| Failure::usage("a volume name is required"))?;
    if world
        .observed
        .volumes
        .iter()
        .any(|volume| volume.id == *name)
    {
        return Err(Failure::domain(format!("volume already exists: {name}")));
    }

    let mut labels = BTreeMap::new();
    for pair in parsed.all(&["--label", "-l"]) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| Failure::usage(format!("`{pair}` is not key=value")))?;
        labels.insert(key.to_owned(), value.to_owned());
    }

    let mut options = BTreeMap::new();
    let mut size_bytes = 0;
    for option in parsed.all(&["--opt"]) {
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| Failure::usage(format!("`{option}` is not key=value")))?;
        if key == "size" {
            size_bytes = parse_size(value)
                .ok_or_else(|| Failure::usage(format!("`{value}` is not a size")))?;
        }
        options.insert(key.to_owned(), value.to_owned());
    }
    if let Some(size) = parsed.last("-s") {
        size_bytes =
            parse_size(size).ok_or_else(|| Failure::usage(format!("`{size}` is not a size")))?;
    }

    world.observed.volumes.push(ObservedVolume {
        id: name.clone(),
        configuration: VolumeConfiguration {
            labels,
            options,
            size_in_bytes: size_bytes,
        },
    });
    println!("{name}");
    Ok(())
}

fn volume_delete(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(args, &["--debug"], &["--all", "-a"])?;
    let name = parsed
        .positionals
        .first()
        .ok_or_else(|| Failure::usage("a volume name is required"))?;
    let index = world
        .observed
        .volumes
        .iter()
        .position(|volume| volume.id == *name)
        .ok_or_else(|| Failure::domain(format!("volume not found: {name}")))?;
    world.observed.volumes.remove(index);
    println!("{name}");
    Ok(())
}

fn volume_list(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(args, &["--format"], &["--quiet", "-q", "--debug"])?;
    if !parsed.positionals.is_empty() {
        return Err(Failure::usage("volume list takes no arguments"));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&world.observed.volumes).expect("json")
    );
    Ok(())
}

fn network_create(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(
        args,
        &[
            "--subnet",
            "--subnet-v6",
            "--label",
            "-l",
            "--option",
            "--plugin",
            "--debug",
        ],
        &["--internal"],
    )?;
    let name = parsed
        .positionals
        .first()
        .ok_or_else(|| Failure::usage("a network name is required"))?;
    if world
        .observed
        .networks
        .iter()
        .any(|network| network.id == *name)
    {
        return Err(Failure::domain(format!("network already exists: {name}")));
    }

    let mut labels = BTreeMap::new();
    for pair in parsed.all(&["--label", "-l"]) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| Failure::usage(format!("`{pair}` is not key=value")))?;
        labels.insert(key.to_owned(), value.to_owned());
    }
    let subnet = parsed.last("--subnet").map(str::to_owned);

    world.observed.networks.push(ObservedNetwork {
        id: name.clone(),
        configuration: NetworkConfiguration {
            labels,
            ipv4_subnet: subnet.clone(),
        },
        status: Some(NetworkStatus {
            ipv4_subnet: subnet,
        }),
    });
    println!("{name}");
    Ok(())
}

fn network_delete(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(args, &["--debug"], &["--all", "-a"])?;
    let name = parsed
        .positionals
        .first()
        .ok_or_else(|| Failure::usage("a network name is required"))?;
    let index = world
        .observed
        .networks
        .iter()
        .position(|network| network.id == *name)
        .ok_or_else(|| Failure::domain(format!("network not found: {name}")))?;
    world.observed.networks.remove(index);
    println!("{name}");
    Ok(())
}

fn network_list(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(args, &["--format"], &["--quiet", "-q", "--debug"])?;
    if !parsed.positionals.is_empty() {
        return Err(Failure::usage("network list takes no arguments"));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&world.observed.networks).expect("json")
    );
    Ok(())
}

fn image_pull(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(
        args,
        &[
            "--scheme",
            "--progress",
            "--max-concurrent-downloads",
            "--arch",
            "--os",
            "--platform",
            "--debug",
        ],
        &[],
    )?;
    let reference = parsed
        .positionals
        .first()
        .ok_or_else(|| Failure::usage("an image reference is required"))?;
    if world
        .observed
        .images
        .iter()
        .all(|image| image.configuration.name != *reference)
    {
        world.observed.images.push(ObservedImage {
            id: reference.clone(),
            configuration: ImageConfiguration {
                name: reference.clone(),
            },
        });
    }
    println!("{reference}");
    Ok(())
}

fn image_list(args: &[String], world: &mut World) -> Result<(), Failure> {
    let parsed = parse(
        args,
        &["--format"],
        &["--quiet", "-q", "--verbose", "--debug"],
    )?;
    if !parsed.positionals.is_empty() {
        return Err(Failure::usage("image list takes no arguments"));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&world.observed.images).expect("json")
    );
    Ok(())
}

fn system_status(args: &[String]) -> Result<(), Failure> {
    let parsed = parse(args, &["--format"], &["--debug"])?;
    if !parsed.positionals.is_empty() {
        return Err(Failure::usage("system status takes no arguments"));
    }
    println!(r#"{{"status":"running"}}"#);
    Ok(())
}

fn system_version(args: &[String]) -> Result<(), Failure> {
    let parsed = parse(args, &["--format"], &["--debug"])?;
    if !parsed.positionals.is_empty() {
        return Err(Failure::usage("system version takes no arguments"));
    }
    println!(
        r#"[{{"appName":"container","version":"1.6.0"}},{{"appName":"container-apiserver","version":"1.6.0"}}]"#
    );
    Ok(())
}

fn parse(args: &[String], value_flags: &[&str], bool_flags: &[&str]) -> Result<Parsed, Failure> {
    let mut parsed = Parsed::default();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if argument.starts_with('-') {
            if value_flags.contains(&argument.as_str()) {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| Failure::usage(format!("`{argument}` needs a value")))?;
                parsed
                    .values
                    .entry(argument.clone())
                    .or_default()
                    .push(value.clone());
                index += 2;
            } else if bool_flags.contains(&argument.as_str()) {
                parsed.flags.insert(argument.clone());
                index += 1;
            } else {
                return Err(Failure::usage(format!("unknown flag `{argument}`")));
            }
        } else {
            parsed.positionals.push(argument.clone());
            index += 1;
        }
    }
    Ok(parsed)
}

fn parse_size(value: &str) -> Option<u64> {
    let digits = value.find(|character: char| !character.is_ascii_digit())?;
    let (number, suffix) = value.split_at(digits);
    let number: u64 = number.parse().ok()?;
    let multiplier = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        "p" | "pb" | "pib" => 1 << 50,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

fn record(dir: &Path, argv: &[String]) {
    let calls = dir.join("calls.jsonl");
    let seq = fs::read_to_string(&calls)
        .map(|text| text.lines().count() + 1)
        .unwrap_or(1);
    let program = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| std::env::args().next().unwrap_or_default());
    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let call = Call {
        program: &program,
        argv,
        cwd,
        seq,
        env: resolved_env(argv),
    };
    let line = serde_json::to_string(&call).expect("json");
    let mut existing = fs::read_to_string(&calls).unwrap_or_default();
    existing.push_str(&line);
    existing.push('\n');
    let _ = fs::write(&calls, existing);
}

fn resolved_env(argv: &[String]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if argv.first().map(String::as_str) != Some("create") {
        return env;
    }
    let mut index = 1;
    while index < argv.len() {
        let argument = &argv[index];
        if argument == "--env" || argument == "-e" {
            if let Some(spec) = argv.get(index + 1) {
                match spec.split_once('=') {
                    Some((key, value)) => {
                        env.insert(key.to_owned(), value.to_owned());
                    }
                    None => {
                        if let Ok(value) = std::env::var(spec) {
                            env.insert(spec.clone(), value);
                        }
                    }
                }
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    env
}

fn injection(dir: &Path, argv: &[String]) -> Option<FailureInjection> {
    let text = fs::read_to_string(dir.join("fail.json")).ok()?;
    let injection: FailureInjection = serde_json::from_str(&text).ok()?;
    let prefix = match argv.first().map(String::as_str) {
        Some(command @ ("volume" | "network" | "image" | "system")) => {
            format!(
                "{command} {}",
                argv.get(1).map(String::as_str).unwrap_or("")
            )
        }
        Some(command) => command.to_owned(),
        None => return None,
    };
    if prefix != injection.subcommand {
        return None;
    }
    if let Some(needle) = &injection.needle {
        if !argv.join(" ").contains(needle) {
            return None;
        }
    }
    Some(injection)
}

fn load_world(dir: &Path) -> World {
    fs::read_to_string(dir.join("world.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(default_world)
}

fn default_world() -> World {
    let mut world = World::default();
    world.observed.networks.push(ObservedNetwork {
        id: "default".to_owned(),
        configuration: NetworkConfiguration {
            labels: BTreeMap::new(),
            ipv4_subnet: Some("192.168.64.0/24".to_owned()),
        },
        status: Some(NetworkStatus {
            ipv4_subnet: Some("192.168.64.0/24".to_owned()),
        }),
    });
    world
}

fn save_world(dir: &Path, world: &World) -> std::io::Result<()> {
    let temporary = dir.join("world.json.tmp");
    let text = serde_json::to_string_pretty(world)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    fs::write(&temporary, text)?;
    fs::rename(temporary, dir.join("world.json"))
}
