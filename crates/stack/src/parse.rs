//! Reading `stack.toml`, and refusing it with a key path when it is wrong.
//!
//! Two passes. The first walks the untyped document against a key schema so an
//! unknown key can be named precisely (`[services.sieve].envs`) and a near miss
//! can suggest the intended key. The second deserialises into the typed model,
//! where `serde_path_to_error` supplies the path for a type error.
//!
//! The untyped pass exists only to produce those messages. It is not a data
//! model: nothing outside this module sees a `toml::Value`, and the typed
//! structs are the only representation the rest of the tool handles. A schema
//! walk is also the only way to say "did you mean `image`?" — `deny_unknown_fields`
//! reports the field but not where it was.

use std::path::Path;

use crate::error::StackError;

use super::model::StackFile;

const ROOT: &[&str] = &["version", "name", "network", "volumes", "services", "env"];
const NETWORK: &[&str] = &["mode", "subnet"];
const ENV: &[&str] = &["file"];
const VOLUME: &[&str] = &["size", "journal"];
const SERVICE: &[&str] = &[
    "image",
    "build",
    "pull",
    "command",
    "entrypoint",
    "env",
    "user",
    "workdir",
    "init",
    "shm_size",
    "cap_add",
    "cap_drop",
    "ulimits",
    "volumes",
    "tmpfs",
    "read_only",
    "ports",
    "aliases",
    "depends_on",
    "restart",
    "max_retries",
    "healthcheck",
    "labels",
    "cpus",
    "memory",
];
const BUILD: &[&str] = &["context", "containerfile", "target", "args"];
const HEALTHCHECK: &[&str] = &["exec", "interval", "timeout", "start_period", "retries"];
const DEPENDENCY: &[&str] = &["service", "condition"];

pub fn parse_file(path: &Path) -> Result<StackFile, Vec<StackError>> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        vec![StackError::new(
            path.display().to_string(),
            format!("cannot read the stack file: {error}"),
        )]
    })?;
    parse_text(&text, path)
}

pub fn parse_text(text: &str, path: &Path) -> Result<StackFile, Vec<StackError>> {
    let document: toml::Value = match text.parse() {
        Ok(document) => document,
        Err(error) => {
            return Err(vec![StackError::new(
                path.display().to_string(),
                clean_message(&error.to_string()),
            )]);
        }
    };

    let mut errors = Vec::new();
    if !document.is_table() {
        errors.push(StackError::new(
            path.display().to_string(),
            "the stack file must be a table",
        ));
        return Err(errors);
    }

    let mut trail = Vec::new();
    walk(&document, &mut trail, &mut errors);
    required(&document, &mut errors);
    if !errors.is_empty() {
        return Err(errors);
    }

    let deserializer = toml::de::Deserializer::new(text);
    serde_path_to_error::deserialize::<_, StackFile>(deserializer).map_err(|error| {
        vec![StackError::new(
            display_path(&error.path().to_string()),
            clean_message(&error.inner().to_string()),
        )]
    })
}

fn walk(value: &toml::Value, trail: &mut Vec<String>, errors: &mut Vec<StackError>) {
    match value {
        toml::Value::Table(table) => {
            if let Some(allowed) = allowed(trail) {
                for (key, child) in table {
                    if allowed.contains(&key.as_str()) {
                        trail.push(key.clone());
                        walk(child, trail, errors);
                        trail.pop();
                    } else {
                        errors.push(unknown_key(trail, key, allowed));
                    }
                }
            } else if is_map_of_tables(trail) {
                // `[services]` and `[volumes]` hold user-chosen names, so their
                // keys are not part of the schema; the values are.
                for (key, child) in table {
                    trail.push(key.clone());
                    walk(child, trail, errors);
                    trail.pop();
                }
            }
            // Otherwise this is a map of scalars (`env`, `labels`, `build.args`),
            // where every key belongs to the user and serde checks the values.
        }
        toml::Value::Array(items) if trail.last().map(String::as_str) == Some("depends_on") => {
            for (index, item) in items.iter().enumerate() {
                trail.push(format!("[{index}]"));
                walk(item, trail, errors);
                trail.pop();
            }
        }
        _ => {}
    }
}

fn is_map_of_tables(trail: &[String]) -> bool {
    matches!(trail, [only] if only == "services" || only == "volumes")
}

