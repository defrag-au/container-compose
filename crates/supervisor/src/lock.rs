//! The project lock.
//!
//! One supervisor per project. The lock is a pid file next to the health cache;
//! a pid that is gone is ignored, because a killed supervisor must not block
//! the next `up`. `down` reads the same file and refuses while the pid lives —
//! stopping containers a supervisor would immediately restart is not a teardown.

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::cache;

#[derive(Debug)]
pub enum LockError {
    Held { pid: u32 },
    Io(io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::Held { pid } => write!(
                f,
                "another supervisor holds this project (pid {pid}); stop it first"
            ),
            LockError::Io(error) => write!(f, "cannot take the project lock: {error}"),
        }
    }
}

impl std::error::Error for LockError {}

#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
    pid: u32,
}

impl Lock {
    pub fn acquire(project: &str) -> Result<Self, LockError> {
        let dir = cache::dir()
            .ok_or_else(no_directory)
            .map_err(LockError::Io)?;
        Self::acquire_at(dir.join(format!("{project}.lock")))
    }

    pub fn acquire_at(path: PathBuf) -> Result<Self, LockError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(LockError::Io)?;
        }

        // Two attempts: a stale file is removed and the create retried once.
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let pid = std::process::id();
                    write!(file, "{pid}").map_err(LockError::Io)?;
                    return Ok(Self { path, pid });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if let Some(pid) = read_pid(&path) {
                        if cache::is_alive(pid) {
                            return Err(LockError::Held { pid });
                        }
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(error) => return Err(LockError::Io(error)),
            }
        }

        Err(LockError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the lock file could not be taken after removing a stale one",
        )))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Only remove our own lock: a stale one may have been replaced.
        if read_pid(&self.path) == Some(self.pid) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The live owner of a project, if there is one.
pub fn owner(project: &str) -> Option<u32> {
    let path = cache::dir()?.join(format!("{project}.lock"));
    owner_at(&path)
}

pub fn owner_at(path: &Path) -> Option<u32> {
    let pid = read_pid(path)?;
    cache::is_alive(pid).then_some(pid)
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn no_directory() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "neither CONTAINER_COMPOSE_CACHE_DIR nor HOME is set",
    )
}

#[cfg(test)]
mod tests {
    use super::{owner_at, Lock, LockError};

    #[test]
    fn a_second_acquire_reports_the_holder() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("held.lock");
        let lock = Lock::acquire_at(path.clone()).expect("the first acquire succeeds");
        match Lock::acquire_at(path.clone()) {
            Err(LockError::Held { pid }) => assert_eq!(pid, std::process::id()),
            other => panic!("expected Held, got {other:?}"),
        }
        assert_eq!(owner_at(&path), Some(std::process::id()));
        drop(lock);
        assert_eq!(owner_at(&path), None, "dropping the lock releases it");
    }

    #[test]
    fn a_stale_lock_is_taken_over() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stale.lock");
        std::fs::write(&path, "999999").unwrap();
        let lock = Lock::acquire_at(path.clone()).expect("a dead pid does not block");
        assert_eq!(owner_at(&path), Some(std::process::id()));
        drop(lock);
    }
}
