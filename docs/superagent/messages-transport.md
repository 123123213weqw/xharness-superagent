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
tests. Existing Runtime, Host, Core and default sandbox code are unchanged.
