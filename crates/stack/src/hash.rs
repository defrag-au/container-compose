//! The config hash.
//!
//! The input is the *resolved* service, rendered as JSON with object keys
//! sorted and arrays in declared order, then SHA-256 truncated to 16 hex
//! characters. It is written by hand rather than through `serde_json::Value`
//! so the shape is a decision in code and not a consequence of field order.

use std::collections::BTreeMap;
use std::fmt::Write;

use sha2::{Digest, Sha256};

use super::resolve::{
    MountSource, ResolvedHealthcheck, ResolvedNetwork, ResolvedService, ResolvedVolume,
};

#[derive(Debug, Clone)]
enum Canon {
    Bool(bool),
    UInt(u64),
    Float(f64),
    Str(String),
    Array(Vec<Canon>),
    Object(BTreeMap<String, Canon>),
}

impl Canon {
    fn render(&self, out: &mut String) {
        match self {
            Canon::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Canon::UInt(value) => {
                write!(out, "{value}").expect("writing to a String cannot fail");
            }
            Canon::Float(value) => {
                write!(out, "{value}").expect("writing to a String cannot fail");
            }
            Canon::Str(value) => render_string(value, out),
            Canon::Array(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    value.render(out);
                }
                out.push(']');
            }
            Canon::Object(values) => {
                out.push('{');
                for (index, (key, value)) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    render_string(key, out);
                    out.push(':');
                    value.render(out);
                }
                out.push('}');
            }
        }
    }

    fn digest(&self) -> String {
        let mut rendered = String::new();
        self.render(&mut rendered);
        let digest = Sha256::digest(rendered.as_bytes());
        let mut hex = String::with_capacity(64);
        for byte in digest {
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
        }
        hex.truncate(16);
        hex
    }
}

