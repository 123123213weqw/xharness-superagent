# Concurrent logical admission through the sole worker

Prepared input no longer waits for the preceding prepared operation's physical
flush to settle before the actor can offer the next logical admission. The
canonical Session/Inbox remains the one source of accepted input. There is no
new Agent loop, persistent input mirror, retry timer or independent writer task.
Legacy physical-only Stores still decline the explicit prepared capability.

## Owned durability and delivery stages

The same DriverWorker owns a FuturesOrdered collection of transient operation
stages. Releasing a synchronous producer callback queues a generation-bound
flush stage and releases the Inbox mutation gate. The worker polls those stages
alongside commands, capacity waits and active Core events. A live Core command
is another owned stage rather than a blocking await in the command handler; its
private control handle addresses only the original exact run, never a later
turn or replacement worker. Nothing is spawned to drive these input stages.

Stage publication is ordered. A flush result below the actual append receipt's
revision is rejected; a later successful prefix flush does not erase an earlier
caller's typed failure. The existing completion guard assigns an abandoned
failure exactly once to the remaining owner, without payload/raw-error logging.
Normal worker exit drains every admitted stage. The existing supervisor grace
and forced-cleanup classification remain in charge of a stuck owner. A dropped
future or a notification-channel send is not proof of persistence success.
Graceful worker joining is also not a claim that every storage operation
succeeded: producers retain their typed outcomes and Store faults remain visible.

A new turn/claim is not started while these stages remain outstanding. This
preserves the existing kernel's execution/durability boundary; it is NOT proof
that every scheduling/model-request timing matches the SDK machine. Other
legacy actor handlers and Core reconciliation can still await their own I/O.
Do not call this a permanently warm, synchronous Agent facade for all phases.

## Original generation and exact input origin

The prepared LogicalJournal supplies an explicit async flush bound to its
original Store Entry, not `flush(id)` on a possibly recreated Session id. Its
synchronous closed observation concerns storage mutation lifetime, not Agent
liveness or execution authority. Reads can retain a historical cut after
retirement; the retired capability cannot certify a new owner's journal.

Before model-facing delivery, the worker replays the existing borrowed Inbox
projection against the SAME immutable canonical Session cut. It checks the
original append revision and insertion object, not only an equal id/body. An
input removed and identically reinserted has a different origin; an already
consumed input must not be injected again. The accepted append can still be
published as a historical fact, without waking or delivering a stale origin.
This check is not a claim of a new recall/cancellation contract after Core has
already accepted a live control operation.

## Validation and remaining product scope

The strict concurrent-admission assertion first failed against the serial
implementation. Test-owned flush barriers mirror the original AgentDelivery
experiment, while actual actor/Core/JSONL tests also use a real OS advisory lock,
active model requests, typed failures, receipt regression, shutdown, lease
release and removed/reinserted/consumed origins. Three modes in one test remain
one test case, not three. A generation-reuse test exercises the actual buffered
Store capability. Required Windows and full workspace/release acceptance are
not implied by the affected-package tests.

The production AgentDelivery facade, accepted-throw/RunnerDrop/abort/current
object/dedup/history/global flush composition, all actor-phase timing, consistent
CreationAuthority/Runtime/tools/Models Store wiring, per-session release and
memory/scale remain work. These canonical features are not installed into the
SuperAgent Runtime here. Do not call a passing kernel slice final product
alignment or silently weaken legacy gates/default permissions to make it pass.
