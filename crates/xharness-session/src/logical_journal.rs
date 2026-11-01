//! Optional canonical hot journal capability; never a durability/Agent lease.
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

use crate::{AppendReceipt, Revision, Session, SessionEvent, StoreError};

/// A prepared Store's ONE canonical Session, not a caller-owned transcript.
/// Snapshot/append are synchronous and perform no physical I/O. Append uses
/// ordinary Session validation/CAS. Flush is an explicit async barrier bound
/// to THIS journal generation, not a later owner reusing the same Session id.
/// Close/retirement rejects new mutations; existing reads remain historical.
#[async_trait::async_trait]
pub trait LogicalJournal: Send + Sync + 'static {
    /// Storage mutation lifetime only, NOT an Agent/worker liveness certificate.
    fn is_closed(&self) -> bool;
    async fn flush(&self) -> Result<Revision, StoreError>;
    fn snapshot(&self) -> Result<Session, StoreError>;
    fn append(
        &self,
        expected: Revision,
        events: Vec<SessionEvent>,
    ) -> Result<AppendReceipt, StoreError>;
}
