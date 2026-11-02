# Linux user service

`xharness-superagent@.service` supervises one explicitly configured profile. It
is not a migration command: do not point it at a running legacy state or enable
an old and new consumer for the same Source simultaneously.

Install a fixed, remotely built binary at
`~/.local/lib/xharness-superagent/PROFILE/bin/xharness-superagent`, and a mode-600
configuration at `~/.config/xharness-superagent/PROFILE/config.json`. The working
directory must exist. Optional `environment` uses systemd EnvironmentFile syntax
and must be private; reference existing authorized credentials rather than
committing them. Configure stable CLI/toolchain/cache paths explicitly. Worker
network and permission defaults are unchanged. The unit has no credential
bootstrap, self-SSH, proxy, or automatic permission grant.

After validating the chosen profile name, inspect and install the unit:

```sh
install -m 644 deploy/superagent/xharness-superagent@.service \
  ~/.config/systemd/user/xharness-superagent@.service
systemctl --user daemon-reload
systemctl --user start xharness-superagent@PROFILE.service
systemctl --user status xharness-superagent@PROFILE.service
journalctl --user -u xharness-superagent@PROFILE.service
systemctl --user stop xharness-superagent@PROFILE.service
```

Run the binary's `--check` against the configured **new** state before starting.
Do not enable at boot until the profile's acceptance and rollback gates pass.
A host administrator must separately verify user-manager persistence/boot policy;
this template does not change linger settings or system services.

`KillMode=control-group` is essential: root SIGKILL cannot run Rust cleanup, but
systemd kills remaining children in the service cgroup before restarting it.
Graceful stop gives the product 45 seconds to join Source/Native/tool processes,
then force-kills that cgroup. `Restart=on-failure` retries crashes, not an explicit
operator stop. Start-rate limiting prevents an infinite configuration-failure
loop. `NoNewPrivileges` must pass the installed sandbox's readiness check; do
not replace a failed check with full access or disable sandboxing.

This does not stop commands already launched on another SSH host. Such workflows
need an independently verified remote cancellation/reconciliation protocol.
Neither cgroup cleanup nor a directory snapshot reconciles an already-created
external comment, commit, or PR. A killed native request remains subject to the
product's existing restart/replay rules, not an exactly-once OS guarantee.

## Multi-instance release maintenance

Maintain **one source tree and immutable binaries**, not a Git checkout patched
separately for every bot. A profile keeps its own state, sessions, workspace,
source/account configuration, model routing, management port and systemd unit.
Search is independent: opt into `webSearch.provider=deepseek-search-api` in each
Rust profile and reference the existing `DEEPSEEK_API_KEY`. DeepSeek, GLM and other
reasoning routes then use the same API tool; their model keys/routes do not change.
The template can read an optional private `shared-search.environment` before the
profile environment. Do not copy provider secrets into releases, prompts or Git.

The operator-only `scripts/superagent/fleet-control.py` provides:

- `inventory`: read-only implementation/channel/release/hash/health/load for each
  registered instance, including legacy instances during migration.
- `stage`: retain a remotely built Linux binary under its commit + binary hash,
  with source fingerprint and passing test/Clippy evidence. Existing releases
  are verified, never overwritten. This command never compiles or installs.
- `plan`: verify exact owned unit/binary/config, isolated state/port and release
  compatibility. Legacy instances cannot receive a Rust binary without a
  separately authorized cold migration. Model/account/source changes are not
  smuggled into an update; only binary, explicit search and workspace policy can change.
- `upgrade`: serialize upgrades under a fleet lock; defer if workers are active,
  waiting or queued; pause, recheck, stop/join **one** service, verify a full
  current-state backup, atomically install a new binary inode and run `--check`.
  Wait for runtime and previously healthy enabled Sources before activation.
  Record the deployed artifact on that instance. Other units are not restarted.
- `promote`: explicit operator acceptance of the currently running canary and its
  matching binary, not elapsed time or a fabricated test result. Stable profiles
  cannot upgrade an unpromoted release.
- `incident`: record a concise operator bug report tied to instance, release,
  binary hash and current health. It does not publish chat, credentials or logs
  to GitHub. Add a sanitized reproduction/regression in the main repository,
  fix there, build the next release, canary-test, promote, then roll one by one.

Use an owned mode-700 fleet directory with a `.superagent-fleet-owner` marker and
mode-600 `registry.json`. Each Rust instance's dedicated root must have its own
`.superagent-goal-owner`, and the exact systemd unit digest in `unitShaFile`.
Registry example shape (replace paths/owners deliberately; no automatic adoption):

