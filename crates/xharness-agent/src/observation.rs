//! Passive lifecycle/event subscriptions; never an activation or control lease.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    unused_must_use
)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::let_underscore_must_use,
        unused_must_use
    )
)]
use crate::{AgentEvent, AgentStatus, WorkerAvailability};
use tokio::sync::{broadcast, watch};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentActivity {
    pub generation: u64,
    pub availability: WorkerAvailability,
    /// Unavailable/starting/closed are NOT evidence that a worker is idle.
    pub status: Option<AgentStatus>,
}
/// Subscriptions are installed before a caller replays the authoritative
/// Session. Broadcast lag requires a journal refresh, not a fabricated failure.
/// Use activity.borrow_and_update before an async read and has_changed after:
/// comparing equal activity values cannot detect an Idle/Running/Idle ABA.
/// Neither receiver contains a control Sender, activation, or lease owner.
pub struct AgentObservation {
    pub events: broadcast::Receiver<AgentEvent>,
    pub activity: watch::Receiver<AgentActivity>,
}
