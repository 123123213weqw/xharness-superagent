# Optional canonical logical journal before physical I/O

`BufferedJsonlSessionStore::new(JsonlSessionStore)` is explicit opt-in. The
existing `JsonlSessionStore` defaults and public durable Inbox APIs are unchanged.
There is no new on-disk format/event/required field, dependency, permission,
retry timer, Agent loop or product-side conversation/Inbox mirror.

## One logical source, one physical JSONL

Preparing validates an existing full physical cut and certifies that exact
prefix, not just its revision. A `LogicalSession` then synchronously validates
CAS/events and commits the canonical logical journal without disk I/O. Old cuts
remain immutable. `Store::load` returns this accepted logical cut, including
unflushed input when the backing path is currently unwritable. This is NOT a
physical durability or worker-liveness certificate.

Flush materializes the exact prefix under normal process/advisory locks. It
verifies header/prefix equality, preserves every revision/seq/timestamp and
ordinary batch boundaries, fsyncs data and parent and checks file identity.
Foreign complete prefixes/corruption fail closed, never auto-rebase/overwrite.
After an ambiguous lost flush result, retry reconciles the actual matching
physical prefix and does not duplicate it. Only a genuine parser EOF tail may
be repaired before NEW batches; a flush with no new batches does not alter that
tail. Unknown unterminated fields are corruption, not a permissive EOF fallback.

The backend spawns no writer task. Its normal awaited filesystem operations
retain their own blocking-I/O ownership. Dropping an in-flight flush records an
UNKNOWN durability result; it does not certify success, retract the logical
append or pretend the blocking operation stopped. Retry observes its actual
prefix through the same underlying path locks.

## Ownership and API lifetime

Prepare/create/delete serialize through a PER-ID ownership gate, never a global
map lock over I/O. Initializing owners cannot be missed by deletion. Delete
fences stale mutation handles before physical I/O; successful deletion retires
the old generation, and recreation gets a new canonical owner. Failed or abandoned
deletion leaves the retired logical cut readable until deletion actually succeeds;
abandonment records an UNKNOWN result, never a clean close. Callers still own
and join concurrent delete/API tasks. Idle unpublished
lookup slots are pruned, but initialized journals are not evicted or silently
discarded. Mutex publication checks, not a refcount/stale flag guess, protect
accepted data. The owner intentionally retains prepared full logical journals;
product lifetime/release and memory/performance still require their full audit.

`flush_all` attempts initialized journals without closing admission. `close`
first fences new mutations and then attempts all of them; all typed errors are
returned. Retry can flush a retained prefix without reopening admission. Callers
still own/join their other creation/read/API tasks; this is not whole-runtime
shutdown. A logical handle is weak with respect to its Store owner and contains
no Agent/control/activation lease. Status's durable revision is the LAST
successfully certified prefix, not fresh proof of current file state or health.
Typed faults are retained privately; no payload/raw error logging is added.

Audit/Harness/tool archive ports preserve their original namespace and capture
policy. Physical-only catalogue/history/checkpoint accelerations deliberately
return unsupported/default no-op, requiring authoritative logical-load fallback;
an unflushed view must never publish a false idle/durable recovery stamp.

## Validation scope / product boundary

Real JSONL tests cover hot-before-I/O, exact clocks/batches/CAS/snapshots,
unwritable paths, foreign/corrupt prefixes, ambiguous canceled writes, independent
sessions, close/bulk barriers, creation/preparation/deletion generation races,
unknown lookup ownership, seeded/compressed history and archive capture modes.
Actual actor/Core tests exercise all three modes with the log ALREADY a directory
before logical admission: the receipt can exist, a callback can run, then flush
fails without a model wake; restored storage recovers once, passive injection
stays passive, and a held Store/handle does not pin the actual Agent file lease.

This is canonical-only and NOT installed in the SuperAgent Runtime yet. These
library/actor tests do not implement the entire product AgentDelivery: a
synchronous root-local authorize -> logical commit -> accepted boundary still
needs its actor/prepared-capability rendezvous, object identity/abort/Runner
ownership, dedup/history/producer acknowledgement and actual callback identity
composition. Do not call accepted after merely queuing unvalidated input, or
replace reference pre-flush acceptance with an after-durable callback. Full
Root/Source/API/UI/deployment and final product Rubrics remain unaccepted.
