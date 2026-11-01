# 当前产品范围优先

2026-11-01：[产品范围](../docs/superagent/product-scope.md) 取消全域 JS 语言
兼容。下文是历史研究与差分工具说明，不是当前产品完成声明或全量发布门禁。
`rubric.json`/`backlog.json` 保留原始计划，取消的边界不计通过。

# SuperAgent conformance

This directory is an executable comparison harness, not the new product.

- `baseline.lock.json` pins all three repositories and the unmodified MIT-licensed reference files.
- `rubric.json` keeps 96 requirements unverified until target-side evidence exists.
- `reference/upstream` contains the original reference commit, including its MIT license.
- `reference/driver.mjs` invokes the original Engine and Policy, with deterministic clock and recording ports.
- `scenarios` holds synthetic inputs only. No production credentials, model calls or external writes are involved.
- A passing reference self-test is NOT evidence of Rust product parity.

Use Node 24 with experimental TypeScript transformation. Install only the pinned conformance dependencies with scripts disabled. All Rust compilation and tests, including compilation invoked by shell/Python helpers, run remotely on WZU_Server after syncing current source. Do not run scripts/sync-upstream.sh locally: it invokes cargo check.

Reference tests:

```sh
cd conformance
npm ci --ignore-scripts
npm run test:oracle
```

The npm lock was generated on the remote execution host and returned to this checkout. Full reference-application tests require their own locked DSH dependencies; the runtime-free domain adapter does not replace those tests.

## Execution and evidence boundaries

Run `scripts/superagent/remote-validate.sh oracle` from the repository root to synchronize current source and execute reference self-tests remotely. `rust`, `reference`, and `contracts` select the other gates. The wrapper compiles Rust only through SSH to WZU_Server. Evidence is kept in `.alignment-evidence/`, which is neither committed nor synchronized.

`core` runs the current Rust component tests plus primitive, record and policy differential matrices. `policy-contracts` replays the unmodified original policy assertions through the Rust policy boundary. SourceRegistry and migration controls in that file still run in TypeScript and do not constitute Rust source/migration coverage. Partial cast fixtures are hydrated only in fields the policy functions do not read. Neither component mode replaces the full workspace gate or product end-to-end acceptance.

- `reference/resolved-snapshot.json`: original default settings, rules, limits, error/reason catalog and prompt text.
- `reference/runtime-contracts.json`: original Runtime registration of 14 main tools and 6 worker tools, their complete parameter schemas, 25 API declarations, resources and prompt parts. This uses the original mocked host fixture, not a real provider envelope.
- `reference/assertion-inventory.json`: TypeScript AST declarations/assertions for all 59 reference test files. Requirement-level assertion review remains pending.
- `primitives.mjs`: exact original-versus-Rust comparisons for typed model round-trip, stable keys, stage precedence, retry, and read receipts. It does not test the product Engine or grant parity to a complete rubric.
- `records.mjs`: follow-up ownership, thread-context reachability, settlement, automatic-input classification and public error/reason wording.
- `policy.mjs`: deterministic settings, validation diagnostics, admission cells, bot lists, revalidation, effects and assessments against the original functions. Identity encoding also checks insertion order, numeric property names and binary64 number formatting. These are scripted SourceScope ports, not real source adapters.
- `compare.mjs <reference.json> <target.json>`: compares JSON object fields structurally, array order and values exactly; it does not remove IDs, timestamps or authorization fields. Only JSON object-key presentation order is ignored.

Original reference files must remain unchanged. The reference's existing failing Feishu stream-health test and the target's existing deployment-dependent sandbox failures are retained as failures in the execution status, not patched away or counted as passes. Do not run real source/model writes without the corresponding explicit configuration and budget.

The `filtered-explicit-direct-route` scenario records a reference-core distinction: receiving filtration prevents normal intake routing, but does not itself prohibit an explicit allowed direct decide. This fixture does not prove that the production Runtime/tool path exposes that operation. Keep SRC-03 unverified until that boundary is tested.

