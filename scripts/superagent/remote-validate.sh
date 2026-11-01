#!/usr/bin/env bash
# This wrapper never invokes a Rust compiler on the calling computer.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
MODE="${1:-rust}"
case "$MODE" in rust|core|oracle|reference|contracts|policy-contracts|engine-contracts|store-contracts|migration-contracts|time-contracts|repository-contracts|driver-contracts|native-session-contracts|passes-contracts|delivery-contracts|waking-input-contracts|retry-health-contracts|legacy-control-contracts|models-contracts|prompts-contracts|native-prompts-contracts) ;; *) echo "usage: $0 [rust|core|oracle|reference|contracts|policy-contracts|engine-contracts|store-contracts|migration-contracts|time-contracts|repository-contracts|driver-contracts|native-session-contracts|passes-contracts|delivery-contracts|waking-input-contracts|retry-health-contracts|legacy-control-contracts|models-contracts|prompts-contracts|native-prompts-contracts]" >&2; exit 2;; esac
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
EVIDENCE="$ROOT/.alignment-evidence/$STAMP-$MODE"
mkdir -p "$EVIDENCE"
REMOTE='~/codex-build/xharness-superagent/'
ssh -o BatchMode=yes -o ConnectTimeout=15 WZU_Server 'bash -s' <<'OWNED_REMOTE'
set -euo pipefail
dir="$HOME/codex-build/xharness-superagent"
owner='123123213weqw/xharness-superagent'
if test -d "$dir" && ! test -f "$dir/.superagent-publication-owner"; then
  if test -n "$(find "$dir" -mindepth 1 -maxdepth 1 -print -quit)"; then
    echo 'Refusing an existing non-owned build directory' >&2; exit 2
  fi
fi
mkdir -p "$dir"
if test -f "$dir/.superagent-publication-owner"; then
  test "$(cat "$dir/.superagent-publication-owner")" = "$owner"
else
  printf '%s\n' "$owner" > "$dir/.superagent-publication-owner"
fi
OWNED_REMOTE
rsync -az --exclude='.git/' --exclude='target/' --exclude='node_modules/' \
  --exclude='.alignment-evidence/' --exclude='.env' --exclude='.env.*' \
  --exclude='*.pem' --exclude='*.key' --exclude='credentials.json' --exclude='credentials.toml' \
  --exclude='cookies.txt' "$ROOT/" "WZU_Server:$REMOTE"