```json
{
  "schema": 1,
  "owner": "YOUR_FLEET_OWNER",
  "instances": {
    "rs": {
      "kind": "rust", "channel": "canary", "port": 3093,
      "root": "/absolute/owned/rs", "owner": "YOUR_RS_OWNER",
      "binary": "/absolute/owned/rs/xharness-superagent",
      "config": "/absolute/owned/rs/config.json",
      "state": "/absolute/owned/rs/state",
      "environment": "/absolute/owned/rs/environment",
      "unit": "xharness-superagent-rs.service",
      "unitFile": "/home/OPERATOR/.config/systemd/user/xharness-superagent-rs.service",
      "unitShaFile": "/absolute/owned/rs/unit.sha256"
    },
    "old-profile": {"kind": "legacy", "port": 3092}
  }
}
```

Run commands **on the server**, after syncing current source and doing all Rust
compilation/test/Clippy there. Stage evidence is a JSON record with
`testsPassed > 0`, `testsFailed: 0`, `strictClippyPassed: true` and the exact
64-hex `sourceFingerprint`. Keep full test logs alongside your verification.

```sh
python3 /path/fleet-control.py --registry /private/fleet/registry.json inventory
python3 /path/fleet-control.py --registry /private/fleet/registry.json stage \
  --binary /remote/target/debug/xharness-superagent \
  --source-commit EXACT_40_HEX_COMMIT --evidence /private/verified-release.json
python3 /path/fleet-control.py --registry /private/fleet/registry.json plan \
  --instance rs --release RELEASE_ID --config /private/rs/next-config.json
python3 /path/fleet-control.py --registry /private/fleet/registry.json upgrade \
  --instance rs --release RELEASE_ID --config /private/rs/next-config.json
python3 /path/fleet-control.py --registry /private/fleet/registry.json promote \
  --instance rs --release RELEASE_ID --acceptance /private/canary-acceptance.json
```

Acceptance must contain `accepted: true` and the exact `release` ID. A health
check alone is not a business acceptance. Do not promote incomplete workflows.
To roll a compatible release back, run the same guarded `plan`/`upgrade` with the
retained previous release and its compatible configuration, **keeping current
state/cursors**. Failure before activation automatically restores the previous
binary/config, not the old database. Once activation was attempted, errors require
current-state reconciliation rather than replaying an old snapshot.

This first updater only accepts schema-11, same-event-compatible releases. It is
not a database migration framework, an unattended auto-updater, a crash-atomic
multi-file transaction, a cross-host SSH canceller or a guarantee that external
side effects can be undone. All legacy profiles remain visibly `legacy` until
cold-migrated and separately accepted; an inventory is not a completed rollout.

## Generic repository checkout

Opt in independently of the bounded `repository_delivery` tool:

```json
{"workspace":{"checkout":{"enabled":true,"cache":true,"revision":{"kind":"auto"}}}}
```

Repository and account always come from the verified GitHub Source; never copy
another instance's working checkout or cache. `auto` checks out a trusted GitHub
PR trigger's current head; other tasks resolve the repository's default branch
through GitHub. Explicit revision kinds are `default_branch`, `branch` (`name`),
`commit` (full `sha`) and `pull_request` (positive `number`). No main/master guess.
This checks out a PR head; it does not by itself implement every PR continuation
or publishing workflow. Publisher policy/base configuration remains separate.

Each task has a new independent `.git` and writable working directory. The
optional disposable bare cache lives below that instance's
`stateDir/repository-cache`, keyed by canonical repository and verified account
identity (not a token). It stores no credential helper/token, never uses borrowed
alternates or hardlinks, and is protected by an OS lock. GitHub resolves the
expected SHA on every checkout; a fetch must match it exactly. API/fetch failure
never silently serves a stale checkout. Invalid cache identity/connectivity is
quarantined and rebuilt; retained quarantine/snapshots require operator disk
maintenance while no checkout is running (no automatic GC in this release).
Cancellation joins subprocesses, removes unpublished task staging and releases
the lock. Existing task work is never reset by checkout. A workspace already
prepared before a later preparation-command failure is preserved, not silently
replaced; reconcile it through the existing retry/recovery flow.

Omitting `workspace` preserves the legacy `repositoryDelivery.checkout` path.
An explicit workspace policy overrides it, including `enabled:false`. `cache`
defaults false. Ordinary Worker permissions, model routes, account grants and
per-instance data are unchanged. Example: `config/superagent.workspace.example.json`.
Existing persisted Source CLI settings must be changed through the human
management CAS endpoint; editing first-boot `settings` is not a live migration.
