//! The env file: one file per stack, read as a substitution source.
//!
//! There is no per-service file and no automatic injection. A service names the
//! variables it wants (`API_KEY = "${SIEVE_API_KEY}"`), the shell environment
//! wins over the file, and a value that is not in either is a load error.
//!
//! It is also the one place the tool reads something that may hold a secret, so
//! it can ask git whether the file is safe. That check refuses in `plan`/`up`
//! (via `Guard`) and warns in `config`; a file that is tracked, or merely not
//! ignored, is one `git add .` from being committed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::StackError;

#[derive(Debug, Clone)]
pub struct EnvSource {
    pub path: Option<PathBuf>,
    pub values: BTreeMap<String, String>,
}

/// What git says about a file that may hold secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Not in a worktree, or git is unavailable. Nothing to say.
    NotChecked,
    /// Ignored and not tracked. The safe state.
    Ignored,
    /// In the index: it will be committed.
    Tracked,
    /// Untracked but not ignored: the next `git add .` will commit it.
    NotIgnored,
}

impl Guard {
    pub fn is_dangerous(self) -> bool {
        matches!(self, Guard::Tracked | Guard::NotIgnored)
    }
}

/// Read `declared` relative to the stack file, or `.env` when nothing is
/// declared. A declared file that is missing is an error; an absent default
/// `.env` is not.
pub fn load(declared: Option<&str>, dir: &Path) -> Result<EnvSource, StackError> {
    let (path, required) = match declared {
        Some(relative) => (dir.join(relative), true),
        None => (dir.join(".env"), false),
    };

    if !path.exists() {
        return if required {
            Err(StackError::new(
                "[env].file",
                format!("`{}` does not exist", path.display()),
            ))
        } else {
            Ok(EnvSource {
                path: None,
                values: BTreeMap::new(),
            })
        };
    }

    let entries = dotenvy::from_path_iter(&path).map_err(|error| {
        StackError::new(
            "[env].file",
            format!("cannot read `{}`: {error}", path.display()),
        )
    })?;

    let mut values = BTreeMap::new();
    for entry in entries {
        let (key, value) = entry.map_err(|error| {
            StackError::new("[env].file", format!("`{}`: {error}", path.display()))
        })?;
        values.insert(key, value);
    }

    Ok(EnvSource {
        path: Some(path),
        values,
    })
}

/// Replace `${NAME}` with the process environment's value, or the file's.
///
/// Only one pass: a substituted value is not itself scanned, so a value that
/// happens to contain `${` cannot recurse.
pub fn substitute(value: &str, file: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;

    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err("unclosed `${`; write `${NAME}`".to_owned());
        };
        let name = &after[..end];
        if name.is_empty() {
            return Err("`${}` has no variable name".to_owned());
        }
        match std::env::var(name).ok().or_else(|| file.get(name).cloned()) {
            Some(resolved) => out.push_str(&resolved),
            None => {
                return Err(format!(
                    "`${{{name}}}` is set neither in the environment nor in the env file"
                ));
            }
        }
        rest = &after[end + 1..];
    }

    out.push_str(rest);
    Ok(out)
}

/// Ask git whether `path` is safe to hold secrets.
pub fn check_secret_file(path: &Path) -> Guard {
    let Some(parent) = path.parent() else {
        return Guard::NotChecked;
    };

    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(parent)
        .args(["rev-parse", "--show-toplevel"])
        .output()
    else {
        return Guard::NotChecked;
    };
    if !output.status.success() {
        return Guard::NotChecked;
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if root.is_empty() {
        return Guard::NotChecked;
    }
    let Ok(relative) = path.strip_prefix(&root) else {
        return Guard::NotChecked;
    };

    if let Ok(tracked) = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(relative)
        .output()
    {
        if tracked.status.success() {
            return Guard::Tracked;
        }
    }

    match Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["check-ignore", "-q", "--"])
        .arg(relative)
        .status()
    {
        Ok(status) if status.success() => Guard::Ignored,
        Ok(_) => Guard::NotIgnored,
        Err(_) => Guard::NotChecked,
    }
}

#[cfg(test)]
mod tests {
    use super::{check_secret_file, substitute, Guard};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn substitutes_from_the_file() {
        let mut file = BTreeMap::new();
        file.insert("API_KEY".to_owned(), "abc".to_owned());
        assert_eq!(substitute("${API_KEY}", &file).unwrap(), "abc");
        assert_eq!(
            substitute("prefix-${API_KEY}-suffix", &file).unwrap(),
            "prefix-abc-suffix"
        );
    }

    #[test]
    fn an_unset_variable_names_itself() {
        let file = BTreeMap::new();
        let message = substitute("${MISSING}", &file).unwrap_err();
        assert!(message.contains("MISSING"), "{message}");
    }

    #[test]
    fn an_unclosed_brace_is_an_error() {
        assert!(substitute("${MISSING", &BTreeMap::new()).is_err());
    }

    #[test]
    fn a_directory_outside_a_worktree_is_not_checked() {
        let directory = tempfile::TempDir::new().unwrap();
        assert_eq!(
            check_secret_file(&directory.path().join(".env")),
            Guard::NotChecked
        );
        let _ = Path::new(".");
    }

    #[test]
    fn git_reports_untracked_ignored_and_tracked() {
        let directory = tempfile::TempDir::new().unwrap();
        let root = directory.path();
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "-q"])
            .status()
            .expect("git is on PATH");
        assert!(status.success());

        let path = root.join(".env");
        fs::write(&path, "KEY=value\n").unwrap();
        assert_eq!(check_secret_file(&path), Guard::NotIgnored);

        fs::write(root.join(".gitignore"), ".env\n").unwrap();
        assert_eq!(check_secret_file(&path), Guard::Ignored);

        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["add", "-f", ".env"])
            .status()
            .expect("git is on PATH");
        assert!(status.success());
        assert_eq!(check_secret_file(&path), Guard::Tracked);
    }
}
