//! Running the runtime binary, with the argv preserved for the error message.
//!
//! Every failure carries the exact argv that was issued, because "the container
//! command failed" is not actionable and the argv is the thing the tool
//! promised to show.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone)]
pub struct Command {
    program: String,
    args: Vec<String>,
    envs: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct RuntimeError {
    /// The full argv, program first.
    pub argv: Vec<String>,
    pub status: Option<i32>,
    pub stderr: String,
}

impl Command {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: BTreeMap::new(),
        }
    }

    /// Values passed to the child process only.
    ///
    /// This is how a `.env` value reaches the container: the argv carries
    /// `--env KEY` (the inherit form), and the child's environment carries the
    /// value, so the secret is never in an argv, a plan or a golden file.
    pub fn envs(mut self, values: BTreeMap<String, String>) -> Self {
        self.envs = values;
        self
    }

    pub fn args<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.args
            .extend(values.into_iter().map(|value| value.as_ref().to_owned()));
        self
    }

    pub fn argv(&self) -> &[String] {
        &self.args
    }

    pub fn render(&self) -> String {
        format!("{} {}", self.program, self.args.join(" "))
    }

    pub fn run(&self) -> Result<Output, RuntimeError> {
        let full_argv = std::iter::once(self.program.clone())
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>();

        let output = std::process::Command::new(&self.program)
            .args(&self.args)
            .envs(&self.envs)
            .output()
            .map_err(|error| RuntimeError {
                argv: full_argv.clone(),
                status: None,
                stderr: format!("cannot run `{}`: {error}", self.program),
            })?;

        let status = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if output.status.success() {
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        } else {
            Err(RuntimeError {
                argv: full_argv,
                status: Some(status),
                stderr,
            })
        }
    }
}

impl RuntimeError {
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            argv: Vec::new(),
            status: None,
            stderr: message.into(),
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let argv = self.argv.join(" ");
        let stderr = self.stderr.trim();
        match self.status {
            Some(status) if argv.is_empty() => write!(f, "runtime exited with {status}: {stderr}"),
            Some(status) => write!(f, "`{argv}` exited with {status}: {stderr}"),
            None => write!(f, "{stderr}"),
        }
    }
}

impl std::error::Error for RuntimeError {}
