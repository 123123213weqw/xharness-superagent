# Prepared synchronous logical input boundary

This is an explicit opt-in kernel capability, NOT the complete product
AgentDelivery facade. Physical-only Stores decline preparation with a typed
error. Their existing append/flush APIs and default behavior do not change.
Session events, persisted batches, revision/CAS, provenance, permissions and
execution leases keep their existing meaning.

A Store may prepare its ONE canonical `LogicalJournal`. Snapshot and append are
synchronous, perform no physical I/O, validate ordinary Session/CAS semantics,
and reject mutation after their owner closes or retires. The buffered JSONL
implementation supplies this capability; it is not another product Inbox.

`DurableAgentHandle::prepare_input_admission` rendezvous with the sole actor at
its real command boundary and holds its per-Inbox mutation gate. It returns a
single-use capability before any input is appended. A root-local caller can now
check signal/current object/authorization, read current pending, commit, and call
accepted without an await. Commit reads the fresh canonical cut (not a prepared
revision guessed earlier), validates the existing Inbox rules and appends once.
FollowStep consults a read-only cancellation observation at commit time, marks
wake intent only for the selected step target and never interrupts the model.
The observation grants no cancellation/control rights or run/Agent lease.

The shared retirement fence and logical append are one critical section.
Shutdown or worker retirement cannot invalidate the capability and then allow a
late append. If commit wins, the actor still owns the append after release/drop
or shutdown, and attempts its normal flush before event publication/delivery/
wake. A notification channel is not a durability certificate. The existing
completion guard preserves typed failures and reports an abandoned failure once
without payload/raw error logging. Dropping before commit releases with no input
or wake. Never hold a preparation while awaiting another command on the same
actor. Preparation, receipt and cancellation observation do not retain an Agent
activation or execution lease.

## Remaining product and concurrent admission work

Reference AgentDelivery must retain its synchronous authorize -> pending check ->
input -> accepted sequence, including thrown-value identity, deduplication,
object identity, abort checks, history loader and final flush. The kernel tests'
local callbacks are NOT the production AgentDelivery facade. A native per-send
adapter must also release/observe its committed operation when accepted throws,
when the result waiter is dropped, or when the owning Runner terminates. Do not
leave a cached preparation or receipt pinning the actor after these exits.

Prepared operations now use sole-worker owned durability/delivery stages; see
`concurrent-logical-admission.md`. A following prepared logical admission can
proceed while a prior prepared flush is pending, without moving the Inbox truth
or hiding a typed failure. Generation-bound capability flush prevents an old
receipt from certifying a recreated Session id. This does not yet prove every
actor-phase/model-request timing or complete production callback composition.
Full shutdown/join, per-session release, memory/scale and consistent Store
installation in CreationAuthority/Runtime/tools/Models remain mandatory. These
source-only features are not installed into Runtime by this change.
