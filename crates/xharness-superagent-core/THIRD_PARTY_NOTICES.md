# Reference attribution

The SuperAgent domain model and stage-classification semantics are adapted from
`deepseek-ai/dsh-superagent-prototype` at
`6337fc823ce20251c8de8085948165c81aaa701e` (`src/core/model.ts`, `stage.ts`, `ids.ts`,
`memory.ts`, `errors.ts`, `reasons.ts`, `src/policy.ts`, `src/admission.ts` and `src/limits.ts`).
The original MIT license is retained verbatim in
`LICENSE-MIT` beside this notice and applies to the referenced portions.
This module does not implement or claim full Engine, Source, persistence, or runtime parity yet.

Identity-sensitive number formatting uses the safe API of the separately licensed
`ryu-js = 1.0.3` dependency, without changing serde_json features for other crates.
See its [upstream documentation](https://docs.rs/ryu-js/1.0.3/ryu_js/) and package license files.
