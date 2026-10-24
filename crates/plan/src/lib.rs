//! What `up` would do, as data.
//!
//! `plan` is a pure function of the resolved stack and the observed world. It
//! never touches the runtime; the CLI observes and renders, `up` executes the
//! same steps. A refusal is part of the plan, so `plan` can print the whole
//! picture and still exit 3.

pub mod apply;
pub mod plan;

pub use apply::{down, up, ApplyError, DownOptions, UpOptions};
pub use plan::{
    plan, plan_with, Action, Detail, Options as PlanOptions, Plan, Refusal, Resource, Step,
};