## Transactional Engine milestone

`engine.mjs <remote-built-binary> [report.json]` compares each command's result,
repository snapshot, domain events, attempted port callbacks and replies with the
pinned original Engine + configured Policy. Only root implementation-description
metadata is excluded; no domain ID, authorization field or timestamp is removed.

`engine-contracts` additionally replays the original **unchanged**
`tests/core-engine.spec.ts` through the Rust Engine. The test-only Linux FIFO
bridge synchronously calls the actual fixture's JS Policy/Scope/Clock, prepare
hook and observational ports. It never uses the original Engine to produce a
Rust result. Original model helpers and fixture repository projection remain TS
controls, not Rust helper coverage. Direct fixture repository writes are imported
before each command; Rust state, including error rollback, is exported afterward.

`reference/engine-clock.spec.ts` separately compares original-versus-Rust results,
clock reads, complete state and callback order after each command, using advancing
clocks with/without an event listener and 12 bounded seeded mixed-command walks.
Rejected commands and stale callbacks are compared too, not dropped from the run.
Evidence includes full compressed request/callback/result transcripts. Synthetic
ports perform no model, network, process-execution or source write operations.

The production `IntentPorts` projection differs deliberately from observational
fixtures on rollback: an unsuccessful command must retain no newly queued effect
or event. Rust unit tests check this distinction. `IntentPorts` is **not a durable
queue**; no intent may be dispatched until the application has atomically persisted
its domain state and intents. SQLite/receipt and crash components were subsequently added below; production
Runtime, source adapters and native async dispatch remain unimplemented.

## SQLite / durable queue milestone

`store-contracts` runs the product-owned Rust persistence tests and unchanged
original `store.spec.ts` (17), `effects.spec.ts` (13), and `outbox.spec.ts` (2).
The native SQLite connection, CRUD/read queries, generated working-set columns,
nested savepoints, versioned settings, receipt lifecycle, queue row selection,
generation fences, retries and health projection execute in Rust. Fixture callback
bodies, Promise scheduling, HealthBoard, Intl text segmentation, Runtime, Sources
and model helpers remain JavaScript controls. Do not call this Rust Runtime/source
coverage. The synchronous FIFO transport supports actual reentrant Store reads
from limit/source callbacks; it does not substitute expected Settings values.

The new `xharness-superagent` crate also commits the existing Rust Engine's domain
records and queued assessment/launch/delivery/reply intents in a single SQLite
transaction. Post-commit event/cache/stop controls cannot be released before the
outermost transaction commits or after it rolls back. Reply withdrawal really
removes a pending receipt, unlike a delivery drop which leaves a permanent receipt.
An in-flight send that finishes after withdrawal recreates its permanent receipt.

Native crash tests terminate a disposable child process at domain-save,
intent-save, before-commit and after-commit cut points, reopen the WAL database,
and replay acceptance without duplicate tickets, receipts or effects. No source
send, subprocess tool, model call or production service is used by those tests.

Known unfinished boundaries: production async effect driving/role/source
integration is not built yet; IM word segmentation is a supplied test callback,
not a Rust implementation. The current domain transaction adapter loads a complete
record snapshot and must not receive a scalability acceptance until its working-set
cost is reviewed/benchmarked or replaced by a repository boundary. General JSON
ordering/UTF-16/date gaps remain open. None is an approved behavior deviation.


## Atomic schema-10 migration milestone

`migration-contracts` builds/tests only on WZU_Server, then replays the original
**unchanged** `migration.spec.ts` (19 tests) against Rust initialization and native
basis/reason/copy helpers. The original JS Prompts consumers remain controls, not
native prompt/API coverage. A separate, unmocked comparison constructs the actual
original Store and the actual Rust Store over identical schema-10 seeds: nine
scenarios compare logical rows, SQLite columns/indexes and exact identity-bearing
Settings/Prompt/Source KV JSON encodings. It retains both raw JSON strings for 55
generic record encoding differences; these are unresolved D07 diagnostics, not
normalized-away passes or complete serialized snapshot parity.

