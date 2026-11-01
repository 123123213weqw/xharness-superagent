# Explicit non-interrupting input delivery

This addition is opt-in. `DurableAgentHandle::steer` and Runtime
`admit_steering` retain their interrupting semantics and old receipt policy.
`followup`, `inject`, `admit_turn`, ordinary inbox append/prepend, automatic
failure/stop gates, and activation-without-wake retain their defaults.

- `follow_step` admits input for the nearest model step and requests wake. It
  uses `InjectionMode::NextStep`, not interrupt/steer. At the actor boundary,
  an already-cancelled turn redirects the new waking item to next-turn.
- Runtime `admit_follow_step` and `admit_followup` install input observers
  BEFORE admitting and waking the actor. Unsupported runtimes fail closed;
  they never silently fall back to interrupting steer or no-op admission.
- `admit_followup` is explicitly waking next-turn delivery; old `admit_turn`
  remains durable queue-only admission. Fresh explicit admissions replace stale
  cached subscriptions after a removed/unconsumed identity is re-admitted.
- Producer idempotency remains a producer responsibility: pending/consumed
  receipt checks precede these low-level mutations. A mutation error is not a
  success receipt; owner recovery must re-flush/re-subscribe before waking.

## Representation and authority

No new Session event or required `InboxMessage` field is added. An initial
required-field draft was rejected because it broke existing Rust struct
literals despite backward-readable JSON. The existing opaque transport
metadata envelope instead carries a strict, versioned `xharness.delivery`
marker, its exact original metadata (including absent versus explicit null),
and the original effective producer in `source`.

This is scheduling intent, NOT human provenance, permission or resume
authority. `is_user_authored` is unchanged. A waking internal next-step input
is runnable only behind an already-open automatic gate. A real user prompt
still follows the existing atomic resume-and-claim path. No fake Goal, empty
prompt, global resume, extra conversation store or second Loop is used.

Only the actor's explicit new command writes this marker. Legacy actor/inbox
admission APIs strip a copied valid marker, so replaying arbitrary input
metadata through those APIs does not create wake intent. Content replacement
preserves the existing item's intent; removal removes it with the input.
Malformed/unknown/inconsistent markers are opaque, not wake authority. Raw
Store mutation is an existing trusted journal authority, not a public model
or guest API. A reserved-namespace collision in foreign/corrupt old metadata
is not presumed compatible without explicit migration evidence.

The original Session schema and Rust struct shape stay byte-identical.
An old reader can decode new items and preserve provenance; because it does
not know the new intent it conservatively treats non-human next-step input
as passive. That is safe parking, not equivalent automatic progress or a
completed rollback/deployment gate.

## Ownership and verification

The Session JSONL inbox remains the only durable input authority. Existing
claim, user/message, receipt, CAS/flush and input-scoped observation machinery
owns delivery. No SQLite mirror or global retry task is introduced.

The new tests use the real actor, LoopEngine and journal with explicit offline
provider/barrier probes: idle wake, passive non-wake, no human/Goal forgery,
held-model non-interruption, next-step grouping, closed-gate parking/resume,
cold replay, edit/copy semantics, end-boundary repetition and accepted-cancel
redirection while the old turn's terminal append is deliberately held.
Metadata roundtrip tests preserve absent/null/nested source and old struct
construction. Existing steering, failure, dispatch, inbox, Host and persistence
tests remain required. Windows, arbitrary provider/inter-owner fault ordering,
production transport, model/prompt changes and full product lifecycle are NOT
proven by these finite tests. No commit, push, merge or deployment is implied.
