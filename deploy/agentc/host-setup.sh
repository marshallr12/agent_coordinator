#!/usr/bin/env bash
# Prepares this Linux host for supervised agent launches (autonomy plan P2).
#
#   sudo SUPERVISOR=<path> CLI=<path> [PUSH=<path>] [APPARMOR_BWRAP=1] [HEADLESS_SHELL=0] \
#     deploy/agentc/host-setup.sh
#   sudo deploy/agentc/host-setup.sh --uninstall
#
# Creates the agentc-impl / agentc-rev / agentc-egress / agentc-push accounts,
# root-owned pinned binaries, Rust toolchain and reviewer headless browser
# under /opt/agentc (apt adds the browser's missing libraries), private
# per-role state under /var/lib/agentc, a read-only Git mirror, the egress
# proxy service and an nftables table that filters ONLY the two agent uids.
# For the candidate-push helper it also writes /etc/agentc/push.toml when
# absent and hands an existing push App key to agentc-push; it never creates,
# prints or copies that key. Nothing else on the host changes. Idempotent:
# re-running re-pins binaries and reloads the rules. Claude tokens, Codex
# logins, coordinator credentials and the push App key stay manual (printed at
# the end); an installed Claude token is held at root:<role> 0440. It also
# installs attention.py with the agentc-canary (every 10 minutes) and
# agentc-digest (daily) systemd timers, configured by /etc/agentc/attention.env,
# and e2e-canary.py with a daily agentc-e2e-canary@<harness> timer per
# configured harness, configured by /etc/agentc/e2e-canary.env.
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
# Playwright's Chromium headless shell for verifying reviewers (plan M2),
# pinned to Playwright v1.63.0's chromium-headless-shell (browser revision
# 1243) and checked against these SHA-256 sums; HEADLESS_SHELL=0 removes it.
HEADLESS_SHELL=${HEADLESS_SHELL:-1}
HEADLESS_SHELL_REVISION=1243
HEADLESS_SHELL_VERSION=153.0.8010.12
HEADLESS_SHELL_CDN=https://cdn.playwright.dev/builds/cft/$HEADLESS_SHELL_VERSION
HEADLESS_SHELL_SHA256_X64=a9da028861a0cf789ff25c2fed45f5f1aaf969ed9247835b6a7821a4f7af9d1d
HEADLESS_SHELL_SHA256_ARM64=d433c45172c7836e38124fe545f767b02210bfb43a6262f08a297473a8e91c99
BROWSERS=$PREFIX/browsers
BROWSER_PROFILE=/etc/apparmor.d/agentc-browser
# Shared temp directories --uninstall clears of agent-owned files.
TEMP_DIRS=(/tmp /var/tmp /dev/shm)
# The placeholder token containment-suite.sh installs for a run (its
# DUMMY_TOKEN); one left behind is warned about, never adopted silently.
SUITE_DUMMY_TOKEN=agentc-suite-dummy-token
# The attention canary and digest (attention.py): the script, an environment
# file written once (the owner's values live there), the owner-installed
# coordinator token and the timers' schedules (systemd OnUnitActiveSec and
# OnCalendar values; re-run with new ones to change them).
ATTENTION_SCRIPT=$PREFIX/bin/attention.py
ATTENTION_ENV=$ETC/attention.env
ATTENTION_TOKEN=${ATTENTION_TOKEN:-$ETC/attention-token}
ATTENTION_TOKEN_GROUP=agentc-impl
UNIT_DIR=/etc/systemd/system
CANARY_INTERVAL=${CANARY_INTERVAL:-10min}
DIGEST_CALENDAR=${DIGEST_CALENDAR:-daily}
# The daily end-to-end canary (e2e-canary.py): one timer instance per harness
# in the environment file's E2E_HARNESSES (default claude), all serialized by
# one lock so the harnesses run in turn. Its token is root-only: the unit
# runs as root.
E2E_SCRIPT=$PREFIX/bin/e2e-canary.py
E2E_ENV=$ETC/e2e-canary.env
E2E_TOKEN=${E2E_TOKEN:-$ETC/e2e-canary-token}
E2E_CALENDAR=${E2E_CALENDAR:-daily}
E2E_KNOWN_HARNESSES=(claude codex)
E2E_DEFAULT_HARNESSES=claude
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
  # The reviewer principal's write credential and sessions (run loop reviews):
  # root only, never readable by a launch.
  for path in "$STATE/verdict" "$STATE/verdict/home"; do
    refuse_symlink "$path"
    install -d -o root -g root -m 0700 "$path"
  done
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

