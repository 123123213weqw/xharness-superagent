//! Durable control state for long-lived XHarness agents.
//!
//! The model/tool loop remains a turn executor. This crate owns pending input,
//! stable message identities and atomic claim batches that survive process
//! restart. The session event log is the only authoritative history.

mod activation;
mod dispatch;
pub use dispatch::*;
mod driver;
mod inbox;
mod input_dispatch;
pub use input_dispatch::is_waking_step;
mod input_admission;
mod input_completion;
mod prepared_input;
pub use input_admission::{InputAdmissionError, ObservedInputMode, PendingInputAdmission};
pub use prepared_input::PreparedInputAdmission;
mod lease;
mod lifecycle;
mod observation;
pub use observation::{AgentActivity, AgentObservation};

pub use activation::*;
pub use driver::*;
pub use inbox::*;
pub use lease::*;
pub use lifecycle::*;
pub use xharness_session::{InboxMessage, InboxTarget};

mod delegation;
pub use delegation::{AgentOperation, AgentTaskCommand, DelegationRuntime, TaskReviewOutcome};

mod goal;
pub use goal::*;

#[cfg(test)]
mod stability_repro;

#[cfg(test)]
mod retirement_tests;
