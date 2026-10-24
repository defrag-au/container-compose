//! The ownership labels.
//!
//! They are the tool's only durable identity in the runtime: the name is the
//! handle, but the labels are what make `down` safe. One module, so the plan,
//! the argv builder and the observers cannot drift.

pub const PROJECT: &str = "dev.container-compose.project";
pub const SERVICE: &str = "dev.container-compose.service";
pub const CONFIG: &str = "dev.container-compose.config";
pub const SCHEMA: &str = "dev.container-compose.schema";
/// Marks a one-off `run` container. `ps` shows it while it lives; `down` and
/// the plan ignore it, because it is not part of the stack.
pub const RUN: &str = "dev.container-compose.run";
pub const SCHEMA_VERSION: &str = "1";