fn render_string(value: &str, out: &mut String) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control < ' ' => {
                write!(out, "\\u{:04x}", control as u32).expect("writing to a String");
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn object(entries: impl IntoIterator<Item = (&'static str, Canon)>) -> Canon {
    Canon::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn strings(values: &[String]) -> Canon {
    Canon::Array(values.iter().cloned().map(Canon::Str).collect())
}

fn string_map(values: &BTreeMap<String, String>) -> Canon {
    Canon::Object(
        values
            .iter()
            .map(|(key, value)| (key.clone(), Canon::Str(value.clone())))
            .collect(),
    )
}

fn healthcheck(value: &ResolvedHealthcheck) -> Canon {
    object([
        ("exec", strings(&value.exec)),
        ("interval_ms", Canon::UInt(value.interval.millis() as u64)),
        ("timeout_ms", Canon::UInt(value.timeout.millis() as u64)),
        (
            "start_period_ms",
            Canon::UInt(value.start_period.millis() as u64),
        ),
        ("retries", Canon::UInt(u64::from(value.retries))),
    ])
}

pub fn service_hash(service: &ResolvedService) -> String {
    let mut fields = BTreeMap::new();
    fields.insert("image".to_owned(), Canon::Str(service.image.clone()));
    fields.insert("init".to_owned(), Canon::Bool(service.init));
    fields.insert("read_only".to_owned(), Canon::Bool(service.read_only));
    fields.insert(
        "restart".to_owned(),
        Canon::Str(service.restart.as_str().to_owned()),
    );

    if let Some(build) = &service.build {
        let mut build_fields = BTreeMap::new();
        build_fields.insert(
            "context".to_owned(),
            Canon::Str(build.context.display().to_string()),
        );
        build_fields.insert(
            "containerfile".to_owned(),
            Canon::Str(build.containerfile.display().to_string()),
        );
        if let Some(target) = &build.target {
            build_fields.insert("target".to_owned(), Canon::Str(target.clone()));
        }
        if !build.args.is_empty() {
            build_fields.insert("args".to_owned(), string_map(&build.args));
        }
        fields.insert("build".to_owned(), Canon::Object(build_fields));
    }

    if let Some(command) = &service.command {
        fields.insert("command".to_owned(), strings(command));
    }
    if let Some(entrypoint) = &service.entrypoint {
        fields.insert("entrypoint".to_owned(), Canon::Str(entrypoint.clone()));
    }
    if !service.env.is_empty() {
        fields.insert("env".to_owned(), string_map(&service.env));
    }
    if let Some(user) = &service.user {
        fields.insert("user".to_owned(), Canon::Str(user.clone()));
    }
    if let Some(workdir) = &service.workdir {
        fields.insert("workdir".to_owned(), Canon::Str(workdir.clone()));
    }
    if let Some(size) = service.shm_size {
        fields.insert("shm_size".to_owned(), Canon::UInt(size.bytes()));
    }
    if let Some(size) = service.memory {
        fields.insert("memory".to_owned(), Canon::UInt(size.bytes()));
    }
    if let Some(cpus) = service.cpus {
        fields.insert("cpus".to_owned(), Canon::Float(cpus));
    }
    if !service.cap_add.is_empty() {
        fields.insert("cap_add".to_owned(), strings(&service.cap_add));
    }
    if !service.cap_drop.is_empty() {
        fields.insert("cap_drop".to_owned(), strings(&service.cap_drop));
    }
    if !service.ulimits.is_empty() {
        fields.insert("ulimits".to_owned(), strings(&service.ulimits));
    }
    if !service.tmpfs.is_empty() {
        fields.insert("tmpfs".to_owned(), strings(&service.tmpfs));
    }
    if !service.aliases.is_empty() {
        fields.insert("aliases".to_owned(), strings(&service.aliases));
    }
    if !service.ports.is_empty() {
        fields.insert(
            "ports".to_owned(),
            Canon::Array(
                service
                    .ports
                    .iter()
                    .map(|port| {
                        let mut fields = BTreeMap::new();
                        if let Some(address) = port.host_ip {
                            fields.insert("host_ip".to_owned(), Canon::Str(address.to_string()));
                        }
                        if let Some(value) = port.host_port {
                            fields.insert("host_port".to_owned(), Canon::UInt(u64::from(value)));
                        }
                        fields.insert(
                            "container_port".to_owned(),
                            Canon::UInt(u64::from(port.container_port)),
                        );
                        fields.insert(
                            "protocol".to_owned(),
                            Canon::Str(port.protocol.as_str().to_owned()),
                        );
                        Canon::Object(fields)
                    })
                    .collect(),
            ),
        );
    }
    if !service.volumes.is_empty() {
        fields.insert(
            "volumes".to_owned(),
            Canon::Array(
                service
                    .volumes
                    .iter()
                    .map(|mount| {
                        let mut fields = BTreeMap::new();
                        match &mount.source {
                            MountSource::Volume { name, .. } => {
                                fields.insert("volume".to_owned(), Canon::Str(name.clone()));
                            }
                            MountSource::Bind(path) => {
                                fields.insert(
                                    "bind".to_owned(),
                                    Canon::Str(path.display().to_string()),
                                );
                            }
                        }
                        fields.insert("target".to_owned(), Canon::Str(mount.target.clone()));
                        fields.insert("read_only".to_owned(), Canon::Bool(mount.read_only));
                        Canon::Object(fields)
                    })
                    .collect(),
            ),
        );
    }
    if let Some(healthcheck) = &service.healthcheck {
        fields.insert("healthcheck".to_owned(), healthcheck_value(healthcheck));
    }
    if !service.labels.is_empty() {
        fields.insert("labels".to_owned(), string_map(&service.labels));
    }

    Canon::Object(fields).digest()
}

fn healthcheck_value(value: &ResolvedHealthcheck) -> Canon {
    healthcheck(value)
}

/// The header hash: the service hashes plus the volume and network definitions.
/// `depends_on` and the project name are excluded, as for a service hash.
pub fn stack_hash(
    network: &ResolvedNetwork,
    volumes: &BTreeMap<String, ResolvedVolume>,
    services: &BTreeMap<String, ResolvedService>,
) -> String {
    let mut network_fields = BTreeMap::new();
    network_fields.insert(
        "mode".to_owned(),
        Canon::Str(network.mode.as_str().to_owned()),
    );
    if let Some(subnet) = &network.subnet {
        network_fields.insert("subnet".to_owned(), Canon::Str(subnet.clone()));
    }

    let volumes_value = Canon::Object(
        volumes
            .iter()
            .map(|(declared, volume)| {
                let mut fields = BTreeMap::new();
                if let Some(size) = volume.size {
                    fields.insert("size".to_owned(), Canon::UInt(size.bytes()));
                }
                if let Some(journal) = &volume.journal {
                    let mut journal_fields = BTreeMap::new();
                    journal_fields.insert("mode".to_owned(), Canon::Str(journal.mode.clone()));
                    if let Some(size) = journal.size {
                        journal_fields.insert("size".to_owned(), Canon::UInt(size.bytes()));
                    }
                    fields.insert("journal".to_owned(), Canon::Object(journal_fields));
                }
                (declared.clone(), Canon::Object(fields))
            })
            .collect(),
    );

    let services_value = Canon::Object(
        services
            .iter()
            .map(|(name, service)| (name.clone(), Canon::Str(service.config_hash.clone())))
            .collect(),
    );

    object([
        ("network", Canon::Object(network_fields)),
        ("volumes", volumes_value),
        ("services", services_value),
    ])
    .digest()
}

#[cfg(test)]
mod tests {
    use super::Canon;

    #[test]
    fn object_keys_are_sorted_and_arrays_keep_their_order() {
        let value = Canon::Object(
            [
                ("b".to_owned(), Canon::UInt(2)),
                (
                    "a".to_owned(),
                    Canon::Array(vec![Canon::UInt(2), Canon::UInt(1)]),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let mut rendered = String::new();
        value.render(&mut rendered);
        assert_eq!(rendered, r#"{"a":[2,1],"b":2}"#);
    }

    #[test]
    fn the_digest_is_sixteen_hex_characters() {
        let digest = Canon::Str("x".to_owned()).digest();
        assert_eq!(digest.len(), 16);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
