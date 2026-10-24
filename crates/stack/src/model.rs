//! The raw shape of `stack.toml`.
//!
//! These types mirror the file exactly — every field optional except the ones
//! the design marks required — and carry no defaults. Defaults belong to
//! `resolve`, so the difference between "the file said 30s" and "nobody said"
//! is still visible while validating.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackFile {
    pub version: u32,
    pub name: Option<String>,
    pub network: Option<NetworkConfig>,
    #[serde(default)]
    pub volumes: BTreeMap<String, VolumeConfig>,
    pub services: BTreeMap<String, ServiceConfig>,
    pub env: Option<EnvConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvConfig {
    pub file: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub mode: Option<NetworkMode>,
    pub subnet: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    Default,
    Isolated,
}

impl NetworkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            NetworkMode::Default => "default",
            NetworkMode::Isolated => "isolated",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeConfig {
    pub size: Option<String>,
    pub journal: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub image: Option<String>,
    pub build: Option<BuildConfig>,
    pub pull: Option<PullPolicy>,
    pub command: Option<Vec<String>>,
    pub entrypoint: Option<String>,
    pub env: Option<BTreeMap<String, String>>,
    pub user: Option<String>,
    pub workdir: Option<String>,
    pub init: Option<bool>,
    pub shm_size: Option<String>,
    pub cap_add: Option<Vec<String>>,
    pub cap_drop: Option<Vec<String>>,
    pub ulimits: Option<Vec<String>>,
    pub volumes: Option<Vec<String>>,
    pub tmpfs: Option<Vec<String>>,
    pub read_only: Option<bool>,
    pub ports: Option<Vec<String>>,
    pub aliases: Option<Vec<String>>,
    pub depends_on: Option<Vec<Dependency>>,
    pub restart: Option<RestartPolicy>,
    pub max_retries: Option<u32>,
    pub healthcheck: Option<HealthcheckConfig>,
    pub labels: Option<BTreeMap<String, String>>,
    pub cpus: Option<CpuCount>,
    pub memory: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildConfig {
    pub context: Option<String>,
    pub containerfile: Option<String>,
    pub target: Option<String>,
    #[serde(default)]
    pub args: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub service: String,
    pub condition: Condition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    ServiceStarted,
    ServiceHealthy,
}

impl Condition {
    pub fn as_str(self) -> &'static str {
        match self {
            Condition::ServiceStarted => "service_started",
            Condition::ServiceHealthy => "service_healthy",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthcheckConfig {
    pub exec: Vec<String>,
    pub interval: Option<String>,
    pub timeout: Option<String>,
    pub start_period: Option<String>,
    pub retries: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    No,
    OnStop,
    Always,
}

impl RestartPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            RestartPolicy::No => "no",
            RestartPolicy::OnStop => "on-stop",
            RestartPolicy::Always => "always",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullPolicy {
    Missing,
    Always,
    Never,
}

impl PullPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            PullPolicy::Missing => "missing",
            PullPolicy::Always => "always",
            PullPolicy::Never => "never",
        }
    }
}

/// A CPU count that accepts both `4` and `1.5`.
///
/// A plain `f64` would reject the integer form in TOML; an untagged enum would
/// make the accepted shape a guess. The visitor is explicit about both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuCount(pub f64);

impl<'de> Deserialize<'de> for CpuCount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct CpuVisitor;

        impl<'de> serde::de::Visitor<'de> for CpuVisitor {
            type Value = CpuCount;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a number of CPUs, such as `4` or `1.5`")
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(CpuCount(value as f64))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(CpuCount(value as f64))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(CpuCount(value))
            }
        }

        deserializer.deserialize_any(CpuVisitor)
    }
}
