#!/usr/bin/env bash
# Prepares this Linux host for supervised agent launches (autonomy plan P2).
#
#   sudo SUPERVISOR=<path> CLI=<path> [PUSH=<path>] [APPARMOR_BWRAP=1] deploy/agentc/host-setup.sh
#   sudo deploy/agentc/host-setup.sh --uninstall
#
# Creates the agentc-impl / agentc-rev / agentc-egress / agentc-push accounts,
# root-owned pinned binaries and Rust toolchain under /opt/agentc, private
# per-role state under /var/lib/agentc, a read-only Git mirror, the egress
# proxy service and an nftables table that filters ONLY the two agent uids.
# For the candidate-push helper it also writes /etc/agentc/push.toml when
# absent and hands an existing push App key to agentc-push; it never creates,
# prints or copies that key. Nothing else on the host changes. Idempotent:
# re-running re-pins binaries and reloads the rules. Claude tokens, Codex
# logins, coordinator credentials and the push App key stay manual (printed at
# the end); an installed Claude token is held at root:<role> 0440.
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
# Opt-in (Ubuntu's AppArmor userns restriction): an agentc-only Bubblewrap
# copy whose own profile lets the reviewer's nested sandbox start.
APPARMOR_BWRAP=${APPARMOR_BWRAP:-0}
BWRAP_GROUP=agentc-bwrap
BWRAP_COPY=$PREFIX/bin/bwrap
BWRAP_PROFILE=/etc/apparmor.d/agentc-bwrap
# Shared temp directories --uninstall clears of agent-owned files.
TEMP_DIRS=(/tmp /var/tmp /dev/shm)
# The placeholder token containment-suite.sh installs for a run (its
# DUMMY_TOKEN); one left behind is warned about, never adopted silently.
SUITE_DUMMY_TOKEN=agentc-suite-dummy-token
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
    seal_claude_config "$user" "$role/claude-config"
    secure_claude_token "$user" "$role/claude-token"
  done
}

# Seals role account $1's Claude configuration directory $2 (root:<role>
# 0750) and admits only the root-owned seeds, plus the retired login file,
# which it removes. Never guesses what to preserve from other harness state.
seal_claude_config() {
  local user=$1 path=$2 entry
  refuse_symlink "$path"
  [ ! -e "$path" ] || [ -d "$path" ] || { echo "refusing non-directory: $path" >&2; exit 1; }
  install -d -o root -g "$user" -m 0750 "$path"
  protected_chain "$path"
  while IFS= read -r -d '' entry; do
    case ${entry##*/} in
      settings.json|CLAUDE.md|.credentials.json) require_single_file "$entry" ;;
      *) echo "refusing unexpected Claude config entry; owner cleanup required: $path" >&2; exit 1 ;;
    esac
  done < <(find "$path" -mindepth 1 -maxdepth 1 -print0)
  remove_retired_login "$path/.credentials.json"
}

# Exits unless $1 is a single-link regular file and not a symlink, so root
# never writes through an entry an agent could have redirected.
require_single_file() {
  refuse_symlink "$1"
  [ -f "$1" ] && [ "$(stat -c %h -- "$1")" = 1 ] || {
    echo "refusing non-regular or linked file; owner repair required: $1" >&2; exit 1;
  }
}

# Removes a leftover `claude auth login` file (decision U27): Claude cannot
# refresh it, because its refresh lock needs a writable claude-config. Agent
# accounts use the owner-installed claude-token instead.
remove_retired_login() {
  [ -e "$1" ] || [ -L "$1" ] || return 0
  require_single_file "$1"
  zero_and_remove "$1"
  echo "removed retired Claude login $1; agent accounts now use claude-token" >&2
}

# Overwrites single-link regular file $1 with zeros and deletes it; where
# shred is missing, only deletes it.
zero_and_remove() {
  if command -v shred >/dev/null; then shred -n 0 -z -u -- "$1"; else rm -f -- "$1"; fi
}

# Holds role account $1's owner-installed Claude token $2 at root:<role>
# 0440, which the supervisor requires: the role can read it, never change
# it. Only a root-owned token is adopted, since the owner installs it with
# `sudo install -o root`; any other owner gets an owner-repair refusal. A
# missing token is left for the owner (see next_steps).
secure_claude_token() {
  local user=$1 path=$2
  refuse_symlink "$path"
  [ -e "$path" ] || return 0
  require_single_file "$path"
  owned_by_root "$path" ||
    { echo "refusing $path: not root-owned (install it with sudo install -o root); owner repair required" >&2; exit 1; }
  warn_suite_dummy "$path"
  chmod 0400 -- "$path"
  chown "root:$user" -- "$path"
  chmod 0440 -- "$path"
}

