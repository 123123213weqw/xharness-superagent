# Owned input decisions before Step publication

This is an explicit generic Runtime capability, not a second product AgentLoop.
A ContextPolicy may return an InputTransformer instead of the append-only
StepInputResolver. Both getters default to None. The two optional paths are
mutually exclusive. Existing default input commits remain unchanged.

## Ownership and publication

The durable journal has two new audit events: input/proposed and input/resolved.
A proposal owns an immutable ordered list of User-role input messages and their
actual current Inbox provenance. It is not model history or a genuine human
request. Input claim deletions, TurnStart and the initial proposal use one CAS.
Sources are taken only from the current queue with matching identity and body;
a historical matching ID cannot manufacture provenance. Core retains no second
mutable proposal mirror; Session derives pending ownership from journal events.

An Enter decision selects original indices and/or new non-human ContextInput
values. StepStart, the complete decision and all selected message events are
published atomically in one revision, then flushed before provider I/O. Original
human input remains an exact UserMessage. Original non-human input retains its
source as ContextMessage. New context cannot set source.kind=user. Dropped or
replaced candidates never become UserMessage/ContextMessage model history.
Proposals and decisions project audit metadata without exposing proposed bodies
or a surface append. Context history never establishes human approval authority.
The UI audit field is proposedStep, not step: a proposal is not a started Step.
Using active-step coordinates here makes history replay invent a ghost Step.

Reject closes the turn as Blocked without StepStart or a provider call. An empty
first Enter closes as Completed with no generation. Subsequent empty proposals
may continue an existing tool-driven turn. Preparing, pausing or steering before
StepStart does not consume a step ordinal or a hard step-budget unit.

## Concurrency, cancellation and failures

A CAS loser discards its stale decision and recomputes against the current cut;
it does not silently claim new arrivals. Callback cancellation is scoped to that
attempt. Preparation may run multiple times: no untracked external effects or
publication acknowledgement is permitted in the callback. Tool capabilities are
real registered ToolSpec Arcs, not reconstructed metadata. Prompt resolution
precedes transformation. Explicit System messages/injections are rejected in
this opt-in mode; system content belongs in PromptAssembly/PromptResolver. This
restriction does not change legacy System handling.

Append receipts install turn/step ownership before flush, so an fsync failure
still settles the committed lifecycle rather than inventing an unstarted turn.
Cancellation abandons the owned pending proposal. Cold interrupted recovery
records Abandoned and Interrupted without replaying the callback or promoting
raw candidates to human/model history. Exact original proposal audit data
remains durable. Automatic requeue/retry policy and full ActorEpoch recovery are
not accepted by this component; those remain product-level alignment work.

## Terminal contract and deployment

Blocked is distinct from Completed, Cancelled and provider failure, including
legacy Core driver projection and durable Host observer recovery. Goal policy
stops automatic continuation with InputRejected rather than falsely completing
or retrying a provider error.

The current generated terminal contract is v2. The frozen v1 schema/fixtures
must not be rewritten. Old producers remain readable, and unchanged default
producer values remain readable by v1 clients. The new Blocked kind is explicitly
NOT readable by v1 and must fail closed. Upgrade readers before opting into input
transformation; this change does not enable the capability in existing services.

## Evidence boundaries

The seam uses existing scalar Rust Message/String and serde_json::Value types.
It does not yet provide lossless isolated-UTF16-surrogate transport, preserve all
SDK candidate-envelope fields, or install the product skill catalog. Component
Rust tests cover durable Actor claims, cold JSONL, pause/steer/cancel/CAS and
append-receipt/flush failures. They do not replace the original SDK AgentLoop
oracle, full Native model wire, Windows, rollout or final behavior-Rubric proofs.