fn allowed(trail: &[String]) -> Option<&'static [&'static str]> {
    let parts: Vec<&str> = trail
        .iter()
        .map(|part| {
            if part.starts_with('[') {
                "*"
            } else {
                part.as_str()
            }
        })
        .collect();
    match parts.as_slice() {
        [] => Some(ROOT),
        ["network"] => Some(NETWORK),
        ["env"] => Some(ENV),
        ["volumes", _] => Some(VOLUME),
        ["services", _] => Some(SERVICE),
        ["services", _, "build"] => Some(BUILD),
        ["services", _, "healthcheck"] => Some(HEALTHCHECK),
        ["services", _, "depends_on", "*"] => Some(DEPENDENCY),
        _ => None,
    }
}

fn unknown_key(trail: &[String], key: &str, allowed: &[&str]) -> StackError {
    let message = match closest(key, allowed) {
        Some(candidate) => format!("unknown key; did you mean `{candidate}`?"),
        None => format!("unknown key; expected one of {}", list(allowed)),
    };
    StackError::new(key_path(trail, key), message)
}

fn required(document: &toml::Value, errors: &mut Vec<StackError>) {
    let Some(table) = document.as_table() else {
        return;
    };
    if !table.contains_key("version") {
        errors.push(StackError::new(
            "version",
            "required; this binary understands version 1",
        ));
    }
    match table.get("services") {
        None => errors.push(StackError::new(
            "[services]",
            "at least one service is required",
        )),
        Some(toml::Value::Table(services)) if services.is_empty() => errors.push(StackError::new(
            "[services]",
            "at least one service is required",
        )),
        _ => {}
    }
}

fn key_path(trail: &[String], key: &str) -> String {
    if trail.is_empty() {
        key.to_owned()
    } else {
        format!("[{}].{key}", table_path(trail))
    }
}

fn table_path(trail: &[String]) -> String {
    let mut out = String::new();
    for part in trail {
        if part.starts_with('[') {
            out.push_str(part);
        } else {
            out.push('.');
            out.push_str(part);
        }
    }
    out.trim_start_matches('.').to_owned()
}

/// Turn a `serde_path_to_error` path into the same `[table].key` shape the
/// key walk uses.
fn display_path(path: &str) -> String {
    let path = path.trim_start_matches('.');
    if path.is_empty() || path == "." {
        return "stack".to_owned();
    }
    match path.rsplit_once('.') {
        Some((parent, key)) if !key.is_empty() => format!("[{parent}].{key}"),
        _ => format!("[{path}]"),
    }
}

fn clean_message(message: &str) -> String {
    let lines: Vec<&str> = message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match (lines.first(), lines.last()) {
        (Some(first), Some(last)) if first.starts_with("TOML parse error") && first != last => {
            format!("{last} ({first})")
        }
        (_, Some(last)) => (*last).to_owned(),
        _ => message.to_owned(),
    }
}

fn list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn closest<'a>(key: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        let distance = edit_distance(key, candidate);
        if distance <= 2 && best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, left_char) in left.chars().enumerate() {
        current[0] = row + 1;
        for (column, right_char) in right.iter().enumerate() {
            let cost = usize::from(left_char != *right_char);
            current[column + 1] = (previous[column] + cost)
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::{edit_distance, parse_text};
    use std::path::Path;

    fn errors(text: &str) -> Vec<String> {
        parse_text(text, Path::new("stack.toml"))
            .unwrap_err()
            .into_iter()
            .map(|error| error.path)
            .collect()
    }

    #[test]
    fn an_unknown_key_is_named_with_its_table() {
        let paths = errors("version = 1\n[services.sieve]\nimage = \"x\"\nenvs = {}\n");
        assert!(
            paths.contains(&"[services.sieve].envs".to_owned()),
            "{paths:?}"
        );
    }

    #[test]
    fn a_near_miss_is_suggested() {
        let error = parse_text(
            "version = 1\n[services.sieve]\nimage = \"x\"\nrestar = \"no\"\n",
            Path::new("stack.toml"),
        )
        .unwrap_err();
        assert!(error[0].message.contains("`restart`"), "{error:?}");
    }

    #[test]
    fn missing_required_keys_are_named() {
        let paths = errors("[services.a]\nimage = \"x\"\n");
        assert!(paths.contains(&"version".to_owned()), "{paths:?}");
    }

    #[test]
    fn a_block_sequence_is_a_syntax_error_with_a_position() {
        let error = parse_text("version = 1\nservices = [\n", Path::new("stack.toml")).unwrap_err();
        assert!(error[0].message.contains("TOML parse error"), "{error:?}");
    }

    #[test]
    fn distance_is_small_for_a_typo() {
        assert_eq!(edit_distance("restar", "restart"), 1);
        assert_eq!(edit_distance("envs", "env"), 1);
        assert_eq!(edit_distance("image", "build"), 5);
    }
}
