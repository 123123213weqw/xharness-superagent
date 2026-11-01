# Optional tool model rendering

`ToolSpec::with_model_rendering(ToolModelRendering::SuccessfulContent)` is an
explicit trusted registration capability. The default remains `JsonEnvelope`.
No model arguments, tool metadata or persisted user content can opt a tool in.
The registry cannot replace registrations; a request retains its executor.

Core sends content verbatim only for complete success with no error and only
when the UTF-8 byte length fits the configured result budget. Full content and
metadata are persisted in the existing ToolResult event, not a new event/schema
or a metadata marker. JSON escaping/duplicated metadata does not trigger archival
of this lossless literal observation. Cold projection uses the already persisted
model text, even with no executor or plugin installed.

Every failure, pre-truncated observation, unknown outcome and oversize result
retains the existing explicit envelope/reduction/archive behavior. Side effects,
approval, deadlines, concurrency, lifecycle, repetition observation and raw
ToolCompleted outcome do not change. Existing callers keep their default bytes.

This capability alone is not SuperAgent parity: oversized skill instructions,
SDK error presentation, UTF-16 edge inputs, catalog injection and all other goal
Rubrics remain open until differential and product lifecycle evidence exists.
