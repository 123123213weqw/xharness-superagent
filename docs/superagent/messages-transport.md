# Existing Messages routes

The formal product configuration accepts `protocol: "anthropic_messages"` in
addition to the unchanged `chat_completions` and `responses` routes. This is a
product adapter, not a replacement runtime or a remote SDK dependency.

See [disabled-by-default example](../../config/superagent.messages.example.json).
Choose the existing account, exact model and endpoint explicitly. `baseUrl`
ends at `/anthropic` (or another namespace root) or `/v1`; the adapter appends
`/v1/messages` or `/messages` respectively. It never substitutes a model, changes
billing routes, or falls back to another account. `maxOutputTokens` is required
and positive; a smaller runtime ceiling wins.

Optional `reasoning` maps opaque effort names to endpoint-specific JSON patches.
Only `thinking`, `output_config`, `temperature` and `top_p` may be patched. For
example, a verified DeepSeek route can select:

```json
"reasoning": {
  "high": { "thinking": { "type": "enabled" }, "output_config": { "effort": "high" } }
}
```

The selector must explicitly request `reasoningEffort: "high"`. Do not assume
another vendor supports the same patch. Credentials are read from `apiKeyEnv`,
never from the configuration file, and retain the existing subprocess scrubbing.

## Boundaries

- Actual HTTPS/SSE transport, rolling read/header deadlines, cancellation, bounded
  retained content and HTTP diagnostics. TLS verification remains enabled and
  redirects are not followed with credentials.
- JSON tool schemas and provider-native call IDs, complete tool result pairing,
  thinking/signature/content replay via the existing Session `provider_items`.
- A missing terminal event, incomplete JSON, duplicate IDs or inconsistent
  terminal reasons cannot be reported as successful completion.
- This is currently a **text route**. Images/files, server-side tools, foreign
  opaque provider state and relocating a mid-history system message are rejected
  explicitly. Do not advertise full Anthropic/vendor feature equivalence.
- The optional live test is ignored by default. Running it requires explicit
  authorization for the exact existing route/credential; its passing result is
  adapter evidence, not Source/Worker/PR or production replacement acceptance.

Only two direct edges to already locked packages were added: `reqwest` and
`async-stream`. No external package versions/checksums were refreshed. The exact
lock overlay reconstructs the previous audited lock and has negative mutation
tests. Core/Host and default sandbox behavior are unchanged. A separate, explicit
ProcessRuntime stdin lifetime option fixes the real Feishu consumer defect;
see the live acceptance record.

## Real Feishu stdin failure and narrow runtime fix

The actual unbounded CLI exits when stdin reaches EOF. Its managed process was
previously spawned with null stdin, repeatedly leaving the live Source unhealthy;
history polling could still receive messages and therefore masked the defect.
A side-by-side actual CLI run confirmed: null/unbounded exits immediately,
held-pipe/unbounded stays ready, and the CLI's bounded mode also stays ready.

`ProcessRuntime::spawn_with_open_stdin` now explicitly opts into a supervisor-owned,
non-writable stdin pipe. Taking it out of `Child` matters: `Child::wait` otherwise
closes stdin. The pipe lives until managed-tree shutdown; normal `spawn`, argv,
environment replacement, sandbox defaults, TERM/KILL handling and drain ordering
remain unchanged. Feishu alone opts in. No shell wrapper or periodic artificial
consumer timeout was introduced. Multi-line diagnostics now retain the actual
bounded CLI error instead of only its final JSON brace.

Adversarial offline CLI fixtures now reproduce real stdin-EOF behavior. Tests
also cover the old null default, explicit held lifetime, normal exit, timeout,
cancellation and existing process-tree cleanup. A separately authorized real-bot
test observed ready, a stable connection, cancellation and join; that probe alone
is not live Ticket/reply acceptance.
