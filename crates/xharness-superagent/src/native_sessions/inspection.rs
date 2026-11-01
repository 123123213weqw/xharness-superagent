//! Passive transcript input for the product resource reader. Unlike ensure,
//! inspection cannot create/adopt a workspace, register tools/models/prompts,
//! prepare an actor, acquire its execution lease, or wake a queued input.
#![forbid(unsafe_code)]
#![forbid(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use super::*;
use xharness_api::ScopedSessionCreated;

impl NativeSessions {
    /// Trusted product owner only: logical name and role must come from its
    /// SQL/role binding, never directly from a model/API native Session ID.
    /// Returns an immutable existing JSONL snapshot, NOT a ManagedSession or
    /// execution capability. The caller still enforces ticket read permissions.
    pub async fn inspect_existing(&self, logical: &str, role: Role) -> Result<Option<Session>> {
        if logical.is_empty() || logical.len() > 512 || logical.chars().any(char::is_control) {
            return Err(failure("invalid managed logical Session identity"));
        }
        let request_key = digest(&["xharness.superagent.session.v1", role.name(), logical])?;
        let native = format!("remote-{request_key}");
        let Some(snapshot) = self
            .store
            .load(&native)
            .await
            .map_err(Error::ObservationJournal)?
        else {
            return Ok(None);
        };
        let receipt_id = format!("remote-create-{request_key}");
        let mut receipts = snapshot
            .events()
            .iter()
            .filter_map(|event| match event.data() {
                EventData::SessionMutationCommitted { receipt } if receipt.rpc_id == receipt_id => {
                    Some(receipt)
                }
                _ => None,
            });
        let receipt = receipts
            .next()
            .ok_or_else(|| failure("managed Session creation proof unavailable"))?;
        if receipts.next().is_some()
            || receipt.method != RpcMethod::SessionCreate.as_str()
            || receipt.response_event_seq_field.is_some()
        {
            return Err(failure("managed Session creation proof unavailable"));
        }
        let created: ScopedSessionCreated = serde_json::from_value(receipt.response.clone())?;
        let expected = ScopedSessionCreate {
            workspace: created.workspace,
            request_key,
        };
        if !expected.is_bounded()
            || snapshot.header().id != native
            || created.session != native
            || created.request_key != expected.request_key
            || receipt.fingerprint
                != xharness_control::mutation_fingerprint(
                    RpcMethod::SessionCreate.as_str(),
                    &serde_json::to_value(&expected)?,
                )
            || self
                .host
                .scoped_session_creation_receipt(expected.clone())
                .await
                .map_err(rpc_failure)?
                != Some(expected.receipt())
        {
            return Err(failure("managed Session creation proof unavailable"));
        }
        Ok(Some(snapshot))
    }
}