# True when $1 is owned by uid 0.
owned_by_root() { [ "$(stat -c %u -- "$1")" = 0 ]; }

# Warns when token $1 is the containment suite's leftover placeholder. Only
# a file of exactly the placeholder's size is compared, so a real token is
# read only at that size and never printed.
warn_suite_dummy() {
  [ "$(stat -c %s -- "$1")" = "${#SUITE_DUMMY_TOKEN}" ] &&
    printf '%s' "$SUITE_DUMMY_TOKEN" | cmp -s - "$1" || return 0
  echo "warning: $1 is the containment suite's leftover dummy token; Claude launches cannot authenticate until you replace it with a real setup-token" >&2
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
  [ -n "$node" ] || node=$(ls -d "$OWNER_HOME"/.nvm/versions/node/*/bin/node 2>/dev/null | sort -V | tail -n 1 || true)
  if [ -z "$node" ]; then echo "note: no node found; set NODE=<path> for UI verification" >&2; return; fi
  install -o root -g root -m 0755 "$(readlink -f "$node")" "$PREFIX/bin/node"
}

# With APPARMOR_BWRAP=1, copies the distribution Bubblewrap to an executable
# only the role accounts may run and confines it with its own AppArmor
# profile; otherwise only notes when the host restriction will refuse the
# reviewer's nested sandbox. Never changes the host-wide policy.
install_apparmor_bwrap() {
  local user
  if [ "$APPARMOR_BWRAP" != 1 ]; then remove_apparmor_bwrap; userns_restriction_note; return; fi
  command -v apparmor_parser >/dev/null && [ -d /etc/apparmor.d ] ||
    { echo "APPARMOR_BWRAP=1 needs AppArmor (apparmor_parser, /etc/apparmor.d)" >&2; exit 1; }
  getent group "$BWRAP_GROUP" >/dev/null || groupadd --system "$BWRAP_GROUP"
  for user in "${AGENTS[@]}"; do usermod -a -G "$BWRAP_GROUP" "$user"; done
  check_bwrap_group
  refuse_symlink "$BWRAP_COPY"
  install -o root -g "$BWRAP_GROUP" -m 0750 /usr/bin/bwrap "$BWRAP_COPY"
  refuse_symlink "$BWRAP_PROFILE"
  bwrap_profile > "$BWRAP_PROFILE"
  chmod 0644 "$BWRAP_PROFILE"
  apparmor_parser -r "$BWRAP_PROFILE"
}

# Succeeds when gid $1 is some account's primary group. awk reads all of
# getent's output, so pipefail never sees a SIGPIPE from an early grep -q exit.
primary_group() {
  getent passwd | awk -F: -v gid="$1" '$4 == gid { found = 1 } END { exit !found }'
}

# Refuses a $BWRAP_GROUP with members other than the role accounts, or one
# that is any account's primary group: whoever is in it may run the copy.
check_bwrap_group() {
  local gid members
  gid=$(getent group "$BWRAP_GROUP" | cut -d: -f3)
  members=$(getent group "$BWRAP_GROUP" | cut -d: -f4 | tr ',' '\n' | sort | paste -sd,)
  if [ "$members" != "$(printf '%s\n' "${AGENTS[@]}" | sort | paste -sd,)" ] ||
     primary_group "$gid"; then
    echo "refusing: group $BWRAP_GROUP has members '$members' or is a primary group; owner repair required" >&2; exit 1
  fi
}

# Warns when Ubuntu's AppArmor userns restriction is on without the opt-in.
userns_restriction_note() {
  [ "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null)" = 1 ] || return 0
  echo "note: AppArmor restricts unprivileged user namespaces here, so reviewer preflight" >&2
  echo "      will refuse launches; re-run with APPARMOR_BWRAP=1 to allow agentc's sandbox" >&2
}

# The agentc Bubblewrap profile. Unlike Ubuntu's bwrap-userns-restrict, which
# strips capabilities from everything bwrap starts, children inherit this
# profile, so the reviewer's nested (userns-disabled) candidate sandbox can
# mount. Only $BWRAP_GROUP members can execute the copy it attaches to.
bwrap_profile() {
  cat <<EOF
# Written by deploy/agentc/host-setup.sh (APPARMOR_BWRAP=1); removed by --uninstall.
abi <abi/4.0>,
include <tunables/global>

profile agentc-bwrap $BWRAP_COPY flags=(attach_disconnected,mediate_deleted) {
  allow capability,
  allow file rwlkm /{**,},
  allow ix /**,
  allow network,
  allow unix,
  allow ptrace,
  allow signal,
  allow mqueue,
  allow io_uring,
  allow userns,
  allow mount,
  allow umount,
  allow pivot_root,
  allow dbus,
}
EOF
}

# The Bubblewrap the supervisor runs: the agentc copy when opted in.
bubblewrap_setting() {
  if [ "$APPARMOR_BWRAP" = 1 ]; then echo "bubblewrap = \"$BWRAP_COPY\""
  else echo '# bubblewrap = "/usr/bin/bwrap"'; fi
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
$(bubblewrap_setting)
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

# Live mode (plan P3b): \`agentc-supervisor run\` (unit agentc-run) polls
# \`next\` with $STATE/impl/coordinator/credentials.toml, claims, launches and
# cleans up; clones come from $STATE/mirror.git and the heartbeat is
# $STATE/heartbeat.json.
# [run]
# poll_seconds = 60
# harness = "claude"             # or "codex"
# model = "default"
# effort = "high"
# min_free_mib = 20480           # refuse to claim below this much free disk
# branch = "main"                # mirror branch each clone starts from
# allow_insecure_loopback = false
# reviewer = false               # reviewer launches are not implemented yet

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

# Installs the live supervisor loop (agentc-supervisor run, plan P3b) as a
# systemd unit but never enables or starts it: the owner opts in with
# systemctl enable --now agentc-run. Reboot-safe: it starts only after the
# firewall and egress proxy (and stops with the firewall), restarts after a
# crash, and a started launch that never finished is kept for recovery
# rather than reused. sysvinit hosts get no unit.
install_run_unit() {
  has_systemd || { echo "no systemd: agentc-run unit not installed"; return 0; }
  cat > /etc/systemd/system/agentc-run.service <<UNIT
[Unit]
Description=agentc-run (agentc live supervisor loop)
Wants=network-online.target
After=network-online.target agentc-firewall.service agentc-egress.service
Requires=agentc-firewall.service agentc-egress.service
[Service]
ExecStart=$PREFIX/bin/agentc-supervisor run
Restart=on-failure
RestartSec=30
TimeoutStopSec=60
[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload
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
# kept, handed back to root (0400). Each role's Claude token is zeroed and
# deleted. Each shared parent goes only once empty.
# Agent accounts are retired first, so nothing they run outlives the firewall;
# the egress account only once its unit (Restart=always) is stopped.
uninstall() {
  for user in "${AGENTS[@]}" "$PUSH_USER"; do retire_account "$user"; done
  remove_service agentc-run
  remove_service agentc-egress
  retire_account agentc-egress
  remove_apparmor_bwrap
  remove_service agentc-firewall
  nft delete table inet agentc 2>/dev/null || true
  git config --system --unset-all safe.directory "^$STATE/mirror.git\$" 2>/dev/null || true
  keep_push_key
  remove_claude_tokens
  remove_own_paths
  rmdir "$PREFIX/bin" "$PREFIX" "$STATE" "$ETC" 2>/dev/null || true
  [ -z "$KEY_NOTE" ] || echo "$KEY_NOTE"
  echo "agentc host setup removed"
}

# Unloads and deletes the agentc Bubblewrap profile, copy and group (whose
# deletion drops the role accounts' membership). Used by --uninstall after
# the role accounts are retired, and by setup runs without APPARMOR_BWRAP=1,
# so an opted-out host never keeps a stale, permissive copy.
remove_apparmor_bwrap() {
  if [ -f "$BWRAP_PROFILE" ] && [ ! -L "$BWRAP_PROFILE" ]; then
    apparmor_parser -R "$BWRAP_PROFILE" 2>/dev/null || true
    rm -f -- "$BWRAP_PROFILE"
  fi
  rm -f -- "$BWRAP_COPY"
  groupdel "$BWRAP_GROUP" 2>/dev/null || true
}

# Ends an account's processes, removes its crontab, at jobs, lingering user
# manager and its files in the shared temp directories, then deletes it, so
# nothing it started or scheduled outlives the account. Absent accounts are
# skipped; failures are reported.
retire_account() {
  local user=$1
  id -u "$user" >/dev/null 2>&1 || return 0
  command -v loginctl >/dev/null && loginctl disable-linger "$user" 2>/dev/null || true
  stop_processes "$user"
  crontab -r -u "$user" 2>/dev/null || true
  rm -f -- "/var/spool/cron/crontabs/$user"
  remove_at_jobs "$user"
  remove_temp_files "$user"
  userdel "$user" 2>/dev/null || echo "warning: could not delete account $user" >&2
}

# SIGKILLs every process whose real or effective uid is $1 until none is
# left (a dying parent can still fork), giving up with a warning after five
# kill rounds or if pgrep cannot tell.
stop_processes() {
  local round
  for round in 1 2 3 4 5 6; do
    case $(processes_left "$1") in
      none) return 0 ;;
      unknown) return 0 ;;
    esac
    [ "$round" -lt 6 ] || break
    pkill -KILL -u "$1" || true
    pkill -KILL -U "$1" || true
    sleep 1
  done
  echo "warning: $1 still has processes after 5 kill rounds" >&2
}

# Prints "some", "none" or (with a warning) "unknown": whether any process
# has $1 as its real or effective uid.
processes_left() {
  local flag status
  for flag in -u -U; do
    status=0; pgrep "$flag" "$1" >/dev/null || status=$?
    case $status in
      0) echo some; return 0 ;;
      1) ;;
      *) echo "warning: cannot list $1's processes (pgrep exit $status)" >&2; echo unknown; return 0 ;;
    esac
  done
  echo none
}

# Removes $1's queued at jobs when at is installed (root's atq lists every
# user's jobs, owner last).
remove_at_jobs() {
  command -v atq >/dev/null || return 0
  local job
  for job in $(atq 2>/dev/null | awk -v u="$1" '$NF == u {print $1}'); do
    atrm "$job" 2>/dev/null || true
  done
}

# Deletes the files, symlinks (never their targets) and emptied directories
# $1 owns in the shared temp directories; reports what other owners' files
# kept in place.
remove_temp_files() {
  local dir
  for dir in "${TEMP_DIRS[@]}"; do
    [ -d "$dir" ] || continue
    find "$dir" -xdev -mindepth 1 -user "$1" -delete 2>/dev/null || true
    if [ -n "$(find "$dir" -xdev -mindepth 1 -user "$1" -print -quit 2>/dev/null)" ]; then
      echo "warning: files owned by $1 remain under $dir" >&2
    fi
  done
}

# Zeroes and deletes each role's single-link regular claude-token before its
# state directory goes; any other entry at that path is left to
# remove_own_paths, whose rm never follows a symlink.
remove_claude_tokens() {
  local user path
  for user in "${AGENTS[@]}"; do
    path=$STATE/${user#agentc-}/claude-token
    if [ -f "$path" ] && [ ! -L "$path" ] && [ "$(stat -c %h -- "$path")" = 1 ]; then
      zero_and_remove "$path"
    fi
  done
}

# Deletes this script's own files and directories, and the containment
# suite's leftover bin directories, without following symlinks.
remove_own_paths() {
  local name
  for name in agentc-supervisor agent-coordinator agentc-push claude codex node bwrap; do
    rm -f -- "$PREFIX/bin/$name"
  done
  rm -rf -- "$PREFIX/rustup" "$PREFIX/cargo" "$PREFIX/rustup-init.sh" "$PREFIX"/suite-bin.*
  for name in impl rev push mirror.git shadow heartbeat.json heartbeat.tmp; do rm -rf -- "${STATE:?}/$name"; done
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
  Claude: as the owner, run $PREFIX/bin/claude setup-token, so each role has
  its own revocable token, then install it (paste the token, then press
  Ctrl-D twice without Enter, so no newline is stored):
  sudo install -o root -g agentc-impl -m 0440 /dev/stdin $STATE/impl/claude-token
  (repeat for agentc-rev with rev/ paths). The token is long-lived and
  inference-only: renew it before it expires and revoke it at claude.ai if it
  is ever exposed. Agent accounts never use claude auth login: its refresh
  needs a writable claude-config, which stays root-owned.
  Codex:
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
That suite uses a mock shell; authenticated Claude/browser compatibility
still requires separate owner verification.
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
Unattended claiming is installed but not enabled; opt in with
  sudo systemctl enable --now agentc-run
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
  install_apparmor_bwrap
  install_seeds
  install_toolchain
  refresh_mirror
  write_config
  write_push_config
  secure_push_key
  install_firewall
  install_egress_service
  install_run_unit
  next_steps
}

main "$@"