Six additional native migration tests cover ten injected rollback points, eleven
real process-crash points (including after commit), replay idempotence, pending
queue attempts/generations, invalid-record atomicity and preserved missing legacy
Decision authority fields. These tests use disposable databases only. Production
upgrade backup, Runtime recovery and deployment rollback are still separate gates.

## Date/time component boundaries

`time-contracts` is intentionally strict and currently **fails**: eight timezone
grammar/arithmetic runs pass 110,720 finite cases, while the independent all-IANA
matrix passes 241,280 of 241,344 cases across 419 Node names. The 64 failures are
Gaza/Hebron far-future tail-rule cases; no comparison values are removed and the
wrapper returns nonzero. See `docs/superagent/time-compatibility.md`.

`time.mjs` and `time-zones.mjs` only invoke prebuilt remote native executables.
They compare original isoTime/Node Date.parse, full TimeClip and extended years,
legacy syntax, local DST gaps/overlaps and ancient LMT. V8 grammar BSD licensing
is retained. Native timezone alias/platform/version and non-string/UTF-16 input
boundaries are not inferred to pass from these finite matrices.


## SQL Repository and native Future driver

`repository-contracts` replays all 113 unchanged core Engine assertions and 14
advancing-clock/seeded walks using the native SQL Repository, via the same shared
transport as the Memory replay. Fixture import/export is explicitly test-only;
it is not included in production command I/O or performance claims. Policy,
Scope, Clock, observational port callbacks and fixture model controls remain JS.

Native repository tests check point/scoped/active read and write counts over
0/256/25,000 history records in each of six tables (150,000 total), unchanged raw
bytes on untouched records, failed SQL/decode propagation, nested rollback,
panic cleanup and staged receipts. SQLite statement VM/fullscan/sort counters
are measured; wall-clock values are diagnostics, not a cold-WAL end-to-end gate.
Corrupt typed-shape tests are native robustness evidence, not proof of equivalent
TypeScript behavior for arbitrary malformed databases.

`driver-contracts` executes real native Rust Futures, not JS Promise scheduling.
It covers commit/rollback control timing, dynamic limits and synchronous handler
prefixes, coalescing, generation/sequence fencing, backoff/Later, health watch,
per-kind idle/drain, cancellation, runner failure and queue restart. No extra
retry timer or implicit handler/model is installed. `Driver.pass` preserves the
reference per-thread commit boundary; the native failure fixture verifies that a
later thread's failure does not erase an earlier committed assessment/intent.

See `docs/superagent/repository-and-driver.md`. This is not yet a complete
role-scoped Runtime/session/Source/API product, and none of the final 96 Rubrics
is promoted to passed by these component counts.


## Native Sessions component (not final Runtime acceptance)

`bash scripts/superagent/remote-validate.sh native-session-contracts` syncs current uncommitted sources under the same exclusions and executes Rust only on WZU_Server. The component now uses the real DurableLoopAgentRuntime, JSONL Session/Inbox authority, FileLeaseManager and durable control/question services.17 offline-probe integration tests cover queue/flush/receipts, explicit cold resume and input observers, trusted role/creation/workspace binding, stable logical identity across state-root renames with an unchanged workspace, model/Prompt boundaries, SQL→JSONL admission and actual shutdown lease release.

The private creation-only Host cannot replace live product role prompts/models or admit input. Queue-only probes are not the reference's production steer relay, and no actual20product tools, skills, Source handlers, complete Passes/API/UI or job/child lifetime are installed. Retained external observers correctly prevent a graceful lease-release report. Full DSH delivery/turn/step equivalence remains open. See `reference/native-session-target-results.json` and `docs/superagent/native-sessions.md`; all96final Rubrics stay not_run.

