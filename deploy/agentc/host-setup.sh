#!/usr/bin/env bash
# Prepares this Linux host for supervised agent launches (autonomy plan P2).
#
#   sudo SUPERVISOR=<path> CLI=<path> deploy/agentc/host-setup.sh
#   sudo deploy/agentc/host-setup.sh --uninstall
#
# Creates the agentc-impl / agentc-rev / agentc-egress accounts, root-owned
# pinned binaries and Rust toolchain under /opt/agentc, private per-role state
# under /var/lib/agentc, a read-only Git mirror, the egress proxy service and
# an nftables table that filters ONLY the two agent uids. Nothing else on the
# host changes. Idempotent: re-running re-pins binaries and reloads the rules.
# Harness logins and coordinator credentials stay manual (printed at the end).
set -euo pipefail

PREFIX=/opt/agentc
STATE=/var/lib/agentc
ETC=/etc/agentc
AGENTS=(agentc-impl agentc-rev)
PROXY_PORT=${PROXY_PORT:-3128}
STAGING_PORT=${STAGING_PORT:-18080}
REPO_URL=${REPO_URL:-https://github.com/marshallr12/agent_coordinator.git}
EXTRA_EGRESS=${EXTRA_EGRESS:-agents.sithbit.com}

# Refuses to run without root and the invoking owner account.
require_root() {
  [ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
  OWNER=${SUDO_USER:?run with sudo from the owner account}
  OWNER_HOME=$(getent passwd "$OWNER" | cut -d: -f6)
}

# Creates the system accounts; agents get no login shell.
create_users() {
  for user in "${AGENTS[@]}" agentc-egress; do
    id "$user" >/dev/null 2>&1 || useradd --system --user-group \
      --home-dir "$STATE/${user#agentc-}/home" --no-create-home \
      --shell /usr/sbin/nologin "$user"
  done
}

# Private 0700 state per role; the mirror and prefix are root-owned, readable.
create_dirs() {
  install -d -o root -g root -m 0755 "$PREFIX" "$PREFIX/bin" "$STATE" "$ETC"
  for user in "${AGENTS[@]}"; do
    local role=${user#agentc-}
    install -d -o "$user" -g "$user" -m 0700 "$STATE/$role"
    for sub in home claude-config codex-home coordinator cargo clones runs; do
      install -d -o "$user" -g "$user" -m 0700 "$STATE/$role/$sub"
    done
  done
}

# Copies the owner's current harness binaries and our binaries, root-owned.
install_binaries() {
  : "${SUPERVISOR:?set SUPERVISOR to a built agentc-supervisor}"
  : "${CLI:?set CLI to a built agent-coordinator}"
  install -o root -g root -m 0755 "$SUPERVISOR" "$PREFIX/bin/agentc-supervisor"
  install -o root -g root -m 0755 "$CLI" "$PREFIX/bin/agent-coordinator"
  install -o root -g root -m 0755 "$(readlink -f "$OWNER_HOME/.local/bin/claude")" "$PREFIX/bin/claude"
  install -o root -g root -m 0755 "$(readlink -f "$OWNER_HOME/.local/bin/codex")" "$PREFIX/bin/codex"
}

# Installs a read-only toolchain matching the owner's rustc (CI uses stable).
install_toolchain() {
  local version
  version=$(sudo -u "$OWNER" -H bash -lc 'rustc --version' | awk '{print $2}')
  if [ ! -x "$PREFIX/cargo/bin/rustup" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$PREFIX/rustup-init.sh"
    RUSTUP_HOME=$PREFIX/rustup CARGO_HOME=$PREFIX/cargo sh "$PREFIX/rustup-init.sh" -y \
      --no-modify-path --profile minimal --default-toolchain none
    rm -f "$PREFIX/rustup-init.sh"
  fi
  RUSTUP_HOME=$PREFIX/rustup CARGO_HOME=$PREFIX/cargo "$PREFIX/cargo/bin/rustup" \
    toolchain install "$version" --profile minimal -c rustfmt -c clippy
  RUSTUP_HOME=$PREFIX/rustup CARGO_HOME=$PREFIX/cargo "$PREFIX/cargo/bin/rustup" default "$version"
  chown -R root:root "$PREFIX/rustup" "$PREFIX/cargo"
  chmod -R go-w,a+rX "$PREFIX/rustup" "$PREFIX/cargo"
}

# Keeps a read-only mirror agents clone from (no network needed per launch).
refresh_mirror() {
  if [ -d "$STATE/mirror.git" ]; then
    git -C "$STATE/mirror.git" fetch --prune --quiet
  else
    git clone --mirror --quiet "$REPO_URL" "$STATE/mirror.git"
  fi
  chown -R root:root "$STATE/mirror.git"
  chmod -R go-w,a+rX "$STATE/mirror.git"
  # Git refuses repositories owned by another uid; trust exactly the mirror
  # (never '*', plan §2.3) so agent accounts can clone from it.
  git config --system --get-all safe.directory | grep -qx "$STATE/mirror.git" ||
    git config --system --add safe.directory "$STATE/mirror.git"
}

# Writes the host config: defaults shown commented, pins and extras set.
write_config() {
  local claude codex
  local as_agent=(sudo -u agentc-impl env -i HOME="$STATE/impl/home")
  claude=$("${as_agent[@]}" "$PREFIX/bin/claude" --version | awk '{print $1}')
  codex=$("${as_agent[@]}" "$PREFIX/bin/codex" --version | awk '{print $NF}')
  cat > "$ETC/supervisor.toml" <<EOF
# agentc-supervisor host configuration. Every entry has an in-code default;
# commented lines show those defaults. Written by deploy/agentc/host-setup.sh.
# bin_dir = "/opt/agentc/bin"
# state_dir = "/var/lib/agentc"
# implementer_user = "agentc-impl"
# reviewer_user = "agentc-rev"
# toolchain_dir = "/opt/agentc"
egress_listen = "127.0.0.1:$PROXY_PORT"
egress_allow_extra = ["$EXTRA_EGRESS"]

[pinned]
claude = "$claude"
codex = "$codex"
EOF
  chmod 0644 "$ETC/supervisor.toml"
}

# Agent uids may reach loopback only on the proxy, the staging coordinator
# and the ephemeral range (tests bind port 0); everything else, including
# DNS and every non-loopback address, is rejected.
install_firewall() {
  cat > "$ETC/agentc.nft" <<EOF
table inet agentc
delete table inet agentc
table inet agentc {
  chain output {
    type filter hook output priority filter; policy accept;
    meta skuid { agentc-impl, agentc-rev } jump agents
  }
  chain agents {
    oifname "lo" tcp dport { $PROXY_PORT, $STAGING_PORT, 32768-60999 } accept
    counter reject with icmpx admin-prohibited
  }
}
EOF
  install_unit agentc-firewall.service <<EOF
[Unit]
Description=Egress filter for supervised agent accounts
Before=agentc-egress.service
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/nft -f $ETC/agentc.nft
ExecStop=/usr/sbin/nft delete table inet agentc
[Install]
WantedBy=multi-user.target
EOF
}

# Runs the allowlisting proxy as its own unprivileged account.
install_egress_service() {
  install_unit agentc-egress.service <<EOF
[Unit]
Description=Egress allowlist proxy for supervised agents
After=network-online.target agentc-firewall.service
Wants=network-online.target
[Service]
User=agentc-egress
ExecStart=$PREFIX/bin/agentc-supervisor egress-proxy
Restart=always
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
[Install]
WantedBy=multi-user.target
EOF
}

# Writes a unit from stdin and (re)starts it.
install_unit() {
  cat > "/etc/systemd/system/$1"
  systemctl daemon-reload
  systemctl enable --quiet "$1"
  systemctl restart "$1"
}

# Removes everything this script created.
uninstall() {
  systemctl disable --now agentc-egress.service agentc-firewall.service 2>/dev/null || true
  rm -f /etc/systemd/system/agentc-egress.service /etc/systemd/system/agentc-firewall.service
  systemctl daemon-reload
  nft delete table inet agentc 2>/dev/null || true
  git config --system --unset-all safe.directory "^$STATE/mirror.git\$" 2>/dev/null || true
  for user in "${AGENTS[@]}" agentc-egress; do userdel "$user" 2>/dev/null || true; done
  rm -rf "$PREFIX" "$STATE" "$ETC"
  echo "agentc host setup removed"
}

# Prints the manual steps that remain (reserved bootstrap).
next_steps() {
  cat <<EOF
Host ready. Manual steps (reserved bootstrap, once per role):
  sudo -u agentc-impl -H env HOME=$STATE/impl/home CLAUDE_CONFIG_DIR=$STATE/impl/claude-config \\
    HTTPS_PROXY=http://127.0.0.1:$PROXY_PORT $PREFIX/bin/claude auth login
  sudo -u agentc-impl -H env HOME=$STATE/impl/home CODEX_HOME=$STATE/impl/codex-home \\
    HTTPS_PROXY=http://127.0.0.1:$PROXY_PORT $PREFIX/bin/codex login
  (repeat for agentc-rev with rev/ paths)
Coordinator credentials: issue class=supervised (impl: write, rev: read) in the
dashboard and save each credentials.toml as $STATE/<role>/coordinator/credentials.toml (0600).
Then run: sudo deploy/agentc/containment-suite.sh
EOF
}

main() {
  require_root
  if [ "${1:-}" = "--uninstall" ]; then uninstall; return; fi
  create_users
  create_dirs
  install_binaries
  install_toolchain
  refresh_mirror
  write_config
  install_firewall
  install_egress_service
  next_steps
}

main "$@"
