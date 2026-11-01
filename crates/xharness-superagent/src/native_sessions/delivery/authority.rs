//! Derived warm scope, not a second Host or execution permission. A trusted
//! resolve FIRST validates and durably flushes the existing scoped creation
//! proof. Later input callbacks validate that same immutable logical generation
//! and original receipt without flushing another producer's unrelated prefix.
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

use super::*;
use xharness_session::SessionMutationReceipt;

#[derive(Clone)]
pub(super) struct Authority {
    pub(super) binding: ManagedSession,
    pub(super) journal: LogicalSession,
    cwd: String,
    epoch: Arc<()>,
    creation: SessionMutationReceipt,
}
fn invalid() -> Error {
    failure("warm managed Session scope unavailable or changed")
}
fn creation(binding: &ManagedSession, session: &Session) -> Result<SessionMutationReceipt> {
    let request = &binding.creation;
    let mut receipts = session
        .events()
        .iter()
        .filter_map(|event| match event.data() {
            EventData::SessionMutationCommitted { receipt }
                if receipt.rpc_id == request.receipt_id() =>
            {
                Some(receipt)
            }
            _ => None,
        });
    let receipt = receipts.next().ok_or_else(invalid)?;
    if receipts.next().is_some()
        || receipt.method != RpcMethod::SessionCreate.as_str()
        || receipt.fingerprint
            != xharness_control::mutation_fingerprint(
                RpcMethod::SessionCreate.as_str(),
                &serde_json::to_value(request)?,
            )
        || receipt.response_event_seq_field.is_some()
        || receipt.response != serde_json::to_value(request.receipt())?
    {
        return Err(invalid());
    }
    Ok(receipt.clone())
}
impl Authority {
    // Private: only called by resolve AFTER root.journal's full existing Host
    // creation proof/flush. A raw ID, path or synthetic receipt is not a grant.
    pub(super) fn capture(
        root: &NativeSessions,
        binding: &ManagedSession,
        journal: LogicalSession,
        epoch: Arc<()>,
    ) -> Result<Self> {
        let session = journal.snapshot().map_err(Error::ObservationJournal)?;
        let value = Self {
            binding: binding.clone(),
            epoch,
            cwd: session.header().cwd.clone().ok_or_else(invalid)?,
            creation: creation(binding, &session)?,
            journal,
        };
        value.snapshot(root)?;
        Ok(value)
    }
    pub(super) fn snapshot(&self, root: &NativeSessions) -> Result<Session> {
        self.with_current(root, Ok)
    }
    pub(super) fn commit(
        &self,
        root: &NativeSessions,
        prepared: PreparedInputAdmission,
        input: InboxMessage,
    ) -> Result<PendingInputAdmission> {
        self.with_current(root, |_| {
            root.gate.require_active().map_err(failure)?;
            prepared.commit(input).map_err(Error::NativeInputAdmission)
        })
    }
    fn with_current<T>(
        &self,
        root: &NativeSessions,
        action: impl FnOnce(Session) -> Result<T>,
    ) -> Result<T> {
        // Exactly one synchronous winner against catalogue retirement. This
        // guard spans the actual logical commit, never a callback or await.
        let epoch = root.warm_authority.lock().map_err(|_| invalid())?;
        if !epoch.valid || !Arc::ptr_eq(&epoch.generation, &self.epoch) {
            return Err(invalid());
        }
        let session = self.view(root)?;
        action(session)
    }
    fn view(&self, root: &NativeSessions) -> Result<Session> {
        if !Arc::ptr_eq(&root.owner, &self.binding.owner)
            || root.gate.closing.load(Ordering::Acquire)
            || self
                .journal
                .status()
                .map_err(Error::ObservationJournal)?
                .closed
        {
            return Err(invalid());
        }
        {
            let roles = root.tools.bindings.read().map_err(|_| invalid())?;
            if roles
                .get(&self.binding.native_id())
                .is_none_or(|role| role.role != self.binding.role || role.cwd != self.cwd)
            {
                return Err(invalid());
            }
        }
        let session = self.journal.snapshot().map_err(Error::ObservationJournal)?;
        if session.header().id != self.binding.native_id()
            || session.header().cwd.as_deref() != Some(self.cwd.as_str())
            || creation(&self.binding, &session)? != self.creation
        {
            return Err(invalid());
        }
        // A prefix witness, not a durable certificate for newly appended data.
        // That data still needs its own real actor/store flush before ack/use.
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, sync::atomic::AtomicU64};
    use xharness_core::{
        IdentityContextPolicy, ModelProvider, ProviderError, ProviderRequest, ProviderStream,
    };
    use xharness_host::{ModelDescriptor, NoTools, RegisteredModel};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _fixture_cleanup = std::fs::remove_dir_all(&self.0);
        }
    }
    struct Never;
    #[async_trait]
    impl ModelProvider for Never {
        async fn stream(
            &self,
            _: ProviderRequest,
            _: CancellationToken,
        ) -> std::result::Result<ProviderStream, ProviderError> {
            Err(ProviderError::new(
                "authority-only test must not execute a model",
            ))
        }
    }
    fn open(path: &Path) -> NativeSessions {
        let mut models = ModelRegistry::new();
        models
            .register(RegisteredModel::new(
                ModelDescriptor::new("offline", "Offline", "model", "Model"),
                Arc::new(Never),
            ))
            .unwrap();
        NativeSessions::open_buffered(
            path,
            models,
            RoleRoutes {
                orchestrator: ModelRoute::new("offline", "model"),
                worker: ModelRoute::new("offline", "model"),
            },
            RoleFactories {
                orchestrator: Arc::new(NoTools),
                worker: Arc::new(NoTools),
            },
            Arc::new(IdentityContextPolicy),
        )
        .unwrap()
    }
    async fn fixture() -> (Dir, NativeSessions, ManagedSession, Authority) {
        let path = std::env::temp_dir().join(format!(
            "xh-warm-authority-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        let dir = Dir(path);
        let root = open(&dir.0.join("state"));
        root.restore().await.unwrap();
        let binding = root.ensure("logical", Role::Worker, &dir.0).await.unwrap();
        root.journal(&binding).await.unwrap();
        let journal = root
            .buffered
            .as_ref()
            .unwrap()
            .prepare(&binding.native_id())
            .await
            .unwrap();
        let authority =
            Authority::capture(&root, &binding, journal, root.warm_epoch().unwrap()).unwrap();
        (dir, root, binding, authority)
    }
    async fn stop(root: &NativeSessions) {
        assert!(root.shutdown(Duration::from_secs(5)).await.is_graceful());
    }
    #[tokio::test]
    async fn warm_creation_witness_does_not_certify_or_flush_a_later_logical_prefix() {
        let (dir, root, binding, authority) = fixture().await;
        let path = dir
            .0
            .join("state/sessions")
            .join(format!("{}.jsonl", binding.native_id()));
        let old = std::fs::read(path.clone()).unwrap();
        let before = authority.snapshot(&root).unwrap();
        root.store
            .append(
                &binding.native_id(),
                before.revision(),
                vec![EventData::AgentDispatchPaused { paused: true }.into()],
            )
            .await
            .unwrap();
        let fresh = authority.snapshot(&root).unwrap();
        assert!(fresh.revision() > before.revision());
        assert_eq!(authority.journal.status().unwrap().pending_batches, 1);
        assert_eq!(std::fs::read(path).unwrap(), old);
        stop(&root).await;
    }
    #[tokio::test]
    async fn warm_creation_witness_rejects_other_owner_changed_role_and_changed_workspace() {
        let (dir, root, binding, authority) = fixture().await;
        let other = open(&dir.0.join("other"));
        assert!(authority.snapshot(&other).is_err());
        {
            let mut roles = root.tools.bindings.write().unwrap();
            let role = roles.get_mut(&binding.native_id()).unwrap();
            role.role = Role::Orchestrator;
        }
        assert!(authority.snapshot(&root).is_err());
        let old = {
            let mut roles = root.tools.bindings.write().unwrap();
            let role = roles.get_mut(&binding.native_id()).unwrap();
            role.role = Role::Worker;
            std::mem::replace(&mut role.cwd, "different workspace".into())
        };
        assert!(authority.snapshot(&root).is_err());
        root.tools
            .bindings
            .write()
            .unwrap()
            .get_mut(&binding.native_id())
            .unwrap()
            .cwd = old;
        assert!(authority.snapshot(&root).is_ok());
        stop(&other).await;
        stop(&root).await;
    }
    #[tokio::test]
    async fn warm_creation_witness_cannot_address_an_identically_recreated_session_generation() {
        let (_dir, root, binding, authority) = fixture().await;
        let original = authority.snapshot(&root).unwrap();
        root.store
            .delete_session_data(&binding.native_id())
            .await
            .unwrap();
        root.store.create_seeded(original).await.unwrap();
        assert!(authority.snapshot(&root).is_err());
        root.journal(&binding).await.unwrap();
        let new = root
            .buffered
            .as_ref()
            .unwrap()
            .prepare(&binding.native_id())
            .await
            .unwrap();
        let new = Authority::capture(&root, &binding, new, root.warm_epoch().unwrap()).unwrap();
        assert!(new.snapshot(&root).is_ok());
        stop(&root).await;
    }
    #[tokio::test]
    async fn failed_catalogue_restore_revokes_warm_scopes_until_successful_rebuild() {
        let (dir, root, binding, authority) = fixture().await;
        let sessions = dir.0.join("state/sessions");
        let saved = dir.0.join("saved-sessions");
        std::fs::rename(&sessions, &saved).unwrap();
        std::fs::write(&sessions, b"not a directory").unwrap();
        let failed = root.restore().await.is_err();
        std::fs::remove_file(&sessions).unwrap();
        std::fs::rename(&saved, &sessions).unwrap();
        assert!(failed);
        assert!(root.warm_epoch().is_err());
        assert!(authority.snapshot(&root).is_err());
        root.restore().await.unwrap();
        assert!(authority.snapshot(&root).is_err());
        root.journal(&binding).await.unwrap();
        let new = root
            .buffered
            .as_ref()
            .unwrap()
            .prepare(&binding.native_id())
            .await
            .unwrap();
        assert!(Authority::capture(&root, &binding, new, root.warm_epoch().unwrap()).is_ok());
        stop(&root).await;
    }
    #[tokio::test]
    async fn original_creation_witness_checks_all_fields_of_valid_mutation_records() {
        let (_dir, root, binding, authority) = fixture().await;
        let original = authority.snapshot(&root).unwrap();
        for index in 0..5 {
            let mut receipt = authority.creation.clone();
            match index {
                0 => receipt.rpc_id.push_str("-different"),
                1 => receipt.method = "session.other".into(),
                2 => receipt.fingerprint = "0".repeat(64),
                3 => receipt.response = json!({"different":true}),
                _ => receipt.response_event_seq_field = Some("eventSeq".into()),
            }
            let mut session = Session::new(original.header().clone()).unwrap();
            session
                .append_batch(
                    Revision::ZERO,
                    vec![
                        EventData::SessionModelSelected {
                            provider: "offline".into(),
                            model: "model".into(),
                            reasoning_effort: None,
                            context_window_tokens: None,
                        }
                        .into(),
                        EventData::SessionMutationCommitted { receipt }.into(),
                    ],
                )
                .unwrap();
            assert!(
                creation(&binding, &session).is_err(),
                "field mutation {index}"
            );
        }
        stop(&root).await;
    }
    #[tokio::test]
    async fn cancelled_catalogue_restore_keeps_the_new_warm_epoch_unavailable() {
        let (dir, root, binding, authority) = fixture().await;
        let path = dir
            .0
            .join("state/sessions")
            .join(format!("{}.lock", binding.native_id()));
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        lock.lock().unwrap();
        let mut restore = Box::pin(root.restore());
        assert!(futures::poll!(restore.as_mut()).is_pending());
        assert!(root.warm_epoch().is_err());
        drop(restore);
        drop(lock);
        assert!(root.warm_epoch().is_err());
        assert!(authority.snapshot(&root).is_err());
        tokio::time::timeout(Duration::from_secs(5), root.restore())
            .await
            .unwrap()
            .unwrap();
        assert!(authority.snapshot(&root).is_err());
        assert!(root.warm_epoch().is_ok());
        stop(&root).await;
    }
    #[tokio::test]
    async fn overlapping_catalogue_rebuilds_serialize_before_replacing_warm_epochs() {
        let (dir, root, binding, _authority) = fixture().await;
        let path = dir
            .0
            .join("state/sessions")
            .join(format!("{}.lock", binding.native_id()));
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        lock.lock().unwrap();
        let mut first = Box::pin(root.restore());
        assert!(futures::poll!(first.as_mut()).is_pending());
        let initial = root.warm_authority.lock().unwrap().generation.clone();
        let mut second = Box::pin(root.restore());
        assert!(futures::poll!(second.as_mut()).is_pending());
        assert!(Arc::ptr_eq(
            &initial,
            &root.warm_authority.lock().unwrap().generation
        ));
        drop(first);
        assert!(futures::poll!(second.as_mut()).is_pending());
        assert!(!Arc::ptr_eq(
            &initial,
            &root.warm_authority.lock().unwrap().generation
        ));
        drop(second);
        drop(lock);
        assert!(root.warm_epoch().is_err());
        tokio::time::timeout(Duration::from_secs(5), root.restore())
            .await
            .unwrap()
            .unwrap();
        stop(&root).await;
    }
}
