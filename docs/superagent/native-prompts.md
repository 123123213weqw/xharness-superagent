# Native request Prompt composition

`native_sessions::prompts::channel` creates a bounded, single-native-owner client
and a local PromptRuntime receiver. `NativeSessions::open_prompted` explicitly
installs the resolver with buffered JSONL. The existing open/open_buffered APIs,
selected role routes, WorkspaceWrite default and setup gates are unchanged.
Construction creates no SQL connection, Source poller, model request or task.

Root must own and poll PromptRuntime::run on its existing !Send SQLite owner,
with the same production Store and deployment-owned RequestInputs, and join the
terminal PromptReport. No unsafe Send, independent Store open/migration, second
conversation, Inbox or activation handle is introduced. Report failures retain
the original shared typed cause; Debug prints counts only. A failed reply remains
owned even if the requester disappears. Cancelled queued work does not bind.

After successful ensure, the private policy binds the actual creation receipt,
owner, role and cwd. Worker allocation additionally calls bind_prompt_ticket with
the accepted Ticket id and matching reference logical id `worker-<ticket>`. Each
request verifies the private binding, creation receipt, current JSONL cwd and
setup gate before reaching SQL. An endpoint cannot be attached to another owner.
An unavailable/closed owner fails before provider I/O rather than substituting a
preview, generic persona or a Turn-start configuration snapshot.

The local operation reads current production Prompts for every generation
preparation. Main replaces the supplied generic base completely. Worker keeps
the supplied Host/AGENTS base (including its existing optional dynamic resolver) and appends the exact product section,
current Ticket Mandate, executor variables from current Settings/Ticket and Source
variables from the trusted deployment callback using the current origin Trigger.
A Ticket deleted after decoration uses the reference's default environment/no
Mandate fallback. A Worker whose Ticket was absent at decoration keeps its generic Host prompt until an explicit bind/ensure retries decoration. Merely creating a Ticket later does not register a policy section. The current dynamic Host base callback still runs and its result is preserved.
Base callbacks receive the original Turn base and run even for Main, whose complete selection discards their persona output. Literal interpolation remains disabled. Live revision history is recorded under
`session-prompts:<reference logical id>`; preview and queue admission do not bind.
Binding precedes environment composition, as in reference promptFor, so a later
composition failure can leave the revision diagnostic without a provider call.

The ordinary JSONL replay deliberately omits plaintext request input/system.
Persisted Prompt fingerprints and the physical Harness snapshot reader prove
which bytes reached the Provider boundary without changing that privacy default.
Normal replay must not be mistaken for the full snapshot reader.

## Verification and remaining integration

`remote-validate.sh native-prompts-contracts` exercises actual Native owners,
local production SQLite, offline Providers and physical JSONL/Harness reads, then
compares emitted requests against original Prompts, mandateInstructions,
environmentOf/ticketEnvironment and the real lock-pinned DSH renderPrompt. The
SDK alpha.2 package follows the pinned prototype lock; it is not a claim about a
newer DSH release. No reference test/source is edited or mocked.

This is an explicit product composition, not a production Root entrypoint. Actual
filesystem AGENTS discovery must be supplied by Host/Root; the tests preserve a
supplied base. Live skill registration/disposal, resourceBase/UTF16 descriptions,
role tool guards and all20 tools, complete SourceRegistry/deployment wiring,
Assessor/Executor/Root/API/UI and all-phase/lifecycle requirements remain open.
The kernel hook still does not re-enter transport retries or internal summaries.
All96 final Rubrics remain unaccepted. External exhaustive matches of the public
Error enum may need the additive NativePrompt, NativeBasePrompt and NativePromptAssembly cases; no release approval is given.

## Acknowledged initial decoration and passive lifetime

Main ensure and explicit Worker Ticket binding now await a bounded local-owner
operation. Root must poll the SAME !Send SQL service while awaiting these setup
calls; the service does not spawn or migrate connections. Recognition and the
initial current revision are read at decoration, with no Session revision KV,
model request or wake. A Worker already decorated remains decorated when its
Ticket is deleted before its first model request, using default environment and
no Mandate. Initially absent Tickets are skipped; only explicit binding/ensure
retries the skipped decoration, just as original Sessions.decorate/ensure.

An already loaded actor is associated at ensure; a first actor can adopt its
setup decoration. Passive watch-channel identity distinguishes actor replacement
from changing activity counters or worker Starting/Unavailable/Ready transitions.
A different channel cancels the old scope; Closed/disconnected watch invalidates
it. No control/activation lease is retained. Full close/ABA and skills-service
restart/disposal parity is still not accepted. Shutdown first settles Core's
cancellation, then closes decoration scopes. Immutable Session reads are dropped
before dynamic base/SQL waits. Owner preflight failures return the same original
shared error held by PromptReport, not a synthetic stopping error.

Seven Native integration and seven private owner/epoch declarations now pass.
Four independent oracle declarations compare nine actual Provider inputs and
physical Harness snapshots; four inputs exercise the ORIGINAL Sessions class's
decoration/skip/retry boundary. Cordis hooks, skills registration and unrelated
tool adapters in that oracle are explicit fakes, not the real skills service or
skill-tool plugin. No original module function is mocked/replaced for prompt
composition. Four production semantic mutants are rejected by unchanged
assertions; source is byte-restored before positive tests and full validation.

Live skill scopes/tool/model catalogs, packaged resourceBase and exact UTF16
boundaries are NEXT product work, not implemented by this acknowledgement.