git -C "$ROOT" rev-parse HEAD > "$EVIDENCE/git-head.txt"
git -C "$ROOT" diff --binary > "$EVIDENCE/tracked-diff.patch"
git -C "$ROOT" status --short > "$EVIDENCE/git-status.txt"
# The host and directory are deliberately not caller-overridable.
ssh -o BatchMode=yes -o ConnectTimeout=15 WZU_Server "bash -s -- '$MODE'" <<'REMOTE_SCRIPT' 2>&1 | tee "$EVIDENCE/full.log"
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH" LC_ALL=C LANG=C
cd "$HOME/codex-build/xharness-superagent"
printf 'HOST=%s UTC=%s MODE=%s\n' "$(hostname)" "$(date -u +%FT%TZ)" "$1"
case "$1" in
  rust)
    cargo fmt --check --all
    cargo check --locked --workspace --all-targets
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo test --locked --workspace --all-targets --no-fail-fast
    cargo test --locked -p xharness-process --test process dropping_ -- --test-threads=1
    ;;
  core)
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent-core --all-targets -- -D warnings
    cargo test --locked -p xharness-superagent-core --all-targets
    cargo build --locked -p xharness-superagent-core --bins
    cd conformance
    node verify-reference.mjs
    npm ci --ignore-scripts
    npm run test:oracle
    node --experimental-transform-types primitives.mjs ../target/debug/superagent-primitives
    node --experimental-transform-types records.mjs ../target/debug/superagent-primitives
    node --experimental-transform-types policy.mjs ../target/debug/superagent-policy
    node --experimental-transform-types engine.mjs ../target/debug/superagent-engine
    ;;
  time-contracts)
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent-core --all-targets -- -D warnings
    cargo test --locked -p xharness-superagent-core --test time
    cargo build --locked -p xharness-superagent-core --bin superagent-primitives
    for zone in UTC Asia/Shanghai America/New_York Pacific/Apia Europe/Amsterdam Asia/Kathmandu Australia/Lord_Howe Africa/Casablanca; do
      TZ="$zone" node --experimental-transform-types conformance/time.mjs target/debug/superagent-primitives ".alignment-evidence/time-$(echo "$zone" | tr / _).json"
    done
    node --experimental-transform-types conformance/time-zones.mjs target/debug/superagent-primitives .alignment-evidence/time-zones.json
    ;;
  models-contracts)
    node conformance/verify-reference.mjs
    node conformance/verify-upstream.mjs
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent -p xharness-agent -p xharness-host --all-targets --features xharness-superagent/conformance -- -D warnings
    cargo test --locked -p xharness-agent --test observation
    cargo test --locked -p xharness-host --test loaded_observation
    cargo test --locked -p xharness-superagent --test models -- --nocapture
    cargo test --locked -p xharness-superagent --test native_sessions -- --nocapture
    ;;
  legacy-control-contracts)
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent --all-targets -- -D warnings
    cargo test --locked -p xharness-superagent --test legacy_control_boundaries -- --nocapture
    cargo test --locked -p xharness-host --test basic_host -- --nocapture
    ;;
  retry-health-contracts)
    node conformance/verify-reference.mjs
    node conformance/verify-upstream.mjs
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent --all-targets --features conformance -- -D warnings
    cargo test --locked -p xharness-superagent --test retry_health -- --nocapture
    cargo test --locked -p xharness-superagent --test native_sessions -- --nocapture
    cargo build --locked -p xharness-superagent --bin superagent-retry-health-bridge --features conformance
    export SUPERAGENT_RETRY_HEALTH_BRIDGE_BIN="$PWD/target/debug/superagent-retry-health-bridge"
    export SUPERAGENT_RETRY_HEALTH_EVIDENCE="$PWD/.alignment-evidence/retry-health-target.json"
    cd conformance/reference/upstream
    npm exec --yes --package=pnpm@11.7.0 -- pnpm install --frozen-lockfile --ignore-scripts
    ./node_modules/.bin/vitest run --config ../retry-health-target.vitest.mjs --reporter=default --reporter=json --outputFile=../../.alignment-evidence/retry-health-target-vitest.json
    ;;
  waking-input-contracts)
    node conformance/verify-reference.mjs
    node conformance/verify-upstream.mjs
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent-core -p xharness-superagent -p xharness-agent -p xharness-host --all-targets --features xharness-superagent/conformance -- -D warnings
    cargo test --locked -p xharness-agent --test noninterrupting_step
    cargo test --locked -p xharness-agent --lib input_dispatch
    cargo test --locked -p xharness-host --test noninterrupting_admission
    cargo test --locked -p xharness-superagent --test native_sessions -- --nocapture
    ;;
  native-prompts-contracts)
    node conformance/verify-reference.mjs
    node conformance/verify-upstream.mjs
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent --all-targets -- -D warnings
    NATIVE_PROMPT_TRACE_DIR="$PWD/.alignment-evidence/native-prompt-traces" cargo test --locked -p xharness-superagent --test native_prompts -- --nocapture
    cargo test --locked -p xharness-superagent --lib native_sessions::prompts:: -- --nocapture
    export NATIVE_PROMPT_TRACE_DIR="$PWD/.alignment-evidence/native-prompt-traces"
    export NATIVE_PROMPT_ORACLE_EVIDENCE="$PWD/.alignment-evidence/native-prompt-oracle.json"
    cd conformance/reference/upstream
    npm exec --yes --package=pnpm@11.7.0 -- pnpm install --frozen-lockfile --ignore-scripts
    ./node_modules/.bin/vitest run --config ../native-prompts.vitest.mjs --reporter=default --reporter=json --outputFile=../../.alignment-evidence/native-prompt-oracle-vitest.json
    ;;
  prompts-contracts)
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent --all-targets --features conformance -- -D warnings
    cargo test --locked -p xharness-superagent --test prompts
    cargo build --locked -p xharness-superagent --bin superagent-store-bridge
    export SUPERAGENT_STORE_BRIDGE_BIN="$PWD/target/debug/superagent-store-bridge"
    export SUPERAGENT_STORE_TARGET_EVIDENCE="$PWD/.alignment-evidence/prompts-target.json"
    export SUPERAGENT_PROMPTS_DIFFERENTIAL_EVIDENCE="$PWD/.alignment-evidence/prompts-differential.json"
    cd conformance/reference/upstream
    npm exec --yes --package=pnpm@11.7.0 -- pnpm install --frozen-lockfile --ignore-scripts
    ./node_modules/.bin/vitest run tests/prompts.spec.ts --reporter=default --reporter=json --outputFile=../../.alignment-evidence/prompts-reference-vitest.json
    ./node_modules/.bin/vitest run --config ../prompts-target.vitest.mjs --reporter=default --reporter=json --outputFile=../../.alignment-evidence/prompts-target-vitest.json
    ;;
  delivery-contracts)
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent-core -p xharness-superagent --all-targets --features xharness-superagent/conformance -- -D warnings
    cargo test --locked -p xharness-superagent --test delivery
    cargo build --locked -p xharness-superagent --bin superagent-delivery-bridge --features conformance
    export SUPERAGENT_DELIVERY_BRIDGE_BIN="$PWD/target/debug/superagent-delivery-bridge"
    export SUPERAGENT_DELIVERY_EVIDENCE="$PWD/.alignment-evidence/delivery-target.json"
    cd conformance/reference/upstream
    npm exec --yes --package=pnpm@11.7.0 -- pnpm install --frozen-lockfile --ignore-scripts
    ./node_modules/.bin/vitest run --config ../delivery-target.vitest.mjs --reporter=default --reporter=json --outputFile=../../.alignment-evidence/delivery-target-vitest.json
    ;;
  passes-contracts)
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent-core -p xharness-superagent --all-targets --features xharness-superagent/conformance -- -D warnings
    cargo test --locked -p xharness-superagent --test passes
    cargo build --locked -p xharness-superagent --bin superagent-passes-bridge --features conformance
    export SUPERAGENT_PASSES_BRIDGE_BIN="$PWD/target/debug/superagent-passes-bridge"
    export SUPERAGENT_PASSES_EVIDENCE="$PWD/.alignment-evidence/passes-target.json"
    cd conformance/reference/upstream
    npm exec --yes --package=pnpm@11.7.0 -- pnpm install --frozen-lockfile --ignore-scripts
    ./node_modules/.bin/vitest run --config ../passes-target.vitest.mjs --reporter=default --reporter=json --outputFile=../../.alignment-evidence/passes-target-vitest.json
    ;;
  native-session-contracts)
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent-core -p xharness-superagent --all-targets -- -D warnings
    cargo test --locked -p xharness-superagent --test native_sessions -- --nocapture
    cargo test --locked -p xharness-superagent-core -p xharness-superagent --all-targets
    ;;
  repository-contracts)
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent-core -p xharness-superagent --all-targets -- -D warnings
    cargo test --locked -p xharness-superagent-core -p xharness-superagent --all-targets
    cargo test --locked -p xharness-superagent --test repository -- --nocapture
    cargo build --locked -p xharness-superagent-core -p xharness-superagent --bins
    export SUPERAGENT_ENGINE_BRIDGE_BIN="$PWD/target/debug/superagent-sql-engine-bridge"
    export SUPERAGENT_ENGINE_TARGET_EVIDENCE="$PWD/.alignment-evidence/sql-engine-target.json"
    export SUPERAGENT_ENGINE_REPOSITORY=sqlite
    cd conformance/reference/upstream
    npm exec --yes --package=pnpm@11.7.0 -- pnpm install --frozen-lockfile --ignore-scripts
    ./node_modules/.bin/vitest run --config ../sql-engine-target.vitest.mjs --reporter=default --reporter=json --outputFile=../../.alignment-evidence/sql-engine-target-vitest.json
    ;;
  driver-contracts)
    cargo fmt --check --all
    cargo clippy --locked -p xharness-superagent --all-targets -- -D warnings
    cargo test --locked -p xharness-superagent --test driver -- --nocapture
    ;;
  oracle)
    cd conformance
    node verify-reference.mjs
    npm ci --ignore-scripts
    npm run test:oracle
    ;;
  reference|contracts|policy-contracts|engine-contracts|store-contracts|migration-contracts)
    MODE="$1"
    if [ "$MODE" = policy-contracts ]; then
      cargo build --locked -p xharness-superagent-core --bin superagent-policy
      export SUPERAGENT_POLICY_BIN="$PWD/target/debug/superagent-policy"
    fi
    if [ "$MODE" = engine-contracts ]; then
      cargo build --locked -p xharness-superagent-core --bin superagent-engine-bridge
      export SUPERAGENT_ENGINE_BRIDGE_BIN="$PWD/target/debug/superagent-engine-bridge"
      export SUPERAGENT_ENGINE_TARGET_EVIDENCE="$PWD/.alignment-evidence/engine-target.json"
    fi
    if [ "$MODE" = store-contracts ] || [ "$MODE" = migration-contracts ]; then
      cargo clippy --locked -p xharness-superagent --all-targets -- -D warnings
      cargo test --locked -p xharness-superagent --all-targets
      cargo build --locked -p xharness-superagent --bins
      export SUPERAGENT_STORE_BRIDGE_BIN="$PWD/target/debug/superagent-store-bridge"
      export SUPERAGENT_STORE_TARGET_EVIDENCE="$PWD/.alignment-evidence/store-target.json"
    fi
    cd conformance/reference/upstream
    npm exec --yes --package=pnpm@11.7.0 -- pnpm install --frozen-lockfile --ignore-scripts
    if [ "$MODE" = policy-contracts ]; then
      ./node_modules/.bin/vitest run --config ../policy-target.vitest.mjs
    elif [ "$MODE" = engine-contracts ]; then
      ./node_modules/.bin/vitest run --config ../engine-target.vitest.mjs --reporter=default --reporter=json --outputFile=../../.alignment-evidence/engine-target-vitest.json
    elif [ "$MODE" = store-contracts ]; then
      ./node_modules/.bin/vitest run --config ../store-target.vitest.mjs --reporter=default --reporter=json --outputFile=../../.alignment-evidence/store-target-vitest.json
    elif [ "$MODE" = migration-contracts ]; then
      ./node_modules/.bin/vitest run --config ../migration-target.vitest.mjs --reporter=default --reporter=json --outputFile=../../.alignment-evidence/migration-target-vitest.json
      ./node_modules/.bin/vitest run --config ../migration-differential.vitest.mjs --reporter=default --reporter=json --outputFile=../../.alignment-evidence/migration-differential-vitest.json
    elif [ "$MODE" = contracts ]; then
      ./node_modules/.bin/vitest run --config ../capture.vitest.mjs
    else
      ./node_modules/.bin/vitest run --maxWorkers=2 --no-file-parallelism
    fi
    ;;
esac
REMOTE_SCRIPT
