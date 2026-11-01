# Repository execution rules

- Match the user's language; keep code comments consistent with existing modules.
- Current scope is docs/superagent/product-scope.md: reproduce product logic, not the complete JavaScript language. Do not restart the cancelled all-path UTF-16 migration.
- Preserve ordinary Unicode/emoji and product-observable identities, authorization, errors and recovery. Never count exclusions as passing tests.
- Never compile Rust locally. Sync current source to a dedicated WZU_Server build directory before compiling remotely; GitHub Actions is also allowed. cargo fmt is permitted locally.
- Exclude .git/, target/, node_modules/, .env/.env.*, keys, credentials and private evidence from remote synchronization. Preserve SSH/TLS verification. No local fallback on network errors.
- Do not modify existing services or non-owned build directories. The remote validation wrapper checks directory ownership.
- Preserve MIT/BSD/Apache notices and the fixed reference under conformance/reference/upstream. Never edit reference assertions to make a result green.
- Keep unsafe forbidden and recoverable errors typed in owned policy/application code. Do not relax lint fences or use silent defaults to hide failures.
- Do not introduce a second chat history store, broaden permissions, enable auto-merge, call paid models or deploy without explicit authorization.
- This is an incomplete development snapshot. State test scope and known failures honestly; historical conformance evidence is not end-to-end product acceptance.
