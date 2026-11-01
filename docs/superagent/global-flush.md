# Managed native global persistence barrier

Reference prototype commit 6337fc823ce20251c8de8085948165c81aaa701e calls the
service flush immediately after a successful accepted callback. It does not
first await one input-specific persistence acknowledgement. The concrete JSONL
backend published as 0.2.1-alpha.2 snapshots its Map values at invocation and
sequentially drains/flushes each original writer. It continues on errors,
returning AggregateError even for a single failed writer, in registration order.

The prototype dependency lock contains the abstract service, NOT that JSONL
backend. The backend probe in `conformance/sdk-flush` is explicitly supplemental
matching-release evidence, not a changed prototype pin or full SDK filesystem
oracle. It executes the actual distributed tracker body and locked abstract
closed-error class with test-owned handle ports; source hashes, verified npm
integrity and MIT license are retained.

Canonical `PendingInputAdmission::begin_persistence()` releases the owning
actor's producer boundary synchronously, before returning an unpolled future.
Native Scope uses that prefix, then AgentDelivery invokes global flush before
awaiting either outcome. Both are observed with join, never try_join/cancellation
on the first error. A global failure is returned as the original typed ordered
NativePersistence aggregate, not replaced by the actor's individual error. An
accepted callback that throws still skips the final flush, preserving its exact
failure and actor ownership of already accepted input.

A root-local registry contains only id + canonical LogicalSession capabilities
of successfully bound managed Sessions. It has no Inbox/event copy or execution
handle/lease. Repeated ensure does not reorder a live writer; a closed storage
generation is replaced at the end. The flush call snapshots before returning
its future and polls the same owned future once for its eager prefix, then
returns it to the existing owning Runner. No detached task or timer is added.
Each writer drains through additional logical revisions appended during its
physical write. Healthy later writers are attempted even after earlier errors.
Read-only health/history/unmanaged loaded slots are not writer registrations.
The generic buffered Store keeps its existing id-sorted barrier and shutdown.

Real FS tests invert id order (z before a), hold an actual OS file lock, verify
late/unmanaged exclusion, repeated registration, retired/recreated generations,
concurrent append cuts, typed ordered failures and continuation. Actual native
integration proves an earlier healthy managed journal progresses while a later
send's own fsync is blocked, and two ordered global failures do not abandon a
later healthy journal. The former test fails on the old wait-own-first bridge.
The old one-writer corrupt-file assertion now deliberately requires a one-error
global aggregate, corroborated by the independent SDK probe, rather than merely
accepting either old or new error type.

## Still not final acceptance

Registration currently follows successful trusted ensure, not SDK reservations
before creation/open I/O. Native live-object disposal and catalogue restore are
not universal writer close/drain; closed dirty generations conservatively fail
instead of hiding unproved durability. Initial allocation races, all writer
claims/retirement/teardown, continuously changing drains, exact per-input failure
retry/scheduling interactions, permanent synchronous Agent facade, bounded live
events and the complete Root/Sources/API/UI product remain open. These tests do
not count as passing any of the 96 final rubrics. No production/default
permission/paid model/real Source changes are enabled.
