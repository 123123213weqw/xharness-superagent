# Warm creation authority / native concurrency (progress, not full SDK facade)

The previous native bridge validated the scoped creation proof through the Host
on EVERY input. That path flushes the current authoritative journal revision;
it can wait on another producer's unrelated pending prefix (and a real OS lock
on an already warm Session). A new native regression first failed BEFORE the
first accepted callback. The red evidence is retained, not ignored.

## Derived capability

Trusted `NativeDelivery::resolve` still uses the original Host creation proof
and durability barrier. It captures a private, non-serializable witness for that
owner, original sole exact receipt, CWD, role binding, buffered logical generation
and current catalogue epoch. This is not a receipt for future input durability,
not raw-ID authority, not an activation/control lease and not a second journal.

Later native preparation uses that SAME logical journal without an unrelated
creation-proof flush. Admission validates owner/CWD/role/receipt/storage lifetime
and the current epoch again. A short epoch guard spans the actual logical
`PreparedInputAdmission::commit`, never user callbacks, awaits or physical I/O.
Catalogue retirement and logical commit therefore have one synchronous winner.
The original automatic/failed dispatch, producer provenance, selected durable
route, permission and Setup/shutdown gates remain in place.

Public `NativeSessions::snapshot`, `ensure` and new `resolve` keep the original
Host proof/flush semantics. A corrupt physical file can make a fresh authority
lookup fail even though a previously established logical capability is readable;
new input still requires its own successful fsync before acknowledgement/use.

## Recovery and identity

A catalogue rebuild retires the old epoch before I/O. Failure/cancellation keeps
the replacement unavailable until a successful rebuild. Rebuilds are serialized
by an owned lifecycle mutex, not by a new input/Agent loop. No authority-state
mutex or execution handle is held across restore I/O. A new trusted resolution
replaces a retired live object; exact Rc identity prevents an old disposer from
evicting its successor. Old logical capabilities cannot address an identically
recreated physical Session ID. Future owned archive/workspace-rebind APIs MUST
retire the managed binding/epoch; the private creation-only Host is not exposed
as a general external control backend.

## Actual evidence scope

* Native Queue/Steer/Inject × idle/held-live-Core × current-thread/2-thread Tokio
  runtime. Both real accepted callbacks run while an actual JSONL OS lock holds
  physical persistence; bytes stay unchanged, no extra model call/cancellation,
  then release and acknowledge. The final physical history now asserts one
  UserMessage per consuming input, or zero for idle non-waking Inject, across
  these twelve subscenarios (not acceptance for all other SDK phases).
* Reentrant Setup revocation before commit, restore epoch replacement and stale
  current-object disposal, unchanged dirty-prefix durability, owner/role/CWD
  rejection, identical-ID storage generation retirement, valid-record receipt
  field mutations, actual failed/cancelled/overlapping catalogue rebuilds.
* Original physical `open` and old NativeSessions/Delivery/Models cases remain.
  No paid model or real Source writes, production switch, permission broadening,
  auto-work or new account configuration.

These are component/controlled native-composition contracts, not final Rubrics.
A per-call actor preparation await STILL exists; direct arbitrary SDK Agent
methods outside that scope are not a complete warm synchronous facade. Native
scope finish now uses the controlled persistence-only certificate rather than
waiting for actor control completion; global flush cut/order/start parity is
still not established. Whole-history reconciliation is not a
bounded incremental live SDK subscription. Prompt revisions/skills/Mandate/real
human authority, exact20tools/Root/Source/API/UI/deploy/rollback and the original
interrupt/A-to-B/migration/time-zone/Feishu gaps remain open. All96 final Rubrics
remain not_run and the full M0–M7/40-task goal stays ACTIVE.
