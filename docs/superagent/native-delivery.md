# Explicit native delivery composition (progress, not final acceptance)

`NativeSessions::open` keeps its physical JSONL/default semantics. The new
`open_buffered` is an explicit opt-in for `NativeDelivery::new(&Arc<NativeSessions>)`.
Restore/ensure/configure are still trusted owner operations; construction starts
no Agent, Source, auto-work or detached local task. The owner must drive and join
the returned existing AgentDelivery `Runner`. No production entry point has been
switched implicitly.

## Ownership and authority

* Runtime, private creation-only Host, tools, question journal and model health
  all receive the same `Arc<dyn Store>` backed by one buffered journal. There is
  no SQL Inbox, second Session cache, second Agent executor or retry driver.
* A physical JSONL clone is only a persistence/history read port after the global
  flush barrier. It is never passed to execution, tools, model projections,
  pending reads or input writes. External physical writers are foreign-prefix
  faults; they are not rebased onto a dirty logical journal.
* Resolving requires the non-serializable `ManagedSession` owner/role/creation/CWD
  capability. Live identity is the exact `Rc<dyn Agent>`, not its durable ID. A
  stale disposer must not evict a later object. Agent objects contain no strong
  DurableAgentHandle/activation or control for a future run.
* Existing selected durable role models, CWD and permission are reused. Missing
  routes fail closed; default WorkspaceWrite remains unchanged and private
  BasicHost remains unbound to the live Runtime.

## Native hot band

The optional Agent `prepare_input` hook defaults to `None`; all original foreign
Agent/reference protocol tests retain their old path. The native hook obtains a
single-use actor rendezvous before `signal -> current -> authorize -> pending ->
logical commit -> accepted`. There is no await/physical I/O in the callback band.
The original Source attribution and ordered text blocks are kept; a user-role
producer is not reclassified as a human. `accepted` is logical visibility, not a
successful fsync. Throwing from it keeps the same failure object and releases the
actor-owned accepted input without rolling it back. Dropping a result waiter does
not cancel the work adopted by the existing Runner.

Per-call scopes have pointer-fenced cleanup: an earlier send finishing after a
later reentrant preparation must not clear the later scope. Uncommitted scopes
release the actor on Drop; committed scopes release their owning receipt. Root
shutdown fences admission, joins the owned Runtime/models, certifies physical
lease release, then closes the opted-in store within the remaining deadline.
Failure/timeout is not a graceful durability certificate.

## Still OPEN — no full AgentDelivery/Rubric claim

1. The native hook introduces an asynchronous preparation phase. Exact abort /
   authorization / unavailable-model error precedence in every SDK phase is not
   accepted, and this is not a permanently warm synchronous Agent façade.
2. The native scope now calls regular `begin_persistence` before returning its
   observation future. Global flush is invoked immediately after that release,
   and both observations are joined without abandoning the global sweep on an
   individual failure. Global typed aggregate errors take precedence. Old full
   `finish`, later control-failure ownership and original callback failures stay
   unchanged. An individual observation still exists; exact scheduling/retry
   interactions are not accepted as complete SDK behavior.
3. The product barrier now snapshots explicitly managed writers at invocation,
   flushes sequentially in registration order, keeps original generations,
   drains through concurrent logical appends and collects typed failures in
   order. Generic `BufferedJsonlSessionStore::flush_all` keeps Session-ID order.
   Full SDK creation reservations/open claims before I/O, disposal/close drain,
   overlapping lifecycle teardown and unbound active writers remain unaccepted.
   See global-flush.md for independent original tracker and real FS evidence.
4. This adapter reconciles consumed live history from the canonical immutable
   cut before its hot band. The actual bounded incremental live Session event
   subscription/initial-read overlap is not installed. Full-history rescanning
   and multimodal/legacy event coverage are not accepted as final SDK behavior.
5. A caller must resolve a trusted live identity before synchronous `send`.
   Resolve itself is not an already-adopted producer operation. The complete
   Sessions.prompt wrapper, prompt revisions, context/skills, Mandate and human
   approval, Root/worker lifecycle, exact tools, Source/API/UI installation and
   graceful Runner lifecycle remain outside this progress seam.
6. Legacy public RunningTurn A→later B control risk, original interrupt timeout,
   migration/foreign-store/time-zone/Feishu gaps remain open and unwaived.

All 96 final behavior rubrics remain `not_run`; component tests are not final
product acceptance. Reference commit and old frozen evidence are unchanged.

## Subsequent warm-authority progress

The per-call Host creation-proof flush bottleneck described above was reproduced
and removed using a private warm scope derived at trusted resolve. Real idle/live
Core ×3mode ×2runtime OS-lock producer matrices and recovery/revocation tests are
now implemented; see warm-authority.md for their exact evidence/remaining scope.
This does NOT remove the per-call actor rendezvous or establish a complete
synchronous SDK Agent facade. A later controlled persistence-observation import
separates Core completion from native persistence acknowledgement; see the
architecture note and current execution status for its precise scope.
