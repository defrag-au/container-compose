//! The rules in `docs/02-stack-file.md`, as code.
//!
//! Everything here is a load error: none of these is a warning. Rules that need
//! a parsed size, duration or port are checked in `resolve`, where the message
//! can still name the key it came from.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use crate::error::StackError;

use super::model::{Condition, NetworkMode, RestartPolicy, StackFile};

pub const RESERVED_LABEL_PREFIX: &str = "dev.container-compose.";

pub fn validate(stack: &StackFile) -> Vec<StackError> {
    let mut errors = Vec::new();

    if stack.version != 1 {
        errors.push(StackError::new(
            "version",
            format!(
                "version {} is not supported; this binary understands version 1",
                stack.version
            ),
        ));
    }

    if let Some(name) = &stack.name {
        if !is_slug(name) {
            errors.push(StackError::new(
                "name",
                format!("`{name}` must match [a-z0-9][a-z0-9-]{{0,30}}"),
            ));
        }
    }

    for volume in stack.volumes.keys() {
        if !is_slug(volume) {
            errors.push(StackError::new(
                format!("[volumes.{volume}]"),
                format!("`{volume}` must match [a-z0-9][a-z0-9-]{{0,30}}"),
            ));
        }
    }

    for (name, service) in &stack.services {
        let path = format!("[services.{name}]");

        if !is_slug(name) {
            errors.push(StackError::new(
                path.clone(),
                format!("`{name}` must match [a-z0-9][a-z0-9-]{{0,30}}"),
            ));
        }

        match (&service.image, &service.build) {
            (None, None) => errors.push(StackError::new(
                path.clone(),
                "one of `image` or `build` is required",
            )),
            (Some(_), Some(_)) => errors.push(StackError::new(
                path.clone(),
                "`image` and `build` are mutually exclusive",
            )),
            _ => {}
        }

        if service.pull.is_some() && service.image.is_none() {
            errors.push(StackError::new(
                format!("{path}.pull"),
                "`pull` only applies to a service with an `image`",
            ));
        }

        if service.max_retries.is_some() && service.restart != Some(RestartPolicy::OnStop) {
            errors.push(StackError::new(
                format!("{path}.max_retries"),
                "`max_retries` only applies to `restart = \"on-stop\"`",
            ));
        }

        if let Some(healthcheck) = &service.healthcheck {
            if healthcheck.exec.is_empty() {
                errors.push(StackError::new(
                    format!("{path}.healthcheck.exec"),
                    "at least one argument is required; the probe is an argv, not a shell string",
                ));
            }
        }

        if let Some(env) = &service.env {
            for key in env.keys() {
                if !is_env_key(key) {
                    errors.push(StackError::new(
                        format!("{path}.env"),
                        format!(
                            "`{key}` is not a valid environment variable name; \
                             use [A-Za-z_][A-Za-z0-9_]*"
                        ),
                    ));
                }
            }
        }

        if let Some(labels) = &service.labels {
            for key in labels.keys() {
                if key.starts_with(RESERVED_LABEL_PREFIX) {
                    errors.push(StackError::new(
                        format!("{path}.labels"),
                        format!("`{key}` uses the reserved prefix `{RESERVED_LABEL_PREFIX}`"),
                    ));
                }
            }
        }

        if let Some(aliases) = &service.aliases {
            if aliases.len() > 32 {
                errors.push(StackError::new(
                    format!("{path}.aliases"),
                    "at most 32 aliases are allowed per network attachment",
                ));
            }
            for alias in aliases {
                if alias.is_empty() {
                    errors.push(StackError::new(
                        format!("{path}.aliases"),
                        "an alias cannot be empty",
                    ));
                } else if alias.contains('*') {
                    errors.push(StackError::new(
                        format!("{path}.aliases"),
                        format!("`{alias}`: `*` is rejected; use a leading-dot subtree alias"),
                    ));
                }
            }
        }

        if let Some(dependencies) = &service.depends_on {
            let mut seen = BTreeSet::new();
            for dependency in dependencies {
                let dependency_path = format!("{path}.depends_on");
                if dependency.service == *name {
                    errors.push(StackError::new(
                        dependency_path.clone(),
                        "a service cannot depend on itself",
                    ));
                } else if !stack.services.contains_key(&dependency.service) {
                    errors.push(StackError::new(
                        dependency_path.clone(),
                        format!("`{}` is not a declared service", dependency.service),
                    ));
                } else if dependency.condition == Condition::ServiceHealthy {
                    let target = &stack.services[&dependency.service];
                    if target.healthcheck.is_none() {
                        errors.push(StackError::new(
                            dependency_path.clone(),
                            format!(
                                "`{}` declares no healthcheck, so `service_healthy` \
                                 can never be satisfied",
                                dependency.service
                            ),
                        ));
                    }
                }
                if !seen.insert(dependency.service.clone()) {
                    errors.push(StackError::new(
                        dependency_path,
                        format!("`{}` is listed more than once", dependency.service),
                    ));
                }
            }
        }
    }

    if let Some(network) = &stack.network {
        if network.subnet.is_some() && network.mode != Some(NetworkMode::Isolated) {
            errors.push(StackError::new(
                "[network].subnet",
                "`subnet` is only valid with `mode = \"isolated\"`",
            ));
        }
        if let Some(subnet) = &network.subnet {
            if !is_ipv4_cidr(subnet) {
                errors.push(StackError::new(
                    "[network].subnet",
                    format!("`{subnet}` is not an IPv4 CIDR, such as `192.168.100.0/24`"),
                ));
            }
        }
    }

    if let Some(cycle) = find_cycle(stack) {
        let first = cycle.first().map(String::as_str).unwrap_or("unknown");
        errors.push(StackError::new(
            format!("[services.{first}.depends_on]"),
            format!("dependency cycle: {}", cycle.join(" -> ")),
        ));
    }

    errors
}

