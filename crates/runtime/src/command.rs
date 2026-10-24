//! Running the runtime binary, with the argv preserved for the error message.
//!
//! Every failure carries the exact argv that was issued, because "the container
//! command failed" is not actionable and the argv is the thing the tool
//! promised to show.

use std::collections::BTreeMap;
use std::fmt;
use std::io::BufRead;
use std::time::{Duration, Instant};

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

/// How a command that was given a deadline finished.
#[derive(Debug, Clone)]
pub enum Completion {
    Exited(Output),
    /// The child was killed because it outlived its deadline.
    TimedOut,
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
        let output = std::process::Command::new(&self.program)
            .args(&self.args)
            .envs(&self.envs)
            .output()
            .map_err(|error| self.spawn_error(error))?;

        self.finish(self.output_of(output))
    }

    /// Run with a deadline. Health probes use this: the runtime offers no
    /// timeout of its own, and a hung `exec` must not hang the supervisor.
    pub fn run_with_timeout(&self, timeout: Duration) -> Result<Completion, RuntimeError> {
        let mut child = std::process::Command::new(&self.program)
            .args(&self.args)
            .envs(&self.envs)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| self.spawn_error(error))?;

        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait().map_err(|error| self.spawn_error(error))? {
                Some(_) => {
                    let output = child
                        .wait_with_output()
                        .map_err(|error| self.spawn_error(error))?;
                    return Ok(Completion::Exited(self.output_of(output)));
                }
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(Completion::TimedOut);
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// Run with stdout delivered line by line, for `logs`. The exit status and
    /// stderr are returned once the child closes stdout.
    pub fn stream_lines(&self, on_line: &mut dyn FnMut(&str)) -> Result<Output, RuntimeError> {
        let mut child = std::process::Command::new(&self.program)
            .args(&self.args)
            .envs(&self.envs)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| self.spawn_error(error))?;

        if let Some(stdout) = child.stdout.take() {
            for line in std::io::BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => on_line(&line),
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(self.spawn_error(error));
                    }
                }
            }
        }

        let output = child
            .wait_with_output()
            .map_err(|error| self.spawn_error(error))?;
        self.finish(self.output_of(output))
    }

    fn finish(&self, output: Output) -> Result<Output, RuntimeError> {
        if output.status == 0 {
            Ok(output)
        } else {
            Err(RuntimeError {
                argv: self.full_argv(),
                status: Some(output.status),
                stderr: output.stderr,
            })
        }
    }

    fn full_argv(&self) -> Vec<String> {
        std::iter::once(self.program.clone())
            .chain(self.args.iter().cloned())
            .collect()
    }

    fn output_of(&self, output: std::process::Output) -> Output {
        let status = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Output {
            status,
            stdout,
            stderr,
        }
    }

    fn spawn_error(&self, error: std::io::Error) -> RuntimeError {
        RuntimeError {
            argv: self.full_argv(),
            status: None,
            stderr: format!("cannot run `{}`: {error}", self.program),
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
