//! Health probes.
//!
//! The runtime records no health, so a probe is `container exec` on a schedule.
//! There are three outcomes, not two, and the difference matters:
//!
//! | observation | meaning |
//! |---|---|
//! | exit 0 | healthy |
//! | a non-zero exit | a failed probe; it counts toward `retries` |
//! | `is not running` on stderr | unknown — the container is gone, which is not the probe's verdict |
//! | `failed to find target executable` | a failed probe that can never pass; the binary is named |
//!
//! The classification is here, in one function, because it is the only signal
//! the runtime offers and the strings are the contract.

use std::time::Duration;

use crate::command::{Command, Completion, Output, RuntimeError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Healthy,
    Failed {
        stderr: String,
    },
    /// The container is not running. This counts for nothing.
    Unknown {
        reason: String,
    },
    /// The probe command does not exist in the container, so the check can
    /// never pass. The binary is named for the warning.
    MissingBinary {
        binary: String,
        stderr: String,
    },
}

pub fn probe(
    program: &str,
    container: &str,
    exec: &[String],
    timeout: Duration,
) -> Result<Probe, RuntimeError> {
    let argv = crate::argv::exec(container, exec);
    match Command::new(program)
        .args(argv.iter().cloned())
        .run_with_timeout(timeout)?
    {
        Completion::TimedOut => Ok(Probe::Failed {
            stderr: format!("timed out after {}ms", timeout.as_millis()),
        }),
        Completion::Exited(output) => Ok(classify(&output)),
    }
}

fn classify(output: &Output) -> Probe {
    if output.status == 0 {
        return Probe::Healthy;
    }
    if output.stderr.contains("is not running") {
        return Probe::Unknown {
            reason: output.stderr.trim().to_owned(),
        };
    }
    if let Some(binary) = missing_binary(&output.stderr) {
        return Probe::MissingBinary {
            binary,
            stderr: output.stderr.trim().to_owned(),
        };
    }
    Probe::Failed {
        stderr: output.stderr.trim().to_owned(),
    }
}

fn missing_binary(stderr: &str) -> Option<String> {
    const MARKER: &str = "failed to find target executable ";
    let start = stderr.find(MARKER)? + MARKER.len();
    let rest = stderr[start..].trim();
    let binary = rest.split_whitespace().next().unwrap_or(rest);
    Some(binary.trim_matches('"').to_owned())
}

#[cfg(test)]
mod tests {
    use super::{classify, missing_binary, Output, Probe};

    fn output(status: i32, stderr: &str) -> Output {
        Output {
            status,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn exit_zero_is_healthy() {
        assert_eq!(classify(&output(0, "")), Probe::Healthy);
    }

    #[test]
    fn a_non_zero_exit_is_a_failed_probe() {
        assert!(matches!(
            classify(&output(3, "Error: no")),
            Probe::Failed { .. }
        ));
    }

    #[test]
    fn a_container_that_is_not_running_is_unknown() {
        assert!(matches!(
            classify(&output(1, "Error: container api is not running")),
            Probe::Unknown { .. }
        ));
    }

    #[test]
    fn a_missing_probe_binary_is_named() {
        let probe = classify(&output(
            1,
            "Error: failed to find target executable /usr/bin/curl",
        ));
        match probe {
            Probe::MissingBinary { binary, .. } => assert_eq!(binary, "/usr/bin/curl"),
            other => panic!("expected MissingBinary, got {other:?}"),
        }
    }

    #[test]
    fn the_marker_without_a_trailing_message_still_yields_a_name() {
        assert_eq!(
            missing_binary("Error: failed to find target executable /bin/curl"),
            Some("/bin/curl".to_owned())
        );
        assert_eq!(missing_binary("no marker here"), None);
    }
}