# The headless browser offered to verifying reviewers (plan M2): the pinned
# headless shell when installed, else a non-snap system Chromium or Chrome,
# else the in-code default. Never a snap wrapper: a snap cannot run as a role
# account outside a login session.
detect_browser() {
  local candidate pinned
  pinned=$(headless_shell_path)
  if [ -n "$pinned" ] && [ -x "$pinned" ]; then echo "$pinned"; return; fi
  for candidate in /usr/bin/chromium /usr/bin/chromium-browser /usr/bin/google-chrome; do
    if [ -x "$candidate" ] && ! is_snap_wrapper "$candidate"; then readlink -f "$candidate"; return; fi
  done
  echo /usr/bin/chromium
}

# Succeeds when $1 resolves into /snap or is a script that hands off to a snap
# (Ubuntu's /usr/bin/chromium-browser).
is_snap_wrapper() {
  local resolved
  resolved=$(readlink -f -- "$1")
  case $resolved in /snap/*) return 0 ;; esac
  [ "$(head -c 2 -- "$resolved")" = '#!' ] && grep -q '/snap/' -- "$resolved"
}

# This machine's Playwright platform name for the headless shell, empty on
# architectures without a pin.
headless_shell_platform() {
  case "$(uname -m)" in
    x86_64) echo linux64 ;;
    aarch64 | arm64) echo linux-arm64 ;;
  esac
}

# The pinned SHA-256 of platform $1's headless shell zip.
headless_shell_sha256() {
  case $1 in
    linux64) echo "$HEADLESS_SHELL_SHA256_X64" ;;
    linux-arm64) echo "$HEADLESS_SHELL_SHA256_ARM64" ;;
  esac
}

# Where the pinned headless shell's executable lives on this machine; empty
# when the architecture has no pin.
headless_shell_path() {
  local platform
  platform=$(headless_shell_platform)
  if [ -n "$platform" ]; then
    echo "$BROWSERS/$HEADLESS_SHELL_REVISION/chrome-headless-shell-$platform/chrome-headless-shell"
  fi
}

# Installs the pinned headless shell root-owned under $BROWSERS/<revision>,
# with its runtime libraries and (APPARMOR_BWRAP=1) its AppArmor profile.
# A re-run keeps an install whose files still match their manifest and
# reinstalls any other; other revisions are deleted. HEADLESS_SHELL=0, or an
# architecture without a pin, removes the profile and every pinned shell.
install_headless_shell() {
  local platform target
  platform=$(headless_shell_platform)
  if [ "$HEADLESS_SHELL" != 1 ] || [ -z "$platform" ]; then
    echo "note: no pinned headless shell installed; reviewers get a system browser if any" >&2
    remove_browser_profile; refuse_symlink "$BROWSERS"; rm -rf -- "$BROWSERS"; return
  fi
  refuse_symlink "$BROWSERS"
  install -d -o root -g root -m 0755 "$BROWSERS"
  target=$BROWSERS/$HEADLESS_SHELL_REVISION
  headless_shell_intact "$platform" "$target" || fetch_headless_shell "$platform" "$target"
  remove_other_revisions
  install_browser_libraries
  install_browser_access
}

# Succeeds when $2 holds platform $1's pinned zip (its .sha256 marker) and
# every file still matches the .manifest written at unpack time, with no file
# added. Re-hashing the unpacked tree takes about a second.
headless_shell_intact() {
  local target=$2 listed present
  [ -d "$target" ] && [ ! -L "$target" ] || return 1
  [ "$(cat -- "$target/.sha256" 2>/dev/null)" = "$(headless_shell_sha256 "$1")" ] || return 1
  listed=$(wc -l < "$target/.manifest") || return 1
  present=$(cd "$target" && find . -type f ! -name .manifest ! -name .sha256 | wc -l)
  [ "$listed" = "$present" ] && (cd "$target" && sha256sum -c --quiet --status --strict .manifest)
}

# Downloads platform $1's zip, refuses it unless its SHA-256 matches the pin,
# and unpacks it root-owned and not group/world-writable as $2, with a
# manifest of its files' hashes. The executable stays 0700 until
# install_browser_access opens it to the right accounts.
fetch_headless_shell() {
  local platform=$1 target=$2 sum work
  sum=$(headless_shell_sha256 "$platform")
  command -v python3 >/dev/null || { echo "installing the headless shell needs python3 (unzip)" >&2; exit 1; }
  work=$(mktemp -d "$BROWSERS/.fetch.XXXXXX")
  curl --proto '=https' --proto-redir '=https' --tlsv1.2 -sSfL -o "$work/shell.zip" \
    "$HEADLESS_SHELL_CDN/$platform/chrome-headless-shell-$platform.zip"
  echo "$sum  $work/shell.zip" | sha256sum -c --quiet - ||
    { rm -rf -- "$work"; echo "refusing: headless shell zip fails its pinned SHA-256" >&2; exit 1; }
  python3 -I -m zipfile -e "$work/shell.zip" "$work/unpacked"
  chown -R root:root "$work/unpacked"
  chmod -R u=rwX,go=rX "$work/unpacked"
  chmod 0700 "$work/unpacked/chrome-headless-shell-$platform/chrome-headless-shell"
  (cd "$work/unpacked" && find . -type f -print0 | sort -z | xargs -0 sha256sum) > "$work/manifest"
  mv -- "$work/manifest" "$work/unpacked/.manifest"
  echo "$sum" > "$work/unpacked/.sha256"
  rm -rf -- "$target"
  mv -T -- "$work/unpacked" "$target"
  rm -rf -- "$work"
}

# Deletes earlier pinned revisions and interrupted downloads under $BROWSERS.
remove_other_revisions() {
  local entry
  for entry in "$BROWSERS"/* "$BROWSERS"/.fetch.*; do
    [ -e "$entry" ] || [ -L "$entry" ] || continue
    [ "$entry" = "$BROWSERS/$HEADLESS_SHELL_REVISION" ] || rm -rf -- "$entry"
  done
}

# Installs the headless shell's runtime libraries (Playwright's Chromium list
# for Debian and Ubuntu) with apt-get, only when the loader cannot resolve
# one; without apt-get it names the missing libraries instead.
install_browser_libraries() {
  local missing
  missing=$(ldd "$(headless_shell_path)" 2>/dev/null | awk '/not found/ { print $1 }' | paste -sd' ' || true)
  [ -n "$missing" ] || return 0
  if ! command -v apt-get >/dev/null; then
    echo "note: install the headless shell's missing libraries: $missing" >&2; return
  fi
  apt-get update -qq
  # shellcheck disable=SC2046 # one package name per word
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends $(browser_packages)
}

# Playwright's Chromium runtime packages, with the t64 names on releases that
# renamed them (Ubuntu 24.04 and newer, Debian 13 and newer).
browser_packages() {
  local t64="" name
  apt-cache show libglib2.0-0t64 >/dev/null 2>&1 && t64=t64
  for name in libasound2 libatk-bridge2.0-0 libatk1.0-0 libatspi2.0-0 libcups2 libglib2.0-0; do
    echo "$name$t64"
  done
  echo libcairo2 libdbus-1-3 libdrm2 libgbm1 libnspr4 libnss3 libpango-1.0-0 libx11-6 libxcb1 \
    libxcomposite1 libxdamage1 libxext6 libxfixes3 libxkbcommon0 libxrandr2 fonts-liberation
}

# Sets who may run the pinned headless shell, ordered so it is never both
# loaded under its profile and runnable by other accounts. With
# APPARMOR_BWRAP=1 the executable becomes root:$BWRAP_GROUP 0750 (only the
# role accounts, like the agentc bwrap copy) before the profile loads: the
# profile grants user namespaces to whoever runs the file. Otherwise the
# profile goes first and the executable becomes root:root 0755.
install_browser_access() {
  local shell
  shell=$(headless_shell_path)
  if [ "$APPARMOR_BWRAP" != 1 ]; then
    remove_browser_profile
    chown root:root "$shell"; chmod 0755 "$shell"; return
  fi
  chown "root:$BWRAP_GROUP" "$shell"; chmod 0750 "$shell"
  refuse_symlink "$BROWSER_PROFILE"
  browser_profile > "$BROWSER_PROFILE"
  chmod 0644 "$BROWSER_PROFILE"
  apparmor_parser -r "$BROWSER_PROFILE"
}

# The headless shell's AppArmor profile: unconfined apart from allowing user
# namespaces, attached to the pinned executable only.
browser_profile() {
  cat <<EOF
# Written by deploy/agentc/host-setup.sh (APPARMOR_BWRAP=1); removed by --uninstall.
abi <abi/4.0>,
include <tunables/global>

profile agentc-browser $(headless_shell_path) flags=(unconfined) {
  userns,
}
EOF
}

# Unloads and deletes the headless shell's AppArmor profile, if any.
remove_browser_profile() {
  [ -f "$BROWSER_PROFILE" ] && [ ! -L "$BROWSER_PROFILE" ] || return 0
  apparmor_parser -R "$BROWSER_PROFILE" 2>/dev/null || true
  rm -f -- "$BROWSER_PROFILE"
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
# allow_insecure_loopback = false  # plain http only to a loopback [run.binding] origin
# reviewer = false               # also review, posting with $STATE/verdict/home/credentials.toml
# review_attempts = 3            # stop claiming a submission after this many failed verdicts
# budget_minutes = 240           # stop renewing a launch's attempt after this
# drain_seconds = 30             # on stop: SIGTERM, then SIGKILL after this
# git_name = "agentc implementer"           # commit identity set in each clone
# git_email = "agentc-impl@agentc.invalid"
# Default: the mirror branch's .agent-coordinator.toml. Set this table to
# work on another coordinator, e.g. the one `staging.py project` prints.
# [run.binding]
# service_url = "http://127.0.0.1:18080"
# project_id = "<staging project id>"
# project_name = "<credential directory>"  # optional CLI credential selector

# Admission before each claim (plan P3b health and cost). While the kill
# switch file exists the loop claims nothing. A 429 marks the vendor
# exhausted ($STATE/vendors.json) and routes to the fallback; every launch's
# tokens and dollars go to $STATE/costs.jsonl and its handoff summary.
# [health]
# kill_switch = "$STATE/kill-switch"
# exhausted_minutes = 60         # when a 429 names no reset time
# token_lifetime_days = 365      # claude setup-token lifetime
# expiry_warn_days = 14          # daily warning this far ahead
# implementer_daily_usd = 150.0  # last-24h spend cap; 0 disables
# reviewer_daily_usd = 50.0
# [health.fallback]              # default: no fallback vendor
# harness = "codex"
# model = "<codex model id>"
# effort = "high"

# Host-approved project setup, run in the launch's sandbox before the
# harness (Claude launches only); output in \$RUN/setup.log.
# [setup.<project-id>]
# command = []                   # e.g. ["cargo", "fetch", "--locked"]
# cache_paths = []               # existing agentc-impl-owned directories
# timeout_seconds = 900

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

# Prints the attention units installed in $UNIT_DIR, canary first.
attention_units() {
  echo agentc-canary.service
  echo agentc-canary.timer
  echo agentc-digest.service
  echo agentc-digest.timer
}

# Prints, one per line, every path --uninstall removes for the attention
# canary and digest: the units, the environment file, the script and the
# canary's paged-set file (next to the heartbeat). The owner's token file is
# not listed: it is handed back to root instead (see remove_attention).
attention_paths() {
  local unit
  for unit in $(attention_units); do echo "$UNIT_DIR/$unit"; done
  echo "$ATTENTION_ENV"
  echo "$ATTENTION_SCRIPT"
  echo "$STATE/canary-state.json"
  echo "$STATE/canary-state.json.tmp"
}

# A oneshot service for attention.py subcommand $2, described as $1. It runs
# as root, which the agent firewall does not filter, so it can reach ntfy and
# the coordinator; it may write only under $STATE (the canary's state file).
attention_service() {
  cat <<EOF
[Unit]
Description=$1
Wants=network-online.target
After=network-online.target
[Service]
Type=oneshot
EnvironmentFile=$ATTENTION_ENV
ExecStart=/usr/bin/python3 -I $ATTENTION_SCRIPT $2
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=$STATE
EOF
}

attention_canary_service() { attention_service "agentc canary (pages through ntfy when an SLO fails)" canary; }
attention_digest_service() { attention_service "agentc attention digest (mails the daily summary)" digest; }

attention_canary_timer() {
  cat <<EOF
[Unit]
Description=Run the agentc canary every $CANARY_INTERVAL
[Timer]
OnBootSec=2min
OnUnitActiveSec=$CANARY_INTERVAL
[Install]
WantedBy=timers.target
EOF
}

attention_digest_timer() {
  cat <<EOF
[Unit]
Description=Run the agentc attention digest ($DIGEST_CALENDAR)
[Timer]
OnCalendar=$DIGEST_CALENDAR
Persistent=true
[Install]
WantedBy=timers.target
EOF
}

# The environment file written when absent. Entries with a code default in
# attention.py are shown commented; the ntfy topic and SMTP settings are the
# owner's to fill in. The coordinator token is a separate file (see
# attention_token_note).
attention_env_file() {
  cat <<EOF
# agentc attention canary and digest (attention.py). Written by
# deploy/agentc/host-setup.sh when absent; never overwritten, removed by
# --uninstall. Commented lines show the in-code defaults.

# Owner-supplied: where the canary pages (required for the canary timer).
ATTENTION_NTFY_TOPIC=
# NTFY_TOKEN=                  # only for a protected ntfy topic

# Owner-supplied: mail the daily digest (leave empty to print it to the journal).
ATTENTION_SMTP_HOST=
ATTENTION_MAIL_TO=
# ATTENTION_SMTP_PORT=25
# ATTENTION_MAIL_FROM=agentc@localhost

# ATTENTION_URL=https://agents.sithbit.com
# ATTENTION_PROJECT=fe95a6c5-2aad-463f-8446-4366d9a281c7
# ATTENTION_TOKEN_FILE=$ATTENTION_TOKEN
# ATTENTION_HEARTBEAT=$STATE/heartbeat.json
# ATTENTION_HEARTBEAT_MAX_AGE=300   # seconds
# ATTENTION_NTFY_URL=https://ntfy.sh
# ATTENTION_STATE=$STATE/canary-state.json
# ATTENTION_MAX_HRI=                # page when more human-required items are open
# ATTENTION_HOURS=24                # the digest's window
# ATTENTION_NEGLECT_DAYS=3          # page when the digest goes unread this many days (0: off)
EOF
}

# Installs attention.py, the environment file (once), and the canary and
# digest units; enables a timer only when what it needs exists, so an
# unconfigured host does not fail every ten minutes. Without systemd it
# installs the script and file only.
install_attention() {
  local unit
  refuse_symlink "$ATTENTION_SCRIPT"; refuse_symlink "$ATTENTION_ENV"
  install -o root -g root -m 0755 "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/attention.py" "$ATTENTION_SCRIPT"
  if [ ! -e "$ATTENTION_ENV" ]; then
    attention_env_file | install -o root -g root -m 0644 /dev/stdin "$ATTENTION_ENV"
  fi
  secure_attention_token
  has_systemd || { echo "no systemd: attention timers not installed"; return 0; }
  write_attention_units "$UNIT_DIR"
  systemctl daemon-reload
  for unit in agentc-canary agentc-digest; do
    if attention_ready "$unit"; then systemctl enable --now --quiet "$unit.timer"
    else systemctl disable --now --quiet "$unit.timer" 2>/dev/null || true; fi
  done
}

# Writes the four unit files into directory $1.
write_attention_units() {
  local dir=$1
  attention_canary_service > "$dir/agentc-canary.service"
  attention_canary_timer > "$dir/agentc-canary.timer"
  attention_digest_service > "$dir/agentc-digest.service"
  attention_digest_timer > "$dir/agentc-digest.timer"
}

# Succeeds when unit $1 has what it needs to run: the token file, and for the
# canary a non-empty ntfy topic in the environment file.
attention_ready() {
  [ -f "$ATTENTION_TOKEN" ] || return 1
  [ "$1" != agentc-canary ] || grep -Eq '^ATTENTION_NTFY_TOPIC=.' "$ATTENTION_ENV"
}

# Holds the owner-installed coordinator token at root:$ATTENTION_TOKEN_GROUP
# 0440 (the role can read it, never change it). Only a root-owned token is
# adopted; a missing one is left for the owner (see attention_token_note).
secure_attention_token() {
  refuse_symlink "$ATTENTION_TOKEN"
  [ -e "$ATTENTION_TOKEN" ] || return 0
  require_single_file "$ATTENTION_TOKEN"
  owned_by_root "$ATTENTION_TOKEN" ||
    { echo "refusing $ATTENTION_TOKEN: not root-owned (install it with sudo install -o root); owner repair required" >&2; exit 1; }
  chmod 0400 -- "$ATTENTION_TOKEN"
  chown "root:$ATTENTION_TOKEN_GROUP" -- "$ATTENTION_TOKEN"
  chmod 0440 -- "$ATTENTION_TOKEN"
}

# Stops the timers and removes the attention units, environment file, script
# and state. The token file is the owner's: it is kept, handed back to root
# (0400) before the role account that could read it is deleted.
remove_attention() {
  local unit path
  if has_systemd; then
    for unit in agentc-canary agentc-digest; do
      systemctl disable --now "$unit.timer" 2>/dev/null || true
      systemctl stop "$unit.service" 2>/dev/null || true
    done
  fi
  while IFS= read -r path; do rm -f -- "$path"; done < <(attention_paths)
  if has_systemd; then systemctl daemon-reload; fi
  if [ -f "$ATTENTION_TOKEN" ] && [ ! -L "$ATTENTION_TOKEN" ] && [ "$(stat -c %h -- "$ATTENTION_TOKEN")" = 1 ]; then
    chown -h root:root -- "$ATTENTION_TOKEN"; chmod 0400 -- "$ATTENTION_TOKEN"
    echo "kept $ATTENTION_TOKEN (root:root 0400); delete it yourself to retire the token"
  fi
}

# Prints what the owner supplies for the attention timers.
attention_token_note() {
  cat <<EOF
Attention canary and digest: attention.py runs from the agentc-canary
($CANARY_INTERVAL) and agentc-digest ($DIGEST_CALENDAR) timers, configured by
$ATTENTION_ENV. Install the supervisor's coordinator token (the bare token,
nothing else) and set ATTENTION_NTFY_TOPIC (and the SMTP entries to mail the
digest) there, then re-run this script to enable the timers:
  sudo install -o root -g $ATTENTION_TOKEN_GROUP -m 0440 /dev/stdin $ATTENTION_TOKEN
EOF
}

# Prints, one per line, every path --uninstall removes for the end-to-end
# canary. The results file (the canary's evidence) and the owner's token are
# kept; see remove_e2e.
e2e_paths() {
  echo "$UNIT_DIR/agentc-e2e-canary@.service"
  echo "$UNIT_DIR/agentc-e2e-canary@.timer"
  echo "$E2E_ENV"
  echo "$E2E_SCRIPT"
  echo "$STATE/e2e-canary.lock"
}

# The service template for harness %i. It runs as root (the agent firewall
# filters only the agent uids) and holds a lock so two harnesses never run at
# once: the host's supervisor serves one canary task at a time.
e2e_service() {
  cat <<EOF
[Unit]
Description=agentc end-to-end canary (%i)
Wants=network-online.target
After=network-online.target
[Service]
Type=oneshot
EnvironmentFile=$E2E_ENV
ExecStart=/usr/bin/flock $STATE/e2e-canary.lock /usr/bin/python3 -I $E2E_SCRIPT --harness %i
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=$STATE
EOF
}

# The timer template: instance %i starts agentc-e2e-canary@%i.service daily.
e2e_timer() {
  cat <<EOF
[Unit]
Description=Run the agentc end-to-end canary for %i ($E2E_CALENDAR)
[Timer]
OnCalendar=$E2E_CALENDAR
Persistent=true
[Install]
WantedBy=timers.target
EOF
}

# The environment file written when absent. Entries with a code default in
# e2e-canary.py are shown commented; the project, topic and harnesses are the
# owner's (the project comes from canary-setup.py).
e2e_env_file() {
  cat <<EOF
# agentc end-to-end canary (e2e-canary.py). Written by
# deploy/agentc/host-setup.sh when absent; never overwritten, removed by
# --uninstall. Commented lines show the in-code defaults.

# Owner-supplied: the canary project (printed by canary-setup.py), where
# failures page, and the harnesses to canary (space separated; default
# $E2E_DEFAULT_HARNESSES). Re-run host-setup.sh after changing them.
E2E_PROJECT=
E2E_NTFY_TOPIC=
# NTFY_TOKEN=                  # only for a protected ntfy topic
# E2E_HARNESSES=$E2E_DEFAULT_HARNESSES

# E2E_URL=https://agents.sithbit.com
# E2E_TOKEN_FILE=$E2E_TOKEN
# E2E_NTFY_URL=https://ntfy.sh
# E2E_TIMEOUT_MINUTES=90            # page when the task is not done by then
# E2E_POLL_SECONDS=20
# E2E_RESULTS=$STATE/e2e-canary.jsonl
# E2E_LEDGER=$STATE/costs.jsonl     # the supervisor's ledger; names the serving harness
# E2E_HOST=                         # label in results (default: hostname)
EOF
}

# The harnesses named by E2E_HARNESSES in the environment file (commas or
# spaces), else the default, one per line; only known harnesses are listed.
e2e_harnesses() {
  local line="" harness
  [ ! -f "$E2E_ENV" ] || line=$(sed -n 's/^E2E_HARNESSES=//p' "$E2E_ENV" | tail -n 1)
  line=${line//,/ }
  [ -n "${line// /}" ] || line=$E2E_DEFAULT_HARNESSES
  for harness in $line; do
    case " ${E2E_KNOWN_HARNESSES[*]} " in
      *" $harness "*) echo "$harness" ;;
      *) echo "ignoring unknown E2E_HARNESSES entry '$harness'" >&2 ;;
    esac
  done
}

# Succeeds when the canary has what it needs to run: the token file and a
# non-empty project and ntfy topic in the environment file.
e2e_ready() {
  [ -f "$E2E_TOKEN" ] && [ -f "$E2E_ENV" ] || return 1
  grep -Eq '^E2E_PROJECT=.' "$E2E_ENV" && grep -Eq '^E2E_NTFY_TOPIC=.' "$E2E_ENV"
}

# Writes the two unit templates into directory $1.
write_e2e_units() {
  e2e_service > "$1/agentc-e2e-canary@.service"
  e2e_timer > "$1/agentc-e2e-canary@.timer"
}

# Installs e2e-canary.py (it needs attention.py beside it), the environment
# file (once) and the unit templates; enables the timer of each configured
# harness once e2e_ready, and disables the others.
install_e2e() {
  local harness wanted=()
  refuse_symlink "$E2E_SCRIPT"; refuse_symlink "$E2E_ENV"
  install -o root -g root -m 0755 "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/e2e-canary.py" "$E2E_SCRIPT"
  if [ ! -e "$E2E_ENV" ]; then
    e2e_env_file | install -o root -g root -m 0644 /dev/stdin "$E2E_ENV"
  fi
  secure_e2e_token
  has_systemd || { echo "no systemd: end-to-end canary timers not installed"; return 0; }
  write_e2e_units "$UNIT_DIR"
  systemctl daemon-reload
  while IFS= read -r harness; do wanted+=("$harness"); done < <(e2e_harnesses)
  for harness in "${E2E_KNOWN_HARNESSES[@]}"; do
    if e2e_ready && [[ " ${wanted[*]:-} " == *" $harness "* ]]; then
      systemctl enable --now --quiet "agentc-e2e-canary@$harness.timer"
    else
      systemctl disable --now --quiet "agentc-e2e-canary@$harness.timer" 2>/dev/null || true
    fi
  done
}

# Holds the owner-installed canary token root-only (0400). Only a root-owned
# token is adopted; a missing one is left for the owner.
secure_e2e_token() {
  refuse_symlink "$E2E_TOKEN"
  [ -e "$E2E_TOKEN" ] || return 0
  require_single_file "$E2E_TOKEN"
  owned_by_root "$E2E_TOKEN" ||
    { echo "refusing $E2E_TOKEN: not root-owned (install it with sudo install -o root); owner repair required" >&2; exit 1; }
  chmod 0400 -- "$E2E_TOKEN"
}

# Stops the canary timers and removes the units, environment file, script and
# lock. The token and the results file are the owner's evidence: kept.
remove_e2e() {
  local harness path
  if has_systemd; then
    for harness in "${E2E_KNOWN_HARNESSES[@]}"; do
      systemctl disable --now "agentc-e2e-canary@$harness.timer" 2>/dev/null || true
      systemctl stop "agentc-e2e-canary@$harness.service" 2>/dev/null || true
    done
  fi
  while IFS= read -r path; do rm -f -- "$path"; done < <(e2e_paths)
  if has_systemd; then systemctl daemon-reload; fi
  if [ -f "$E2E_TOKEN" ]; then echo "kept $E2E_TOKEN; delete it yourself to retire the token"; fi
  if [ -f "$STATE/e2e-canary.jsonl" ]; then echo "kept $STATE/e2e-canary.jsonl (the canary's results)"; fi
}

# Prints what the owner supplies for the end-to-end canary.
e2e_token_note() {
  cat <<EOF
End-to-end canary: once per host run deploy/agentc/canary-setup.py to create
the canary project, then install its agent token and set E2E_PROJECT and
E2E_NTFY_TOPIC (E2E_HARNESSES to canary more than $E2E_DEFAULT_HARNESSES) in
$E2E_ENV, and re-run this script to enable the daily agentc-e2e-canary@<harness>
timers:
  sudo install -o root -g root -m 0400 /dev/stdin $E2E_TOKEN
EOF
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
# The loop drains on SIGTERM (drain_seconds, then a release); systemd kills
# any remaining process only after the loop has exited.
KillMode=mixed
TimeoutStopSec=120
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
# The agentc-run loop stops first, then agent accounts are retired, so nothing
# they run outlives the firewall; the egress account only once its unit
# (Restart=always) is stopped.
uninstall() {
  # The loop runs launches as the agent accounts; stop it before retiring them.
  remove_service agentc-run
  remove_attention
  remove_e2e
  for user in "${AGENTS[@]}" "$PUSH_USER"; do retire_account "$user"; done
  remove_service agentc-egress
  retire_account agentc-egress
  remove_apparmor_bwrap
  remove_browser_profile
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
  rm -rf -- "$PREFIX/rustup" "$PREFIX/cargo" "$PREFIX/rustup-init.sh" "$PREFIX"/suite-bin.* "$BROWSERS"
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
For run-loop reviews, issue a separate write principal (it never implements) and
save its credentials.toml as $STATE/verdict/home/credentials.toml (root, 0600).
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
  attention_token_note
  e2e_token_note
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
  install_headless_shell
  install_seeds
  install_toolchain
  refresh_mirror
  write_config
  write_push_config
  secure_push_key
  install_firewall
  install_egress_service
  install_run_unit
  install_attention
  install_e2e
  next_steps
}

# Sourcing the script (the attention test does) defines the functions only.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then main "$@"; fi
