# Service deployment examples

The maintained deployment guide is in the [Agent Coordinator book source](../book/src/deploy/README.md). The systemd units and configuration examples remain in this directory.

## Supervised agent host (`agentc/`)

`agentc/host-setup.sh` prepares a Linux host for unattended, supervised agent
launches: the `agentc-impl` and `agentc-rev` accounts (plus `agentc-egress` for
the proxy), root-owned pinned `claude`, `codex`, `agent-coordinator` and
`agentc-supervisor` binaries and a Rust toolchain under `/opt/agentc`, private
0700 per-role state under `/var/lib/agentc`, a read-only Git mirror, the
`agentc-egress` allowlist proxy and an nftables table that filters only the
agent accounts (loopback proxy, staging port and ephemeral test ports only; no
DNS, no direct internet). Build the two binaries first, then:

```sh
cargo build --release --locked -p agentc-supervisor -p coordinator-cli
sudo SUPERVISOR=target/release/agentc-supervisor CLI=target/release/agent-coordinator \
  deploy/agentc/host-setup.sh
```

Log each role's harnesses in and install its supervised coordinator credential
as the script prints, then verify with `sudo deploy/agentc/containment-suite.sh`
(add `--cargo-test` to also run the workspace tests under each profile).
`sudo deploy/agentc/host-setup.sh --uninstall` removes everything it created.
The two services are systemd units where systemd is the init system and LSB
`/etc/init.d` scripts otherwise (for example MX Linux with sysvinit), where the
proxy logs to `/var/log/agentc-egress.log`.

### Staging coordinator and UI verification

`agentc/staging.py` runs a disposable coordinator for supervised agents on
`127.0.0.1:18080` (the one loopback port the firewall opens to the agent
accounts besides the proxy). Run it as the owner, not root, from a clean,
committed tree (the service refuses clients built from a dirty tree):

```sh
cargo build --workspace --locked
deploy/agentc/staging.py up            # first run bootstraps; later runs restart
deploy/agentc/staging.py cli impl --json connect
deploy/agentc/staging.py credentials   # prints the sudo install commands
```

The first `up` creates the database under `~/.local/state/agentc-staging`
(0700; override with `--dir` or `AGENTC_STAGING_DIR`), a project with the
production autonomy policy whose remote is a local bare clone of `main`, three
agent credentials (`owner` interactive/write, `impl` supervised/write, `rev`
supervised/read) and the `staging-verifier` operator login. Every child
process runs without any `AGENT_COORDINATOR_*` or `COORDINATOR_*` variable,
so the production credential can never reach staging. `down` stops it;
`destroy --yes` deletes it.

A reviewer launch started with `--project <id>` gets a UI verification
environment when `/etc/agentc/supervisor.toml` has an entry for that project
below the `KEEP` line (kept when `host-setup.sh` re-runs):

```toml
[verification.<project-id>]
url = "http://127.0.0.1:18080"
# browser = true
```

The supervisor writes `$RUN/verification.json` (URL, browser and the path of
the reviewer-only login at `/var/lib/agentc/rev/verification/<project-id>.json`)
and sets `AGENTC_VERIFICATION` and `CHROME_BIN`. For this project,
`node scripts/verify_ui.mjs --path / --expect "text" --out "$RUN/ui"` signs in
and saves `screenshot.png` and `dom.html` as review evidence. Implementer
launches get no verification environment.
