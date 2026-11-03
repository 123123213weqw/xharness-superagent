# Model-directed GitHub exploration

This product extension lets the model choose what to inspect, instead of
requiring a checkout or a separate resource read for each issue. It does not
attempt to reproduce JavaScript semantics or replace the existing GitHub Source.

## Flow

1. `github_discover {}` returns a small catalog.
2. `github_discover {"operation":"search_issues"}` returns that operation's
   parameter schema, on demand.
3. `github_query {"operation":"search_issues","parameters":{"text":"checkout","state":"open"}}`
   returns compact matches. The model chooses relevant numbers to inspect with
   `get_issue`/`get_pull`, not a host-chosen issue traversal.
4. Feed `next_call` back into `github_query` for another page when relevant.
   This is explicit, not unbounded automatic pagination.
5. Workers can use the existing `gh api` path and official GitHub documentation
   for unsupported endpoints/full contents. Use Git for edits/builds, not merely
   to obtain issue context. Main still has no shell, file writes or generic fetch.

The initial catalog is intentionally limited: issue/PR lists, literal issue/PR
search, issue/PR details, conversation comments, changed PR files, branch/commit
lists, and directory listings. It is **not** unrestricted GitHub REST/GraphQL,
code search, or a full OpenAPI agent. The existing worker CLI remains the escape
hatch, under its existing ticket mandate and operator-selected process policy.
No extra crates, service credentials or instance-specific repository constants
are introduced.

## Runtime responsibilities

- Repository/account come from the verified, enabled GitHub Source; arguments
  cannot change credentials, host, method, repository or CLI flags.
- All new remote operations are GET-only. Search uses a dedicated transport
  method; it cannot bypass the ordinary repository-prefix API restriction.
  Text is quoted literal terms, not a raw `q` expression. Search results are also
  checked against the configured repository. State is a separate validated filter.
- One page per request, default 20, maximum 50. `Link` is parsed only to extract
  the next page; its URL is never executed. Original filters/operation are reused.
- Compact cards expose identity, title/status, author, timestamps and text previews;
  missing fields are not full content. Text clipping is Unicode-safe and named in
  `truncated_fields`. The usual tool-result budget may further reduce an oversized
  response, retaining the executor's explicit reduction envelope.
- Search exposes `incomplete_results`, `total_count` and the 1000-match cap.
  `list_issues` includes PRs as GitHub does; `search_issues` explicitly excludes PRs.
- Directory results are sliced with `offset`/`limit` (max 50), disclose the Contents
  API's [1000-entry ceiling](https://docs.github.com/en/rest/repos/contents#get-repository-content), and include a next call. Each call re-reads the directory;
  use a commit SHA in `ref` for a stable listing. No implicit snapshot is promised.
- New GitHub queries serialize across Main and Workers in this process. They do
  **not** serialize the existing Source poller, ordinary worker CLI, or other
  instances. There is no new response cache, automatic retry loop, or global
  rate-limit scheduler. CLI failures (including rate limits) remain explicit;
  do not treat them as an empty collection or repeatedly retry unchanged errors.
- Product pause, Source enablement/repository, Worker ticket/assignment/mandate,
  and cancellation are checked before and after query I/O. The existing CLI
  cancellation path terminates and joins its subprocess. No second history/inbox
  or durable credential cache is added.

## Acceptance scope

Deterministic protocol tests and the formal executable's loopback model/GitHub
fixtures exercise discovery, model-selected operations, two-page search, selected
details, Worker file work, Source-disable rejection, paused-read rejection with
management resume preserved, and Main's lack of shell/write capabilities. Fixtures are not a paid-model evaluation or a live GitHub performance
benchmark. In particular this does **not** prove that the historical 554-read task
now takes a specific number of requests, or complete the existing PR multi-round
workflow goal. See the accompanying verification record for measured test results.

No service is deployed by this change. Existing production binaries/configuration
remain unchanged until an explicit rollout.
