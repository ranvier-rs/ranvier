//! Background job scheduling and execution for Ranvier.
//!
//! This crate provides the `Scheduler` which allows you to execute `Axon` instances
//! on intervals or according to cron expressions. It runs via a Tokio background loop.

pub mod scheduler;
pub mod job;
pub mod trigger;

pub use scheduler::Scheduler;
pub use job::{Job, JobId};
pub use trigger::Trigger;

pub mod prelude {
    pub use crate::{Scheduler, Job, JobId, Trigger};
}
