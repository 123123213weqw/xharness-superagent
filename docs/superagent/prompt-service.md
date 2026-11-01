# Versioned product Prompt service

`prompts::Prompts` is the production SQLite-backed Prompt configuration service,
not a test-only implementation or a second Session store. The local SQLite
owner supplies `Inputs` for current limits, environment, Feishu availability and
time. Those callbacks are not selected by model arguments. No default Native
role routes, permissions, private Host binding or old entrypoints change.

The service implements the pinned prototype's 15 built-in parts, literal packaged
Source skills, team sections, overrides and base hashes, role assembly, one-pass
placeholders, generated environment and final orchestrator Policy. Overrides
retain leading/trailing bytes; formatting does not use PromptAssembler's trim.
JSON schema errors aggregate in definition order with nested record-key issues;
`__proto__` is ignored exactly where the reference parser ignores it. Serialized
configuration budgets count UTF-16 code units, not UTF-8 bytes or Rust characters.
Input object/skill/override order is preserved without changing serde_json's
workspace-wide feature flags. Packaged text uses the reference's paragraph and
ECMAScript whitespace normalization. File reads deliberately match Node UTF8
replacement for malformed bytes while returning actual I/O failures.

Saving validates before CAS, checks the current revision inside the transaction,
keeps immutable revision zero and edited built-in texts, then writes the new
snapshot and pointer atomically. Save failures roll back all of those records.
The configuration history includes skill/override changes in reference order,
preserves prior bases, reports retained/unknown/removed defaults and legacy
comparability, and supports restoring an earlier configuration as a NEW revision.
Settings revisions and Session histories are not rewritten. Returned values are
owned snapshots; preview does not create durable revisions.

## Evidence

The unchanged original `prompts.spec.ts` runs both against the original class and
against the production Rust class plus production Rust Store. The synchronous
Store bridge only transports calls, callback bodies, path resolution and original
error constructors; it contains no prompt algorithm. All get/set/history calls
of each fixture share the same Rust SQLite connection, not a mirrored JS store.

A separate test file uses NO module mocks: actual original Prompts/Node SQLite
are compared with Rust Prompts/SQLite. It checks all 15 texts/base hashes, 180
role/environment/policy composition rows, 13 save operations, 77 schema/semantic
error inputs and 8 real file byte inputs. This independent comparison also
avoids using the target's file loader as the expected built-in text oracle.
The schema tests first failed on 44 original inputs, then on two additional
prototype-key cases; the production parser was corrected, not the goldens.

Twelve Rust test declarations include one subprocess fixture helper. The parent
crash test exercises three abrupt exits: while the clock is read after baseline
records were staged, after an inner save but before outer commit, and after a
durable commit whose response is lost. Reopen proves uncommitted records absent,
committed records durable and a stale retry refused. Other tests exercise SQL
pointer-trigger rollback, restart/history/default restoration, reentrant CAS,
UTF16 limits, exact override bytes, aggregate errors and preview/skill gates.

## Required next integration

This service is not yet installed into NativeSessions' per-model-request path.
A preview or a configured value is NOT evidence that it reached a provider.
NativeSessions still accepts its existing optional caller PromptAssembly. The
next integration must bind trusted Session/role/ticket identity, current revision
and Mandate before EACH request (including later Steps/retries), record matching
wire/audit metadata and per-Session revision history, and replace/register skills
with exact descriptions/resource bases and lifetime ownership. It must preserve
host/repository AGENTS behavior and both tool-registration gates.

Do not freeze the configuration at Turn construction, move a non-Send SQLite
connection into a provider thread, identify a Session by caller-controlled
system text, or silently trim override bytes. A canonical opt-in per-request
hook may be needed; generic runtime changes must go upstream then through a
controlled sync. External-origin notice/effect delivery, Prompt tools/API/UI,
Root and all remaining plan requirements are still open. All 96 final Rubrics
remain not_run; component tests do not establish full model-input acceptance.
