#!/usr/bin/env bash
# Prepares this Linux host for supervised agent launches (autonomy plan P2).
#
#   sudo SUPERVISOR=<path> CLI=<path> [PUSH=<path>] deploy/agentc/host-setup.sh
#   sudo deploy/agentc/host-setup.sh --uninstall
#
# Creates the agentc-impl / agentc-rev / agentc-egress / agentc-push accounts,
# root-owned pinned binaries and Rust toolchain under /opt/agentc, private
# per-role state under /var/lib/agentc, a read-only Git mirror, the egress
# proxy service and an nftables table that filters ONLY the two agent uids.
# For the candidate-push helper it also writes /etc/agentc/push.toml when
# absent and hands an existing push App key to agentc-push; it never creates,
# prints or copies that key. Nothing else on the host changes. Idempotent:
# re-running re-pins binaries and reloads the rules. Harness logins,
# coordinator credentials and the push App key stay manual (printed at the end).
set -euo pipefail

PREFIX=/opt/agentc
STATE=/var/lib/agentc
ETC=/etc/agentc
AGENTS=(agentc-impl agentc-rev)
PROXY_PORT=${PROXY_PORT:-3128}
STAGING_PORT=${STAGING_PORT:-18080}
REPO_URL=${REPO_URL:-https://github.com/marshallr12/agent_coordinator.git}
EXTRA_EGRESS=${EXTRA_EGRESS:-agents.sithbit.com}
# The candidate-push App (decision U17) and the one repository it writes;
# only a missing /etc/agentc/push.toml is written from these.
PUSH_APP_ID=${PUSH_APP_ID:-5168037}
PUSH_INSTALLATION_ID=${PUSH_INSTALLATION_ID:-167333814}
PUSH_REPOSITORY=${PUSH_REPOSITORY:-$REPO_URL}
PUSH_USER=agentc-push
PUSH_KEY=$ETC/push-app.pem
KEEP="# --- entries below this line are kept when host-setup.sh re-runs ---"

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
  create_push_user
}

# The push helper's account: its own group, no home, no login shell (it
# alone may read the push App key). An existing account must already be so.
create_push_user() {
  id "$PUSH_USER" >/dev/null 2>&1 || useradd --system --user-group \
    --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin "$PUSH_USER"
  local problem
  problem=$(push_user_problem)
  [ -z "$problem" ] ||
    { echo "refusing: $PUSH_USER $problem; owner repair required (or userdel it and re-run)" >&2; exit 1; }
}

# Prints why the push account is unsafe, or nothing: root's uid, a uid shared
# with another agentc account, a login shell, or any group but its own.
push_user_problem() {
  local uid shell other groups
  uid=$(id -u "$PUSH_USER") shell=$(getent passwd "$PUSH_USER" | cut -d: -f7)
  groups=$(id -nG "$PUSH_USER")
  [ "$uid" != 0 ] || { echo "has uid 0"; return; }
  for other in agentc-impl agentc-rev agentc-egress; do
    [ "$(id -u "$other" 2>/dev/null)" != "$uid" ] || { echo "shares uid $uid with $other"; return; }
  done
  case $shell in */nologin|*/false) ;; *) echo "has login shell '$shell'"; return ;; esac
  [ "$groups" = "$PUSH_USER" ] || echo "is in groups '$groups', not only its own"
}

