//! Errors, and the exit code each one carries.
//!
//! Only the variants a command can currently return are defined. `Readiness`
//! joins this enum with the supervisor; `Usage` never does, because clap owns
//! usage errors and already exits 2.

use std::fmt;

use container_compose_plan::apply::ApplyError;
use container_compose_plan::Refusal;
use container_compose_stack::error::StackError;

/// What a command returns when it cannot produce an answer.
#[derive(Debug)]
pub enum Error {
    /// The stack file does not validate. Nothing was executed.
    Stack(Vec<StackError>),
    /// A valid stack that this machine's existing state will not allow.
    Refused(Vec<Refusal>),
    /// A command line that names something the stack does not have.
    Usage(String),
    /// A runtime command failed after the stack started changing.
    Runtime(String),
    /// Containers are running but did not become healthy.
    Readiness(String),
    /// The second interrupt skipped the graceful stop.
    Interrupted,
    /// Something that should not happen, such as rendering a document. Exit 1.
    Internal(String),
    /// `container` is missing, its output cannot be parsed, or the apiserver is down.
    Unavailable(String),
}

impl Error {
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::Stack(_) => 2,
            Error::Refused(_) => 3,
            Error::Usage(_) => 2,
            Error::Runtime(_) => 1,
            Error::Readiness(_) => 4,
            Error::Interrupted => 130,
            Error::Internal(_) => 1,
            Error::Unavailable(_) => 5,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Stack(errors) => {
                let lines: Vec<String> = errors.iter().map(ToString::to_string).collect();
                write!(f, "{}", lines.join("\n"))
            }
            Error::Refused(refusals) => {
                let lines: Vec<String> = refusals
                    .iter()
                    .map(|refusal| format!("refused: {}: {}", refusal.path, refusal.message))
                    .collect();
                write!(f, "{}", lines.join("\n"))
            }
            Error::Internal(message) => write!(f, "{message}"),
            Error::Usage(message) => write!(f, "{message}"),
            Error::Runtime(message) => write!(f, "{message}"),
            Error::Readiness(message) => write!(f, "{message}"),
            Error::Interrupted => f.write_str("interrupted twice; containers were killed"),
            Error::Unavailable(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<StackError> for Error {
    fn from(error: StackError) -> Self {
        Error::Stack(vec![error])
    }
}

impl From<Vec<StackError>> for Error {
    fn from(errors: Vec<StackError>) -> Self {
        Error::Stack(errors)
    }
}

impl From<ApplyError> for Error {
    fn from(error: ApplyError) -> Self {
        match error {
            ApplyError::Usage(message) => Error::Usage(message),
            ApplyError::Readiness { .. } => Error::Readiness(error.to_string()),
            other => Error::Runtime(other.to_string()),
        }
    }
}
