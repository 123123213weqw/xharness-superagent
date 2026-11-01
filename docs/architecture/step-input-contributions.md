# Trusted durable Step inputs

A ContextPolicy may explicitly expose a StepInputResolver. The default is None:
legacy policies introduce no async calls or new events. With a resolver, Core
first resolves its prompt (the SDK alpha.2 assembly precedes agent/pre-step), then
provides an immutable pre-StepStart Session snapshot, current turn/step, newly admitted user
inputs and their actual consumed-inbox provenance, and the current ToolSpec Arc
identities. Unknown input sources stay unknown. Unclaimed queued inputs are not
in this batch. This is distinct from disposable ContextPolicy::prepare.

Returned user-role context and complete source objects are validated and written
together with StepStart in one strict snapshot-revision CAS. An empty contribution
still commits StepStart. A conflict discards
the uncommitted contribution and re-resolves from a new snapshot. Cancellation,
pause and steering abandon pending resolution without a StepStart or consuming
a Step ordinal/budget. The opt-in prompt assembly is retained for that Step
across admission retries; legacy no-resolver ordering remains unchanged.
Completed writes are joined and
flushed before provider I/O; an append/fsync error is not an acknowledgement.
The original typed error is retained until the public textual run boundary; a
resolver owner may retain its shared cause, without persisting private diagnostics.

Context messages project into the durable user-role transcript but never become
a genuine user request, a new inbox receipt or a human provenance category.
Cold loads retain exact content and source; history indexes must index these
messages. Existing user/message constructors, event bytes and ordinary input
commit/receipt behavior are unchanged. The new event tag requires an upgraded
reader only when the optional capability is enabled; old readers must fail closed.

This is infrastructure for the product skill catalog and explicit invocation,
not a claim that those listeners, UTF-16 edge cases, final Root or Rubrics are
complete. No business SQLite Session mirror or second agent loop is introduced.

The contribution API appends owned context; it does not yet implement a general
pre-step decision transform that drops or replaces an already admitted candidate.
The SDK catalog's existing-candidate branches and full UTF-16 transport remain
required product integration work, not waived by these infrastructure tests.
