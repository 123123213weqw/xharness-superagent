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
