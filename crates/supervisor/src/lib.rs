//! The foreground supervisor.
//!
//! The runtime has no restart policy and records no health: a container is
//! `running` or `stopped`, and nothing starts it again. So supervision is a
//! property of a live process, and this crate is that process.
//!
//! The split that keeps it testable:
//!
//! - [`tick`] is pure. Time and observations are parameters, and the output is
//!   a list of actions. A crash loop is a fake clock, not a sleeping test.
//! - [`run`] executes those actions against the runtime, installs the signal
//!   handlers and writes the health cache.
//! - [`cache`] and [`lock`] are the two pieces of on-disk state: the health
//!   `ps` reads, and the pid that keeps two supervisors off one project.

pub mod cache;
pub mod clock;
pub mod lock;
pub mod run;
pub mod tick;

pub use run::{interrupt_action, run, OnInterrupt, Options, SupervisorError};
pub use tick::{Action, Health, ProbeOutcome, ServiceSpec, Supervisor, View};
