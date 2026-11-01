# Optional request prompt resolution

The `ContextPolicy::prompt_resolver()` getter is synchronous and defaults to
`None`. Existing policies take the old path with no new async polling or control
boundary. An explicit resolver implements the separate `PromptResolver` trait.
No required method or field was added to public LoopRequest/Host turn requests.

Core supplies the owned Session/run/turn/step/provider scope and the original
Turn's immutable base assembly. Before each normal generation preparation (and
manual compaction preparation), Core resolves and replaces only its own ephemeral
System prefix, then snapshots tools, prepares context, counts tokens and writes
the RequestHeader. The assembly carried into that header is the one used on the
Provider wire. Neither user-message events nor the stored conversation is
rewritten. An unowned explicit System message is rejected, not silently removed.

Resolution is interruptible through the existing command envelope owner. Stop,
pause and steering cancel the child token and abandon the pending resolution;
steering/resume re-enters normal preparation. A completed token is also cancelled,
so a resolver must not treat it as a lifetime/activation capability. None retains
the current assembly; Some supplies a replacement, never a stacked Turn base.
No execution lease, Inbox, second Agent loop or activation handle is created.

`PromptResolutionError` preserves the original shared typed cause. Its Display
and Debug are constant/log-safe. Core holds that cause until the existing textual
LoopResult/journal failure boundary, records failure there once, and does not
format the source into persisted diagnostics. It is not a typed-error persistence
schema and does not add a public structured error field to LoopResult.

`PromptAssembler::assemble_preserving_content` is an explicit opt-in for already
structured product text. Only content trimming changes: id/version validation,
duplicate checks and whitespace-only rejection remain. Section and system hashes
bind the actual untrimmed bytes. Default `assemble` remains unchanged.

## Limits / integration obligations

This is an optional kernel seam, not installation of SuperAgent Sessions, role
prompts, current Mandate, live skills, Root or a native model product profile.
The resolver owner must prove its own managed Session binding, lifecycle and
side-effect/error ownership. Missing/closed owners must not silently substitute
a prompt preview or a static Turn-start snapshot.

The hook is a **preparation** boundary. It is not re-entered by transport-level
reception retries inside one model round, nor by the internal summary provider
calls within compaction. Those reuse their separately owned prepared request.
Manual-compaction outer preparation is not proof of dynamic summary-prompt parity.
These exclusions are explicit; no all-phase/per-HTTP-call parity or release
approval is claimed. The 96 final SuperAgent Rubrics remain unaccepted.
