# Observed input append boundaries

`DurableAgentHandle::begin_input_admission(mode, input)` is an explicit, opt-in
logical append boundary, **not** a successful producer/durability acknowledgement.
Existing followup/follow_step/inject and public Inbox APIs still flush before
returning. There is no new Session event/required field, second Inbox or model
loop, retry policy, permission change, automatic setup, or general control change.

The actor chooses Followup/FollowStep/Inject at its actual ordered boundary,
including redirecting waking step input after accepted cancellation. Existing
opaque provenance is preserved; only the actor sets waking-step metadata. It
validates and appends through its canonical DurableInbox, offers the actual
AppendReceipt, then waits for producer release before **its own** flush and
Core injection/wake. PendingInputAdmission must be finished or dropped before
awaiting another actor command. Shutdown releases held boundaries.

Another existing Core/Store writer can flush this same journal prefix while a
receipt is held. This is NOT a global fsync/visibility freeze. The actual Core
integration test allows that flush while checking that the held nearest-step
input is not consumed until the actor releases and sends its control message.

Dropping a receipt/finish future does not undo the logical append or detach a
writer. The actor still owns completion. Typed InputAdmissionError retains the
original Inbox/Store/Core error. If the API caller abandons a failure before OR
after it is sent, exactly one owner publishes a static error event, never input
contents, caller IDs, or raw backend details. The receiver closes before draining
on Drop, preventing a send-success-after-empty-read race. The completion receiver
retains only an event sender, not an activation/control/physical lease. Tests hold
a receipt across shutdown, handle drop and actual filesystem lease reacquisition.

## Product compatibility boundary

This is **not yet installed in the SuperAgent product** and does not by itself
provide its full AgentDelivery authorize/accepted/finalflush semantics. A physical
append failure can happen before this receipt exists. The locked published DSH
Session instead commits its logical log synchronously, contains event-observer
failures and buffers persistence asynchronously. The original AgentDelivery then
calls accepted before its final flush. Real filesystem experiments with the
original AgentDelivery + actual SDK Session + a test-owned persistence port show
accepted still runs after an append-observer I/O failure, while final flush fails
and a retry retains one pending input. That is not a whole SDK machine or shipped
persistence-backend execution.

Full parity therefore still needs a correctly owned logical-before-I/O Store
boundary (or equivalent), atomic authorization/current-object handling and the
native product callback/flush/abort/accepted composition. Do not invent a product
Inbox, call accepted after merely enqueuing an unvalidated command, or equate this
receipt with durable success. Keep this difference open until direct end-to-end
fault/authority evidence proves it. Canonical source validation does not authorize
an unreviewed Runtime backport or declare final product Rubrics passed.