Whole-workspace flat summary totals include output from subprocess test harnesses and may interleave; they are diagnostic, not a unique test inventory. Per-target failure sets, unchanged imported test-source hashes and the independently known73 owned tests at that Native-Sessions milestone (89 after Passes) are the acceptance evidence for this component regression. The2existing Feishu deployment failures, old reference stream-health failure,64future time-zone cases and55generic migration encoding differences remain unaccepted.


## Passes coordinator

`bash scripts/superagent/remote-validate.sh passes-contracts` builds the explicit test-only bridge with the owned `conformance` feature, runs all-target Clippy with that feature,16native tests and the unchanged original4Passes tests plus19independent reference/target cases. Only time and host observational ports are supplied by JS; pump/coalescing/timer/health/idle/stop decisions execute in Rust. Default product features do not include this bridge.

The await-health red→green case is retained: completion delivery and ready idle Future polling must not move a later coalesced request's error before an earlier pump/idle await returns. A native assertion covers the same boundary without the JS transport. Actual2second SQL progression, due-effect retry kicks, committed-prefix SQL faults and stop-vs-drain are independently tested.

See `reference/passes-target-results.json` and `docs/superagent/passes.md`. This advances a named Runtime coordination requirement; it is not the complete Sessions/Source/tool/provider/API/UI or deployment proof. Arbitrary Promise/global multi-owner interleavings are not inferred from finite cases. Owned inventory is now89tests (core18,app71), while all96final product Rubrics remain not_run.

## Full published SDK AgentLoop / real JSONL cold oracle

This supplementary Linux x64 Node 24 oracle runs the actual published loop,
Session/Projection/Tools/Skill services and JSONL backend with native flock,
plus original `Sessions` / `Prompts` / SQLite. Two separate Node processes
create and resume a synthetic session. The scripted model drives a real
`skill` tool dispatch and second model step; it makes no network model calls.
The recorder surrounds the actual pre-step middleware and compares its output
to the already remote-built production `superagent-skill-catalog-bridge`.
No pre-step waterfall or lifecycle events are manually invoked by the fixture.

Run on `WZU_Server` after syncing the current checkout (including changes,
excluding sensitive files) and building that bridge there:

```sh
python3 conformance/reference/fetch-full-agent-loop.py .alignment-evidence/NEW-sdk-cache
node conformance/run-full-agent-loop.mjs \
  .alignment-evidence/NEW-sdk-cache \
  .alignment-evidence/NEW-full-loop \
  target/debug/superagent-skill-catalog-bridge
```

Both cache and evidence paths must be fresh. Subsequent runs may reuse the
verified immutable cache, never an existing evidence directory. Set
`FULL_LOOP_COMPRESSION=zstd` to exercise the SDK's default compressed backend;
`none` is the explicit plain-JSONL fixture, not a product default change.
No npm install scripts run. Package tarballs and extracted trees are pinned
by `reference/full-agent-loop-source.json`; original 216 reference files
remain untouched. These packages are supplemental, not independently pinned
by the prototype lock; the three original Git baselines are not advanced.

Checks include catalog dedup/update/direct invocation, exact pre-restart
prefix plus the actual SDK `session/end-seed` marker, source unknown fields,
UTF-16 lone high/low values and keys distinct from literal escapes/U+FFFD,
assistant output, scoped status/error notifications and real tool results.
`runtime-context` is SDK-owned: an injected candidate is replaced/removed by
the actual request builder. The opaque preservation fixture therefore uses
an unreserved source kind and separately asserts reserved-candidate removal.

**This proves full SDK execution and the Rust pure policy differential, not
Native Rust AgentLoop integration or full product parity.** Six native tool
names are declarations whose bodies fail if called; the application-tool
list is empty, worker/Root/Sources and the other product tools are not covered.
No final Rubric is signed by this test, and non-Linux platforms remain not_run.
