//! The `container` binary: argv construction, observation and execution.
//!
//! Nothing here knows about plans or commands. It turns the resolved stack into
//! the argv the runtime takes, parses what the runtime reports, and runs the
//! process. The fake runtime in `dev/fake-container` implements the same
//! surface, so the argv can be tested without a real runtime.

pub mod argv;
pub mod command;
pub mod labels;
pub mod observe;
pub mod probe;

pub use command::{Command, Completion, Output, RuntimeError};
pub use observe::{
    normalize_image_reference, observe, ContainerConfiguration, ContainerImage, ContainerStatus,
    ImageConfiguration, NetworkConfiguration, NetworkStatus, Observed, ObservedContainer,
    ObservedImage, ObservedNetwork, ObservedVolume, VolumeConfiguration,
};
pub use probe::{probe, Probe};
