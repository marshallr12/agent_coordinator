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
