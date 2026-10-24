//! Errors, and the exit code each one carries.
//!
//! Only the variants a command can currently return are defined. `Refused`,
//! `Readiness` and `Unavailable` join this enum with `plan`, `up` and the
//! runtime adapter; `Usage` never does, because clap owns usage errors and
//! already exits 2.

use std::fmt;

use container_compose_stack::error::StackError;

/// What a command returns when it cannot produce an answer.
#[derive(Debug)]
pub enum Error {
    /// The stack file does not validate. Nothing was executed.
    Stack(Vec<StackError>),
    /// Something that should not happen, such as rendering a document. Exit 1.
    Internal(String),
}

impl Error {
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::Stack(_) => 2,
            Error::Internal(_) => 1,
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
            Error::Internal(message) => write!(f, "{message}"),
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