# Validate root's destination chain before writing. Never follow an agent's
# symlink or recursively chown an existing role tree during migration.
protected_chain() {
  local path=$1 mode
  while :; do
    if [ -L "$path" ] || [ ! -d "$path" ] || [ "$(stat -c %u -- "$path")" != 0 ]; then
      echo "refusing unsafe root directory: $path; owner repair required" >&2; exit 1
    fi
    mode=$(stat -c %a -- "$path")
    if (( (8#$mode & 0022) != 0 )); then
      echo "refusing writable root directory: $path; owner repair required" >&2; exit 1
    fi
    [ "$path" != / ] || break
    path=${path%/*}; [ -n "$path" ] || path=/
  done
}

refuse_symlink() {
  if [ -L "$1" ]; then echo "refusing: $1 is a symlink; owner repair required" >&2; exit 1; fi
}

# The owner installs Bubblewrap with distribution packages. Never grant it
# setuid privileges or relax kernel policy automatically to make a probe pass.
require_bubblewrap() {
  local path=/usr/bin/bwrap mode
  if [ ! -f "$path" ] || [ -L "$path" ] || [ ! -x "$path" ] || [ "$(stat -c %u -- "$path")" != 0 ]; then
    echo "install root-owned, non-setuid /usr/bin/bwrap (Bubblewrap 0.8 or newer) before setup" >&2; exit 1
  fi
  protected_chain /usr/bin
  mode=$(stat -c %a -- "$path")
  if (( (8#$mode & 06022) != 0 )); then
    echo "Bubblewrap must be non-setuid and not group/world-writable; owner repair required" >&2; exit 1
  fi
}

# Root-owned parents make protected children irreplaceable. Each writable
# child belongs to its role separately. Existing real role directories can be
# sealed without descending into their agent-controlled contents.
create_dirs() {
  local path user role child
  for path in "$PREFIX" "$PREFIX/bin" "$STATE" "$ETC"; do
    refuse_symlink "$path"
    protected_chain "${path%/*}"
    if [ -e "$path" ]; then protected_chain "$path"; fi
    install -d -o root -g root -m 0755 "$path"
  done
  # launch-root's per-launch helper directories: root-only writes, traversable
  # by the implementer and helper accounts.
  refuse_symlink "$STATE/push"
  install -d -o root -g root -m 0711 "$STATE/push"
  for user in "${AGENTS[@]}"; do
    role=$STATE/${user#agentc-}
    refuse_symlink "$role"
    if [ -e "$role" ]; then
      [ -d "$role" ] || { echo "refusing non-directory: $role" >&2; exit 1; }
      case $(stat -c %u -- "$role") in
        0|"$(id -u "$user")") ;;
        *) echo "refusing unexpected owner: $role; owner repair required" >&2; exit 1 ;;
      esac
    fi
    install -d -o root -g "$user" -m 0750 "$role"
    protected_chain "$role"
    for child in home codex-home coordinator clones runs verification downloads; do
      path=$role/$child
      refuse_symlink "$path"
      if [ -e "$path" ]; then
        [ -d "$path" ] && [ "$(stat -c %u -- "$path")" = "$(id -u "$user")" ] || {
          echo "refusing unexpected writable directory: $path; owner repair required" >&2; exit 1;
        }
      fi
      install -d -o "$user" -g "$user" -m 0700 "$path"
    done
    path=$role/claude-config
    refuse_symlink "$path"
    [ ! -e "$path" ] || [ -d "$path" ] || { echo "refusing non-directory: $path" >&2; exit 1; }
    install -d -o root -g "$user" -m 0750 "$path"
    protected_chain "$path"
    # Do not guess what to preserve from arbitrary old harness state.
    local entry
    while IFS= read -r -d '' entry; do
      case ${entry##*/} in
        settings.json|CLAUDE.md|.credentials.json)
          refuse_symlink "$entry"
          [ -f "$entry" ] && [ "$(stat -c %h -- "$entry")" = 1 ] || {
            echo "refusing non-regular or linked Claude file; owner repair required: $path" >&2; exit 1;
          }
          ;;
        *) echo "refusing unexpected Claude config entry; owner cleanup required: $path" >&2; exit 1 ;;
      esac
    done < <(find "$path" -mindepth 1 -maxdepth 1 -print0)
    entry=$path/.credentials.json
    refuse_symlink "$entry"
    if [ -e "$entry" ]; then
      [ -f "$entry" ] && [ "$(stat -c %h -- "$entry")" = 1 ] &&
        [ "$(stat -c %u -- "$entry")" = "$(id -u "$user")" ] &&
        [ "$(stat -c %a -- "$entry")" = 600 ] || {
          echo "refusing unsafe credential file; owner repair required: $entry" >&2; exit 1;
        }
    else
      install -o "$user" -g "$user" -m 0600 /dev/null "$entry"
    fi
  done
}

# All destinations now have sealed parents. Rename fresh seed inodes rather
# than truncating old files an agent might still have open or hard-linked.
install_seeds() {
  local user role name temp path
  for user in "${AGENTS[@]}"; do
    role=${user#agentc-}
    [ "$role" = impl ] && name=implementer || name=reviewer
    path=$STATE/$role/claude-config
    protected_chain "$path"
    temp=$(mktemp "$path/.seed.XXXXXXXX")
    "$PREFIX/bin/agentc-supervisor" settings --role "$name" > "$temp"
    chown root:root "$temp"; chmod 0444 "$temp"
    mv -fT -- "$temp" "$path/settings.json"
    temp=$(mktemp "$path/.seed.XXXXXXXX")
    chown root:root "$temp"; chmod 0444 "$temp"
    mv -fT -- "$temp" "$path/CLAUDE.md"
  done
  protected_chain "$ETC"
  temp=$(mktemp "$ETC/.cargo-seed.XXXXXXXX")
  printf '[net]\ngit-fetch-with-cli = false\n' > "$temp"
  chown root:root "$temp"; chmod 0444 "$temp"
  mv -fT -- "$temp" "$ETC/cargo-config.toml"
}

# Copies the owner's current harness binaries and our binaries, root-owned.
install_binaries() {
  : "${SUPERVISOR:?set SUPERVISOR to a built agentc-supervisor}"
  : "${CLI:?set CLI to a built agent-coordinator}"
  local push=${PUSH:-${SUPERVISOR%/*}/agentc-push}
  [ -f "$push" ] && [ -x "$push" ] || {
    echo "set PUSH to a built agentc-push (default: beside SUPERVISOR)" >&2; exit 1;
  }
  install -o root -g root -m 0755 "$SUPERVISOR" "$PREFIX/bin/agentc-supervisor"
  install -o root -g root -m 0755 "$CLI" "$PREFIX/bin/agent-coordinator"
  install -o root -g root -m 0755 "$push" "$PREFIX/bin/agentc-push"
  install -o root -g root -m 0755 "$(readlink -f "$OWNER_HOME/.local/bin/claude")" "$PREFIX/bin/claude"
  install -o root -g root -m 0755 "$(readlink -f "$OWNER_HOME/.local/bin/codex")" "$PREFIX/bin/codex"
  install_node
}

# Pins node (the repo's UI scripts drive the browser with it) from $NODE or
# the owner's newest nvm install; skipped with a note when neither exists.
install_node() {
  local node=${NODE:-}
  [ -n "$node" ] || node=$(ls -d "$OWNER_HOME"/.nvm/versions/node/*/bin/node 2>/dev/null | sort -V | tail -n 1)
  if [ -z "$node" ]; then echo "note: no node found; set NODE=<path> for UI verification" >&2; return; fi
  install -o root -g root -m 0755 "$(readlink -f "$node")" "$PREFIX/bin/node"
}

# The system headless browser offered to verifying reviewers (plan M2).
detect_browser() {
  local candidate
  for candidate in /usr/bin/chromium /usr/bin/chromium-browser /usr/bin/google-chrome; do
    [ -x "$candidate" ] && { readlink -f "$candidate"; return; }
  done
  echo /usr/bin/chromium
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
# Everything below the KEEP marker (the host owner's verification entries)
# survives re-runs.
write_config() {
  local claude codex kept=""
  local as_agent=(sudo -u agentc-impl env -i HOME="$STATE/impl/home")
  claude=$("${as_agent[@]}" "$PREFIX/bin/claude" --version | awk '{print $1}')
  codex=$("${as_agent[@]}" "$PREFIX/bin/codex" --version | awk '{print $NF}')
  [ -f "$ETC/supervisor.toml" ] && kept=$(sed -n "/^$KEEP\$/,\$p" "$ETC/supervisor.toml" | tail -n +2)
  cat > "$ETC/supervisor.toml" <<EOF
# agentc-supervisor host configuration. Every entry has an in-code default;
# commented lines show those defaults. Written by deploy/agentc/host-setup.sh.
# bin_dir = "/opt/agentc/bin"
# state_dir = "/var/lib/agentc"
# implementer_user = "agentc-impl"
# reviewer_user = "agentc-rev"
# toolchain_dir = "/opt/agentc"
# cargo_config_seed = "/etc/agentc/cargo-config.toml"
# bubblewrap = "/usr/bin/bwrap"
# browser = "/usr/bin/chromium"
browser = "$(detect_browser)"
egress_listen = "127.0.0.1:$PROXY_PORT"
egress_allow_extra = ["$EXTRA_EGRESS"]
# egress_probe_target = "1.1.1.1:443"
# egress_probe_blocked_host = "blocked.invalid"

[pinned]
claude = "$claude"
codex = "$codex"

# The candidate-push helper launch-root runs beside implementer launches.
[push_helper]
program = "$PREFIX/bin/agentc-push"
# config = "$ETC/push.toml"
# user = "$PUSH_USER"

$KEEP
EOF
  if [ -n "$kept" ]; then
    printf '%s\n' "$kept" >> "$ETC/supervisor.toml"
  else
    cat >> "$ETC/supervisor.toml" <<EOF
# UI verification per coordinator project (plan M2); the reviewer's test login
# goes to $STATE/rev/verification/<project-id>.json (agentc-rev, 0600).
# deploy/agentc/staging.py credentials prints the commands for staging.
# [verification.<project-id>]
# url = "http://127.0.0.1:$STAGING_PORT"
# browser = true

# Shadow mode (plan P3a): \`agentc-supervisor shadow\` polls the read-only
# \`next\` endpoint with a read-access host credential and logs would-launch
# records with cost estimates; \`shadow-report\` summarises the log.
# [shadow]
# credential_file = "$ETC/shadow-credentials.toml"
# origin = ""                    # empty: the file's first [[credentials]] entry
# allow_insecure_loopback = false
# projects = []                  # empty: every project the credential lists
# poll_seconds = 60
# log = "$STATE/shadow/would-launch.jsonl"
# [shadow.implementer]           # per-launch token profile (estimates); any
# harness = "claude"             # key left out keeps the role's default
# model = "claude-opus-5-5"
# effort = "high"
# input_tokens = 4000000
# cached_share = 0.9
# output_tokens = 80000
# [shadow.reviewer]              # same keys and defaults, except
# input_tokens = 1200000
# output_tokens = 20000
# [shadow.prices.<model-id>]     # USD per million tokens, all three keys; added
# input = 4.0                    # to the defaults (claude-opus-5-5 4/0.2/20,
# cached_input = 0.2             # claude-sonnet-5 2/0.2/10,
# output = 20.0                  # claude-haiku-4-5 1/0.1/5)
EOF
  fi
  chmod 0644 "$ETC/supervisor.toml"
}

# Writes the push helper's configuration unless one exists: root-owned, mode
# 0640, group agentc-push (launch-root refuses a configuration that is not
# root-owned or is group/world-writable). An existing file is checked, never
# rewritten.
write_push_config() {
  local path=$ETC/push.toml temp
  refuse_symlink "$path"
  if [ -e "$path" ]; then check_push_config "$path"; return; fi
  temp=$(mktemp "$ETC/.push.XXXXXXXX")
  if ! push_config_file "$temp" || ! mv -nT -- "$temp" "$path"; then
    rm -f -- "$temp"; echo "cannot write $path" >&2; exit 1
  fi
  # Left only when mv -n found a file already at the path.
  rm -f -- "$temp"
}

# Fills $1 with the push configuration, root:agentc-push 0640. Each step is
# chained, so a failure returns non-zero even where `set -e` is suspended.
push_config_file() {
  cat > "$1" <<EOF &&
# agentc-push configuration. Written by deploy/agentc/host-setup.sh when
# absent; never overwritten. See crates/integrator/push.example.toml.
app_id = $PUSH_APP_ID
installation_id = $PUSH_INSTALLATION_ID
repository = "$PUSH_REPOSITORY"
# api_base = "https://api.github.com"
# private_key = "$PUSH_KEY"
# max_bundle_bytes = 536870912
EOF
    chown "root:$PUSH_USER" "$1" && chmod 0640 "$1"
}

# Refuses an existing push configuration the helper could not use safely:
# not a single-link regular file, not root-owned, group/world-writable, or
# unreadable by the helper account.
check_push_config() {
  local path=$1
  [ -f "$path" ] && [ "$(stat -c %h -- "$path")" = 1 ] && [ "$(stat -c %u -- "$path")" = 0 ] &&
    (( (8#$(stat -c %a -- "$path") & 0022) == 0 )) &&
    sudo -u "$PUSH_USER" test -r "$path" || {
      echo "refusing $path: needs a root-owned, single-link file that is not group/world-writable and that $PUSH_USER can read; owner repair required" >&2; exit 1;
    }
}

# Hands an existing push App key to the helper account alone (mode 0400, set
# before the owner changes). The key's bytes are never read, printed or
# copied; a missing key is left for the owner (see next_steps).
secure_push_key() {
  refuse_symlink "$PUSH_KEY"
  [ -e "$PUSH_KEY" ] || return 0
  [ -f "$PUSH_KEY" ] && [ "$(stat -c %h -- "$PUSH_KEY")" = 1 ] || {
    echo "refusing non-regular or linked $PUSH_KEY; owner repair required" >&2; exit 1;
  }
  chmod 0400 -- "$PUSH_KEY"
  chown "$PUSH_USER:$PUSH_USER" -- "$PUSH_KEY"
}

# Agent uids may reach loopback only on the proxy, the staging coordinator
# and the ephemeral range (tests bind port 0); everything else, including
# DNS and every non-loopback address, is rejected. Claude launches run in
# their own network namespace and reach only the relayed proxy and staging
# ports (R-P3b.4); the ephemeral range remains for Codex launches.
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
  install_service agentc-firewall \
    "/usr/sbin/nft -f $ETC/agentc.nft" "/usr/sbin/nft delete table inet agentc"
}

# Runs the allowlisting proxy as its own unprivileged account.
install_egress_service() {
  install_service agentc-egress "$PREFIX/bin/agentc-supervisor egress-proxy" ""
}

# True when systemd is the running init (MX Linux and others may use sysvinit).
has_systemd() { [ -d /run/systemd/system ]; }

# Installs, enables and (re)starts a service under whichever init is running.
# An empty stop command means a long-running daemon; otherwise a oneshot.
install_service() {
  if has_systemd; then systemd_unit "$@"; else sysv_script "$@"; fi
}

# systemd: a oneshot with a stop command, or a restarting sandboxed daemon.
systemd_unit() {
  local name=$1 start=$2 stop=$3 body
  if [ -n "$stop" ]; then
    body="Type=oneshot
RemainAfterExit=yes
ExecStart=$start
ExecStop=$stop"
  else
    body="User=agentc-egress
ExecStart=$start
Restart=always
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes"
  fi
  printf '[Unit]\nDescription=%s (agentc)\nAfter=network-online.target\n[Service]\n%s\n[Install]\nWantedBy=multi-user.target\n' \
    "$name" "$body" > "/etc/systemd/system/$name.service"
  systemctl daemon-reload
  systemctl enable --quiet "$name.service"
  systemctl restart "$name.service"
}

# sysvinit: an LSB script; the daemon runs via start-stop-daemon as its own
# account and logs to /var/log/<name>.log (no automatic restart on crash).
sysv_script() {
  local name=$1 start=$2 stop=$3 pid=/run/$1.pid log=/var/log/$1.log
  rm -f "/etc/systemd/system/$name.service"
  local run_start="$start" run_stop="$stop" status="nft list table inet agentc >/dev/null"
  if [ -z "$stop" ]; then
    install -o agentc-egress -g agentc-egress -m 0640 /dev/null "$log"
    run_start="start-stop-daemon --start --background --make-pidfile --pidfile $pid --chuid agentc-egress --startas /bin/sh -- -c 'exec $start >>$log 2>&1'"
    run_stop="start-stop-daemon --stop --pidfile $pid --retry 5; rm -f $pid"
    status="start-stop-daemon --status --pidfile $pid"
  fi
  cat > "/etc/init.d/$name" <<EOF
#!/bin/sh
### BEGIN INIT INFO
# Provides:          $name
# Required-Start:    \$network \$remote_fs
# Required-Stop:     \$network \$remote_fs
# Default-Start:     2 3 4 5
# Default-Stop:      0 1 6
# Short-Description: $name (supervised agent containment)
### END INIT INFO
case "\$1" in
  start) $run_start ;;
  stop) $run_stop ;;
  restart|force-reload) "\$0" stop; "\$0" start ;;
  status) $status ;;
  *) echo "usage: \$0 {start|stop|restart|status}"; exit 2 ;;
esac
EOF
  chmod 0755 "/etc/init.d/$name"
  update-rc.d "$name" defaults >/dev/null
  "/etc/init.d/$name" restart
}

# Stops and removes a service under either init.
remove_service() {
  local name=$1
  if has_systemd; then
    systemctl disable --now "$name.service" 2>/dev/null || true
    rm -f "/etc/systemd/system/$name.service"
    systemctl daemon-reload
  elif [ -x "/etc/init.d/$name" ]; then
    "/etc/init.d/$name" stop || true
    update-rc.d -f "$name" remove >/dev/null
    rm -f "/etc/init.d/$name" "/var/log/$name.log"
  fi
}

# Removes what this script installs, by explicit path, so files other
# installers keep under the same directories (the integrator's binary, state,
# configuration and keys) survive. The push App key is the owner's: it is
# kept, handed back to root (0400). Each shared parent goes only once empty.
uninstall() {
  remove_service agentc-egress
  remove_service agentc-firewall
  nft delete table inet agentc 2>/dev/null || true
  git config --system --unset-all safe.directory "^$STATE/mirror.git\$" 2>/dev/null || true
  keep_push_key
  for user in "${AGENTS[@]}" agentc-egress "$PUSH_USER"; do userdel "$user" 2>/dev/null || true; done
  remove_own_paths
  rmdir "$PREFIX/bin" "$PREFIX" "$STATE" "$ETC" 2>/dev/null || true
  [ -z "$KEY_NOTE" ] || echo "$KEY_NOTE"
  echo "agentc host setup removed"
}

# Deletes this script's own files and directories, and the containment
# suite's leftover bin directories, without following symlinks.
remove_own_paths() {
  local name
  for name in agentc-supervisor agent-coordinator agentc-push claude codex node; do
    rm -f -- "$PREFIX/bin/$name"
  done
  rm -rf -- "$PREFIX/rustup" "$PREFIX/cargo" "$PREFIX/rustup-init.sh" "$PREFIX"/suite-bin.*
  for name in impl rev push mirror.git shadow; do rm -rf -- "${STATE:?}/$name"; done
  for name in supervisor.toml cargo-config.toml agentc.nft push.toml; do
    rm -f -- "$ETC/$name"
  done
}

# Returns an existing push App key to root before its account is deleted,
# once $ETC's whole chain is root-owned and not group/world-writable. A key
# that is a symlink or not a single-link regular file is left untouched.
# Sets KEY_NOTE to what uninstall reports about the key.
keep_push_key() {
  KEY_NOTE=
  [ -e "$ETC" ] || [ -L "$ETC" ] || return 0
  protected_chain "$ETC"
  if [ -L "$PUSH_KEY" ]; then
    KEY_NOTE="left $PUSH_KEY untouched: it is a symlink; owner repair required"
  elif [ -f "$PUSH_KEY" ] && [ "$(stat -c %h -- "$PUSH_KEY")" = 1 ]; then
    chown -h root:root -- "$PUSH_KEY"; chmod 0400 -- "$PUSH_KEY"
    KEY_NOTE="kept $PUSH_KEY (root:root 0400); delete it yourself to retire the key"
  elif [ -e "$PUSH_KEY" ]; then
    KEY_NOTE="left $PUSH_KEY untouched: not a single-link regular file; owner repair required"
  fi
}

# Prints the manual steps that remain (reserved bootstrap).
next_steps() {
  cat <<EOF
Host seeds installed. Manual steps (reserved bootstrap, once per role):
  Authenticate Claude in a separate private owner-controlled bootstrap directory.
  As the owner, install only the resulting .credentials.json (agentc-impl, 0600)
  at $STATE/impl/claude-config/.credentials.json. Do not copy other harness state
  or make claude-config writable to enable login. Credential refresh must update
  this file in place; verify that behavior with the pinned harness.
  sudo -u agentc-impl -H env HOME=$STATE/impl/home CODEX_HOME=$STATE/impl/codex-home \\
    HTTPS_PROXY=http://127.0.0.1:$PROXY_PORT $PREFIX/bin/codex login
  (repeat for agentc-rev with rev/ paths)
Coordinator credentials: issue class=supervised (impl: write, rev: read) in the
dashboard and save each credentials.toml as $STATE/<role>/coordinator/credentials.toml (0600).
Staging: deploy/agentc/staging.py up (as the owner), then run the commands
"deploy/agentc/staging.py credentials" prints and add its [verification.<project-id>]
entry below the KEEP line in $ETC/supervisor.toml.
Claude requires unprivileged user/PID namespaces, nested-userns disabling and
close_range(CLOSE_RANGE_CLOEXEC) kernel support. Preflight fails closed if these
are unavailable. No kernel policy changes or unsandboxed fallback are automatic.
Then run: sudo deploy/agentc/containment-suite.sh
That suite uses a mock shell; authenticated Claude/browser compatibility and
credential refresh still require separate owner verification.
EOF
  push_steps
}

# Prints the candidate-push helper's remaining step and the implementer
# launch command.
push_steps() {
  if [ -e "$PUSH_KEY" ]; then
    echo "Push App key: $PUSH_KEY is now $PUSH_USER-only (0400)."
  else
    cat <<EOF
Push App key: none at $PUSH_KEY. Implementer launches can start, but every
candidate push fails until the owner places the push App's private key there
(root, 0400; never in the repository) and re-runs this script, which hands it
to $PUSH_USER.
EOF
  fi
  cat <<EOF
Push helper configuration: $ETC/push.toml (written only when absent).
Run implementer launches as root through launch-root, which starts the helper:
  sudo $PREFIX/bin/agentc-supervisor launch-root --role implementer --harness claude \\
    --clone <clone> --run <run> --task <task-id>
EOF
}

main() {
  require_root
  if [ "${1:-}" = "--uninstall" ]; then uninstall; return; fi
  require_bubblewrap
  create_users
  create_dirs
  install_binaries
  install_seeds
  install_toolchain
  refresh_mirror
  write_config
  write_push_config
  secure_push_key
  install_firewall
  install_egress_service
  next_steps
}

main "$@"