/// A start order with a deterministic tie-break: independent services are
/// emitted in name order, so two runs of the same stack agree.
pub fn start_order(stack: &StackFile) -> Vec<String> {
    let mut indegree: BTreeMap<String, usize> = stack
        .services
        .keys()
        .map(|name| (name.clone(), 0))
        .collect();
    let mut successors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for (name, service) in &stack.services {
        let Some(dependencies) = &service.depends_on else {
            continue;
        };
        let unique: BTreeSet<&str> = dependencies
            .iter()
            .map(|dependency| dependency.service.as_str())
            .filter(|dependency| stack.services.contains_key(*dependency))
            .collect();
        for dependency in unique {
            successors
                .entry(dependency.to_owned())
                .or_default()
                .insert(name.clone());
            *indegree.get_mut(name).expect("service is a key") += 1;
        }
    }

    let mut ready: BTreeSet<String> = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(name, _)| name.clone())
        .collect();
    let mut order = Vec::new();

    while let Some(name) = ready.iter().next().cloned() {
        ready.remove(&name);
        order.push(name.clone());
        if let Some(next) = successors.get(&name) {
            for successor in next {
                let count = indegree.get_mut(successor).expect("successor exists");
                *count -= 1;
                if *count == 0 {
                    ready.insert(successor.clone());
                }
            }
        }
    }

    order
}

pub fn is_slug(value: &str) -> bool {
    if value.is_empty() || value.len() > 31 {
        return false;
    }
    let mut chars = value.chars();
    let first = chars.next().expect("non-empty");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_env_key(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_ipv4_cidr(value: &str) -> bool {
    let Some((address, prefix)) = value.rsplit_once('/') else {
        return false;
    };
    if address.parse::<Ipv4Addr>().is_err() {
        return false;
    }
    let Ok(prefix) = prefix.parse::<u8>() else {
        return false;
    };
    prefix <= 32
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Colour {
    White,
    Grey,
    Black,
}

fn find_cycle(stack: &StackFile) -> Option<Vec<String>> {
    fn visit(
        name: &str,
        stack: &StackFile,
        colours: &mut BTreeMap<String, Colour>,
        trail: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        match colours.get(name).copied().unwrap_or(Colour::White) {
            Colour::Grey => {
                let start = trail.iter().position(|entry| entry == name).unwrap_or(0);
                let mut cycle = trail[start..].to_vec();
                cycle.push(name.to_owned());
                return Some(cycle);
            }
            Colour::Black => return None,
            Colour::White => {}
        }

        colours.insert(name.to_owned(), Colour::Grey);
        trail.push(name.to_owned());

        if let Some(service) = stack.services.get(name) {
            if let Some(dependencies) = &service.depends_on {
                for dependency in dependencies {
                    if !stack.services.contains_key(&dependency.service) {
                        continue;
                    }
                    if let Some(cycle) = visit(&dependency.service, stack, colours, trail) {
                        return Some(cycle);
                    }
                }
            }
        }

        trail.pop();
        colours.insert(name.to_owned(), Colour::Black);
        None
    }

    let mut colours = BTreeMap::new();
    for name in stack.services.keys() {
        if colours.get(name).copied().unwrap_or(Colour::White) == Colour::White {
            if let Some(cycle) = visit(name, stack, &mut colours, &mut Vec::new()) {
                return Some(cycle);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{is_slug, start_order};
    use crate::model::StackFile;

    fn stack(text: &str) -> StackFile {
        toml::from_str(text).expect("valid model")
    }

    #[test]
    fn slugs_are_the_documented_shape() {
        assert!(is_slug("mitos"));
        assert!(is_slug("a"));
        assert!(is_slug("a-b9"));
        assert!(!is_slug("Mitos"));
        assert!(!is_slug("-mitos"));
        assert!(!is_slug("mitos_1"));
        assert!(!is_slug(""));
        assert!(!is_slug(&"a".repeat(32)));
    }

    #[test]
    fn independent_services_are_ordered_by_name() {
        let stack =
            stack("version = 1\n[services.b]\nimage = \"x\"\n[services.a]\nimage = \"x\"\n");
        assert_eq!(start_order(&stack), vec!["a", "b"]);
    }

    #[test]
    fn a_dependency_starts_before_its_dependent() {
        let stack = stack(
            "version = 1\n\
             [services.app]\nimage = \"x\"\n\
             [[services.app.depends_on]]\nservice = \"db\"\ncondition = \"service_started\"\n\
             [services.db]\nimage = \"x\"\n",
        );
        let order = start_order(&stack);
        assert_eq!(order, vec!["db", "app"]);
    }
}
