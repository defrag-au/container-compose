//! One problem in the stack file, anchored to a key path.
//!
//! The stack crate has no exit codes: those belong to the command that reports
//! the failure. Keeping this type here is what lets the parser and validator be
//! tested without a binary.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackError {
    /// `[services.sieve].envs`, or `version` at the root.
    pub path: String,
    pub message: String,
}

impl StackError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for StackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for StackError {}
