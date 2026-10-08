#!/usr/bin/env bash
# Verifies supervised-launch containment on this host (autonomy plan P2 exit
# criterion). No LLM is launched. Run as root after host-setup.sh:
#
#   sudo deploy/agentc/containment-suite.sh [--cargo-test]
#
# Prints one PASS/FAIL line per check and exits non-zero if any check fails.
# A role with no claude-token gets a dummy one for the run (removed on exit),
# so the mock-harness Claude legs pass preflight before the owner installs
# real tokens. A dummy left by a run whose EXIT trap never ran is a FAIL. The
# suite never prints a token, and compares an existing one with the dummy only
# when its size matches the dummy's.
set -uo pipefail

PREFIX=/opt/agentc
STATE=/var/lib/agentc
SUP=$PREFIX/bin/agentc-supervisor
PROXY=http://127.0.0.1:${PROXY_PORT:-3128}
STAGING=http://127.0.0.1:${STAGING_PORT:-18080}
DISABLED_PUSH=disabled://push-only-via-agent-coordinator
PUSH_USER=agentc-push
PUSH_KEY=/etc/agentc/push-app.pem
ATTENTION_TOKEN=/etc/agentc/attention-token
FAILED=0
SUITE_BINS=()
PUSH_PID=
PUSH_RUN=
DUMMY_TOKEN=agentc-suite-dummy-token
DUMMY_TOKENS=()
STALE_TOKENS=()

pass() { echo "PASS $*"; }
fail() { echo "FAIL $*"; FAILED=1; }

# expect_ok / expect_fail DESCRIPTION COMMAND...: judge a command's exit status.
expect_ok() { local d=$1; shift; if "$@" >/dev/null 2>&1; then pass "$d"; else fail "$d"; fi; }
# expect_ok_logged DESCRIPTION LOG COMMAND...: like expect_ok, keeping the
# output in LOG and printing its tail when the command fails.
expect_ok_logged() {
  local d=$1 log=$2; shift 2
  if "$@" >"$log" 2>&1; then pass "$d"; else fail "$d (log: $log)"; tail -n 40 "$log" | sed 's/^/    /'; fi
}
expect_fail() { local d=$1; shift; if "$@" >/dev/null 2>&1; then fail "$d"; else pass "$d"; fi; }

# Runs a command as an agent account with an empty environment.
as() {
  local user=$1; shift
  sudo -u "$user" env -i PATH=/usr/bin:/bin HOME="$STATE/${user#agentc-}/home" "$@"
}

# Maps an account to the supervisor's role name.
role_of() { [ "$1" = agentc-impl ] && echo implementer || echo reviewer; }

# Creates a fresh hardened clone and run directory for a role.
prepare_clone() {
  local user=$1 base=$STATE/${1#agentc-}
  as "$user" rm -rf "$base/clones/suite" "$base/runs/suite"
  expect_ok "$user: hardened clone at mirror HEAD" as "$user" "$SUP" clone \
    --url "$STATE/mirror.git" --revision "$(git -C "$STATE/mirror.git" rev-parse HEAD)" \
    --dest "$base/clones/suite" \
    --origin-url "$(git -C "$STATE/mirror.git" config --get remote.origin.url)"
  as "$user" mkdir -p "$base/runs/suite"
  as "$user" sh -c "echo containment-suite > '$base/runs/suite/prompt.md'"
}

# Codex cannot isolate candidate code from reviewer secrets (R-P3b.3), so
# preflight must refuse a Codex reviewer for exactly that reason.
codex_reviewer_refused() {
  local user=$1; shift
  expect_ok "$user/codex: preflight refuses Codex reviewers (R-P3b.3)" sh -c \
    "sudo -u $user $SUP preflight $* --harness codex 2>&1 | grep -q 'Codex reviewer launches cannot isolate'"
}

# Preflight and the candidate-instruction flags, per harness.
check_profiles() {
  local user=$1 base=$STATE/${1#agentc-} role
  role=$(role_of "$user")
  local spec=(--role "$role" --clone "$base/clones/suite" --run "$base/runs/suite")
  for harness in claude codex; do
    as "$user" "$SUP" prepare "${spec[@]}" --harness "$harness" >/dev/null 2>&1
    if [ "$role/$harness" = reviewer/codex ]; then codex_reviewer_refused "$user" "${spec[@]}"; continue; fi
    expect_ok "$user/$harness: preflight clean" as "$user" "$SUP" preflight "${spec[@]}" --harness "$harness"
  done
  expect_ok "$user/claude: --safe-mode (no candidate CLAUDE.md or hooks)" \
    sh -c "sudo -u $user $SUP launch ${spec[*]} --harness claude --dry-run | grep -q -- '\"--safe-mode\"'"
  expect_ok "$user/codex: project_doc_max_bytes=0 (no candidate AGENTS.md)" \
    sh -c "sudo -u $user $SUP launch ${spec[*]} --harness codex --dry-run | grep -q 'project_doc_max_bytes=0'"
}

# A planted hook must not run, and origin must not be pushable.
check_git() {
  local user=$1 base=$STATE/${1#agentc-}
  local clone=$base/clones/suite marker=$base/runs/suite/HOOK_RAN
  as "$user" sh -c "printf '#!/bin/sh\ntouch $marker\n' > $clone/.git/hooks/post-commit && chmod +x $clone/.git/hooks/post-commit"
  as "$user" git -C "$clone" -c user.name=suite -c user.email=suite@invalid commit -q --allow-empty -m hook >/dev/null 2>&1
  expect_fail "$user: planted post-commit hook did not run" test -e "$marker"
  expect_ok "$user: origin push URL is disabled" \
    sh -c "[ \"\$(sudo -u $user git -C $clone remote get-url --push origin)\" = '$DISABLED_PUSH' ]"
  expect_fail "$user: raw git push refused" as "$user" git -C "$clone" push origin HEAD:refs/heads/suite-probe
}

# Writes outside the role's own state must fail.
check_writes() {
  local user=$1 other=$2 owner_home
  owner_home=$(getent passwd "${SUDO_USER:-root}" | cut -d: -f6)
  expect_fail "$user: cannot write the owner's home" as "$user" touch "$owner_home/.agentc-escape"
  expect_fail "$user: cannot write pinned binaries" as "$user" touch "$PREFIX/bin/escape"
  expect_fail "$user: cannot write the mirror" as "$user" touch "$STATE/mirror.git/escape"
  expect_fail "$user: cannot write ${other}'s state" as "$user" touch "$STATE/${other#agentc-}/escape"
  expect_fail "$user: cannot read ${other}'s clones" as "$user" ls "$STATE/${other#agentc-}/clones"
}

# Persistent seed parents must deny replacement as well as content writes.
# Probe create permission with harmless names; never unlink real seeds.
# claude-config holds no login, and the role cannot take Claude's OAuth
# refresh lock there.
check_seeds() {
  local user=$1 base=$STATE/${1#agentc-} path
  for path in "$base" "$base/claude-config" /etc/agentc; do
    expect_fail "$user: cannot create entries in protected $path" as "$user" touch "$path/.suite-seed-write"
  done
  for path in "$base/claude-config/settings.json" "$base/claude-config/CLAUDE.md" /etc/agentc/cargo-config.toml; do
    expect_fail "$user: protected seed is not writable: $path" as "$user" test -w "$path"
  done
  expect_fail "$user: cannot mkdir claude-config/.oauth_refresh.lock" \
    as "$user" mkdir "$base/claude-config/.oauth_refresh.lock"
  expect_fail "$user: no retired claude-config/.credentials.json" \
    test -e "$base/claude-config/.credentials.json" -o -L "$base/claude-config/.credentials.json"
  expect_ok "$user: per-launch Cargo baseline is seeded" cmp /etc/agentc/cargo-config.toml "$base/runs/suite/state/cargo/config.toml"
}

# Direct egress and DNS fail; the proxy allows only allowlisted hosts.
check_egress() {
  local user=$1
  expect_fail "$user: direct HTTPS blocked by firewall" as "$user" curl -sS --max-time 10 --noproxy '*' https://github.com/
  expect_fail "$user: DNS blocked" as "$user" getent ahosts example.com
  expect_fail "$user: proxy refuses non-allowlisted host" as "$user" curl -sSf --max-time 10 -x "$PROXY" https://example.com/
  expect_ok "$user: proxy allows github.com" as "$user" curl -sSf -o /dev/null --max-time 20 -x "$PROXY" https://github.com/
}

# The Codex sandbox confines writes to the workspace (no model call).
check_codex_sandbox() {
  local user=$1 base=$STATE/${1#agentc-}
  local sandbox="cd $base/clones/suite && CODEX_HOME=$base/codex-home $PREFIX/bin/codex sandbox -c sandbox_mode=workspace-write --"
  expect_ok "$user: codex sandbox writes inside the clone" as "$user" sh -c "$sandbox touch sandbox-inside"
  expect_fail "$user: codex sandbox cannot write the role home" as "$user" sh -c "$sandbox touch $base/home/sandbox-escape"
}

# Exercise the exact Claude dry-run wrapper with a mock shell, without a model
# call or reading credentials. Python closes inherited descriptors as the Rust
# launcher does; module tests additionally exercise the native close_range hook.
check_claude_sandbox() {
  local user=$1 base=$STATE/${1#agentc-} role
  role=$(role_of "$user")
  as "$user" mkdir -p "$base/runs/suite-other" "$base/clones/suite-other"
  expect_ok_logged "$user: Claude OS write boundary (mock harness)" "/root/agentc-claude-${1#agentc-}.log" \
    as "$user" /usr/bin/python3 - "$SUP" "$base" "$role" <<'PY'
import json
import subprocess
import sys

supervisor, base, role = sys.argv[1:]
run = base + "/runs/suite"
clone = base + "/clones/suite"
spec = ["--role", role, "--harness", "claude", "--clone", clone, "--run", run]
description = json.loads(subprocess.check_output([supervisor, "launch", *spec, "--dry-run"]))
args = description["args"]
assert "--ro-bind" in args and "--die-with-parent" in args
args = args[:args.index("--")]
script = r'''
set -eu
base=$1
role=$2
run=$base/runs/suite
clone=$base/clones/suite
deny() { if "$@" 2>/dev/null; then echo "unexpected success: $*" >&2; exit 1; fi; }
overwrite() { printf corrupted > "$1"; }
for path in "$base/home/suite-escape" "$base/runs/suite-other/escape" \
 "$base/clones/suite-other/escape" "$run/prompt.md" "$run/role-settings.json" \
 "$CARGO_HOME/config.toml" "$CLAUDE_CONFIG_DIR/settings.json" \
 "$CLAUDE_CONFIG_DIR/CLAUDE.md"; do
 deny overwrite "$path"
done
deny mv "$CARGO_HOME" "$CARGO_HOME-replaced"
deny mkdir "$CLAUDE_CONFIG_DIR/.oauth_refresh.lock"
deny touch "$run/unexpected-metadata"
for path in "$HOME" "$CARGO_HOME/registry" "$AGENT_COORDINATOR_HOME" \
 "$TMPDIR" "$CARGO_TARGET_DIR"; do
 printf allowed > "$path/suite-own-write"
done
if [ "$role" = implementer ]; then
 printf allowed > "$clone/suite-own-write"
else
 deny overwrite "$clone/suite-own-write"
fi
'''
environment = dict(entry.split("=", 1) for entry in description["env"])
subprocess.run([description["program"], *args, "--", "/bin/sh", "-c", script,
                "mock-harness", base, role], env=environment, cwd=clone,
               stdin=subprocess.DEVNULL, close_fds=True, check=True, timeout=30)
PY
}

# Writes the mock Claude harness for real launches to $1: no model call and
# no credential read. `--version` defers to the pinned binary so preflight
# passes; otherwise, inside the launch's network namespace, it must reach the
# proxy (possible only through the supervisor's relay), must not reach the
# network directly, and a reviewer's candidate command must reach the proxy.
# It leaves $TMPDIR/suite-harness-token when CLAUDE_CODE_OAUTH_TOKEN is set,
# and $TMPDIR/suite-token-hidden when a reviewer's candidate command can
# neither read rev/claude-token nor see that variable.
write_mock_claude() {
  cat > "$1" <<EOF
#!/bin/sh
[ "\${1:-}" = --version ] && exec $PREFIX/bin/claude --version
set -eu
curl -sSf -o /dev/null --max-time 20 -x "\$HTTPS_PROXY" https://github.com/ || exit 3
if curl -sS -o /dev/null --max-time 5 --noproxy '*' https://github.com/; then exit 4; fi
[ -z "\${CLAUDE_CODE_OAUTH_TOKEN:-}" ] || : > "\$TMPDIR/suite-harness-token"
[ -n "\${CLAUDE_CODE_SHELL_PREFIX:-}" ] || exit 0
"\$CLAUDE_CODE_SHELL_PREFIX" "curl -sSf -o /dev/null --max-time 20 -x \$HTTPS_PROXY https://github.com/ && pwd -P >| \$TMPDIR/suite-relay-cwd" || exit 5
if "\$CLAUDE_CODE_SHELL_PREFIX" "test ! -r $STATE/rev/claude-token && [ -z \"\\\${CLAUDE_CODE_OAUTH_TOKEN+x}\" ] && pwd -P >| \$TMPDIR/suite-token-cwd"; then
  : > "\$TMPDIR/suite-token-hidden"
fi
EOF
  chmod 0755 "$1"
}

# Creates a root-owned bin dir under $PREFIX, records it for cleanup and
# sets SUITE_BIN to it. Not run in a subshell, so the record survives.
new_suite_bin() {
  SUITE_BIN=$(mktemp -d "$PREFIX/suite-bin.XXXXXX") || return 1
  SUITE_BINS+=("$SUITE_BIN")
  chmod 0755 "$SUITE_BIN"
}

# Fills bin dir $1 with executable $2 as the relay supervisor and the mock
# harness, plus a supervisor.toml that is the installed one with bin_dir
# pointing there.
fill_suite_bin() {
  install -m 0755 "$2" "$1/agentc-supervisor" && write_mock_claude "$1/claude" || return 1
  { printf 'bin_dir = "%s"\n' "$1"
    grep -v '^[[:space:]]*bin_dir[[:space:]]*=' /etc/agentc/supervisor.toml; } > "$1/supervisor.toml" &&
    chmod 0644 "$1/supervisor.toml"
}

# Creates and fills one suite bin dir with relay supervisor $1 (see
# fill_suite_bin); exits the suite if that fails. Sets SUITE_BIN.
make_suite_bin() {
  new_suite_bin && fill_suite_bin "$SUITE_BIN" "$1" && return 0
  echo "cannot create suite bin dir" >&2; exit 1
}

# Removes the bin dirs main recorded in SUITE_BINS.
remove_suite_bins() {
  local dir
  for dir in ${SUITE_BINS[@]+"${SUITE_BINS[@]}"}; do rm -rf -- "$dir"; done
}

# Runs one real Claude launch as $1 with config $2 and run dir $3; on
# failure, prints the launch's own stderr.log after the supervisor's output.
launch_logged() {
  local user=$1 config=$2 run=$3 base=$STATE/${1#agentc-}
  as "$user" "$SUP" --config "$config" launch --role "$(role_of "$user")" \
    --harness claude --clone "$base/clones/suite" --run "$run" --model mock --effort low \
    && return 0
  local status=$?
  [ -f "$run/stderr.log" ] && sed 's/^/stderr.log: /' "$run/stderr.log"
  return "$status"
}

# A real Claude launch (R-P3b.4) as $1 through relay bin dir $2: `launch`
# must start the host relay, or the mock harness cannot reach the proxy.
check_claude_launch() {
  local user=$1 bin=$2 run=$STATE/${1#agentc-}/runs/suite-launch
  as "$user" rm -rf "$run"
  as "$user" mkdir -p "$run"
  as "$user" sh -c "echo containment-suite > '$run/prompt.md'"
  expect_ok_logged "$user: real Claude launch reaches the proxy only via the relay" \
    "/root/agentc-launch-${1#agentc-}.log" launch_logged "$user" "$bin/supervisor.toml" "$run"
}

# True when preflight as $1 with config $2 reports a failed relay probe.
# Preflight exits non-zero then, so its output is captured, not piped
# (the suite runs under pipefail).
relay_probe_refused() {
  local base=$STATE/${1#agentc-} report
  report=$(as "$1" "$SUP" --config "$2" preflight --role "$(role_of "$1")" --harness claude \
    --clone "$base/clones/suite" --run "$base/runs/suite")
  [[ $report == *'namespace relay probe failed'* ]]
}

# Preflight runs the relay once: with a supervisor that cannot relay (stub
# bin dir $2), it must refuse the launch.
check_relay_probe() {
  expect_ok "$1: preflight refuses a supervisor that cannot relay" \
    relay_probe_refused "$1" "$2/supervisor.toml"
}

# Writes the mock Claude harness for launch-root's push leg to $1. Inside the
# sandbox it requires AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET, a socket it can
# connect to, a push root showing only its own launch's `sock/`, and a helper
# that serves it: an empty request must get the helper's `bad_request` reply,
# not the foreign-launch refusal. It then
# writes the socket path to $TMPDIR/push-ready and waits up to 120 s for the
# suite's $TMPDIR/push-release, so the suite can inspect the live helper.
write_push_mock() {
  cat > "$1" <<EOF
#!/bin/sh
[ "\${1:-}" = --version ] && exec $PREFIX/bin/claude --version
set -eu
sock=\${AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET:-}
[ -S "\$sock" ] || exit 3
dir=\${sock%/sock/push.sock}
[ "\$(ls -A $STATE/push)" = "\${dir##*/}" ] && [ "\$(ls -A "\$dir")" = sock ] || exit 4
/usr/bin/python3 -c '
import socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(10)
s.connect(sys.argv[1])
s.shutdown(socket.SHUT_WR)
reply = s.makefile("rb").readline()
sys.exit(0 if b"bad_request" in reply else 1)' "\$sock" || exit 5
printf '%s\n' "\$sock" > "\$TMPDIR/push-ready"
i=0
until [ -e "\$TMPDIR/push-release" ]; do i=\$((i + 1)); [ "\$i" -le 600 ] || exit 6; sleep 0.2; done
EOF
  chmod 0755 "$1"
}

# Prints supervisor config $1 with `[push_helper] config` set to $2, adding
# the table when $1 has none.
with_push_config() {
  awk -v line="config = \"$2\"" '
    /^[[:space:]]*\[/ { table = $0; gsub(/[[:space:]]/, "", table) }
    table == "[push_helper]" && /^[[:space:]]*config[[:space:]]*=/ { next }
    { print }
    /^[[:space:]]*\[push_helper\][[:space:]]*$/ { print line; seen = 1 }
    END { if (!seen) { print "[push_helper]"; print line } }' "$1"
}

# Turns suite bin dir $1 into the push leg's: the push mock harness and a
# helper configuration that cannot mint (no key file, unreachable https API),
# so no candidate ever reaches GitHub.
fill_push_bin() {
  write_push_mock "$1/claude" &&
    printf '%s\n' 'app_id = 1' 'installation_id = 1' \
      'repository = "https://github.com/agentc-suite/unreachable.git"' \
      'api_base = "https://127.0.0.1:9"' \
      'private_key = "/nonexistent/agentc-suite-push-key.pem"' > "$1/push.toml" &&
    chmod 0644 "$1/push.toml" &&
    with_push_config "$1/supervisor.toml" "$1/push.toml" > "$1/supervisor.push" &&
    mv -f "$1/supervisor.push" "$1/supervisor.toml" && chmod 0644 "$1/supervisor.toml"
}

# Recreates implementer run directory $1 with a prompt.
fresh_impl_run() {
  as agentc-impl rm -rf "$1"
  as agentc-impl mkdir -p "$1"
  as agentc-impl sh -c "echo containment-suite > '$1/prompt.md'"
}

# Starts an implementer `launch-root` in the background through push bin $1
# with session $2 and run $3, logging to $4; sets PUSH_PID.
start_push_launch() {
  "$SUP" --config "$1/supervisor.toml" launch-root --role implementer --harness claude \
    --clone "$STATE/impl/clones/suite" --run "$3" --model mock --effort low \
    --task suite-push --session-id "$2" >"$4" 2>&1 &
  PUSH_PID=$! PUSH_RUN=$3
}

# On exit: releases a still-running push launch's mock harness, then stops
# its launch-root with SIGTERM and, after 5 s, SIGKILL.
stop_push_launch() {
  [ -n "${PUSH_PID:-}" ] || return 0
  as agentc-impl touch "$PUSH_RUN/tmp/push-release" 2>/dev/null
  kill -TERM "$PUSH_PID" 2>/dev/null || return 0
  local i
  for i in $(seq 25); do kill -0 "$PUSH_PID" 2>/dev/null || return 0; sleep 0.2; done
  kill -KILL "$PUSH_PID" 2>/dev/null
}

# The EXIT trap: no launch-root left running, no suite bin dir or dummy
# token left behind.
suite_cleanup() {
  stop_push_launch
  remove_suite_bins
  remove_dummy_tokens
}

# Installs a dummy claude-token (root:<role> 0440) for each role that has
# none (see install_dummy) and checks an existing one with flag_stale_dummy.
# Exits when a token path is a symlink or not a regular file.
install_dummy_tokens() {
  local user token
  for user in agentc-impl agentc-rev; do
    token=$STATE/${user#agentc-}/claude-token
    if [ -L "$token" ] || { [ -e "$token" ] && [ ! -f "$token" ]; }; then
      echo "refusing: $token is a symlink or not a regular file; owner repair required" >&2; exit 1
    fi
    if [ -e "$token" ]; then flag_stale_dummy "$token"; else install_dummy "$user" "$token"; fi
  done
}

# Installs the dummy token for role account $1 at absent path $2 and records
# it for remove_dummy_tokens. On failure it removes the single-link regular
# file the install left there, owned by the suite's own uid (root), then
# exits; install_dummy_tokens calls it only for an absent path, so that file
# is never a real token.
install_dummy() {
  if printf '%s' "$DUMMY_TOKEN" | install -o root -g "$1" -m 0440 /dev/stdin "$2"; then
    DUMMY_TOKENS+=("$2"); return 0
  fi
  if [ -f "$2" ] && [ ! -L "$2" ] && [ "$(stat -c '%h %u' -- "$2")" = "1 $(id -u)" ]; then
    rm -f -- "$2"
  fi
  echo "cannot install a dummy $2" >&2; exit 1
}

# FAILs when existing token $1 is a dummy left by an earlier run whose EXIT
# trap never ran (SIGKILL, power loss), and records it so its checks carry
# the dummy NOTE. Only a file of exactly the dummy's size is compared, so a
# real token is read only at that size and never printed.
flag_stale_dummy() {
  [ "$(stat -c %s -- "$1")" = "${#DUMMY_TOKEN}" ] && holds_dummy_token "$1" || return 0
  fail "stale suite dummy token at $1; remove it (sudo rm $1) or install the real setup-token"
  STALE_TOKENS+=("$1")
}

# Removes each recorded dummy token while it is still the suite's own file;
# anything else now at a recorded path is left in place with a warning.
remove_dummy_tokens() {
  local token
  for token in ${DUMMY_TOKENS[@]+"${DUMMY_TOKENS[@]}"}; do
    if holds_dummy_token "$token"; then
      rm -f -- "$token"
    elif [ -e "$token" ] || [ -L "$token" ]; then
      echo "warning: left $token in place: it is no longer the suite's dummy token" >&2
    fi
  done
}

# True when $1 is a single-link regular file, not a symlink, holding exactly
# the dummy token. Called on the suite's recorded dummies and, after a size
# check, by flag_stale_dummy.
holds_dummy_token() {
  [ -f "$1" ] && [ ! -L "$1" ] && [ "$(stat -c %h -- "$1")" = 1 ] &&
    printf '%s' "$DUMMY_TOKEN" | cmp -s - "$1"
}

# True when the suite installed $1 as a dummy token or flagged it as a stale
# one. Decided from those records alone, so no token's bytes are read.
is_dummy_token() {
  local token
  for token in ${DUMMY_TOKENS[@]+"${DUMMY_TOKENS[@]}"} ${STALE_TOKENS[@]+"${STALE_TOKENS[@]}"}; do
    [ "$token" = "$1" ] && return 0
  done
  return 1
}

# Waits up to $2 seconds for file $1 while process $3 lives.
await_file() {
  local deadline=$((SECONDS + $2))
  until [ -e "$1" ]; do
    kill -0 "$3" 2>/dev/null && [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 0.2
  done
}

# True once no process matches full-command-line pattern $1 (up to $2 s).
await_no_process() {
  local deadline=$((SECONDS + $2))
  while pgrep -f -- "$1" >/dev/null; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 0.2
  done
}

# The first process whose command line holds --launch=$1: only the helper
# serving session $1 is started with that argument.
helper_pid() { pgrep -f -- "--launch=$1" | head -n 1; }

# True when process $1 has account $2's user and group ids in every slot and
# exactly that account's groups.
runs_as_account() {
  local status=/proc/$1/status uid gid want have
  uid=$(id -u "$2") gid=$(id -g "$2")
  want=$(id -G "$2" | tr ' ' '\n' | sort -n | tr '\n' ' ')
  have=$(awk '$1 == "Groups:" { for (i = 2; i <= NF; i++) print $i }' "$status" | sort -n | tr '\n' ' ')
  [ "$(awk '$1 == "Uid:" { print $2, $3, $4, $5 }' "$status")" = "$uid $uid $uid $uid" ] &&
    [ "$(awk '$1 == "Gid:" { print $2, $3, $4, $5 }' "$status")" = "$gid $gid $gid $gid" ] &&
    [ -n "$have" ] && [ "$have" = "$want" ]
}

# True when account $1 can connect to Unix socket $2. It closes at once,
# sending nothing, so the helper mints nothing.
connects_as() {
  as "$1" /usr/bin/python3 -c \
    'import socket, sys; socket.socket(socket.AF_UNIX).connect(sys.argv[1])' "$2"
}

# True when a process as account $1 outside every launch's process tree is
# refused by the push helper on socket $2 as a foreign launch: a concurrent
# implementer launch shares the account and the socket group, so only the
# helper's process-ancestry check keeps it out.
refused_as_foreign() {
  as "$1" /usr/bin/python3 -c '
import socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(10)
s.connect(sys.argv[1])
reply = s.makefile("rb").readline()
sys.exit(0 if b"not from this helper\x27s launch" in reply else 1)' "$2"
}

# True when `stat -c '%a %U %G'` of $1 is $2.
has_mode() { [ "$(stat -c '%a %U %G' -- "$1")" = "$2" ]; }

# Releases the push mock in run $1 and waits up to 30 s for launch-root
# $2 to exit; kills it if it does not. Returns its exit status.
finish_push_launch() {
  as agentc-impl touch "$1/tmp/push-release" 2>/dev/null
  local deadline=$((SECONDS + 30))
  while kill -0 "$2" 2>/dev/null && [ "$SECONDS" -lt "$deadline" ]; do sleep 0.2; done
  kill -0 "$2" 2>/dev/null && kill -TERM "$2"
  wait "$2"
  local status=$?
  PUSH_PID=
  return "$status"
}

# A launch-root killed with SIGKILL: its helper must exit with it, and its
# per-launch directory stays behind for the next launch-root to sweep. Sets
# STALE_SESSION to that launch's session id.
check_push_kill() {
  local bin=$1 run=$STATE/impl/runs/suite-push-killed helper log=/root/agentc-push-killed.log
  STALE_SESSION=$(cat /proc/sys/kernel/random/uuid)
  fresh_impl_run "$run"
  start_push_launch "$bin" "$STALE_SESSION" "$run" "$log"
  if ! await_file "$run/tmp/push-ready" 60 "$PUSH_PID"; then
    fail "agentc-impl: launch-root (to be killed) reached its harness (log: $log)"
    finish_push_launch "$run" "$PUSH_PID"; return
  fi
  helper=$(helper_pid "$STALE_SESSION")
  expect_ok "agentc-push: helper serves the launch-root to be killed" test -n "$helper"
  kill -KILL "$PUSH_PID"; wait "$PUSH_PID" 2>/dev/null; PUSH_PID=
  expect_ok "agentc-push: helper exits when its launch-root is SIGKILLed" \
    await_no_process "--launch=$STALE_SESSION" 10
  as agentc-impl touch "$run/tmp/push-release"
  expect_ok "agentc-impl: orphaned launch ends once released" \
    await_no_process "--session-id=$STALE_SESSION" 30
  expect_ok "SIGKILLed launch-root leaves its push directory (sweep precondition)" \
    test -d "$STATE/push/$STALE_SESSION"
}

# Checks made while the helper of session $1 (launch dir $2, run $3) serves:
# account and groups, socket and directory modes, who can connect, the
# harness's socket path, and that this launch-root swept the stale directory.
check_live_helper() {
  local session=$1 dir=$2 run=$3 helper
  helper=$(helper_pid "$session")
  expect_ok "agentc-push: helper runs as $PUSH_USER with only its own groups" \
    runs_as_account "${helper:-0}" "$PUSH_USER"
  expect_ok "agentc-push: socket is 0660 $PUSH_USER:agentc-impl" \
    has_mode "$dir/sock/push.sock" "660 $PUSH_USER agentc-impl"
  expect_ok "agentc-push: socket directory is 2750 $PUSH_USER:agentc-impl" \
    has_mode "$dir/sock" "2750 $PUSH_USER agentc-impl"
  expect_ok "agentc-push: launch directory is 711 root:root" has_mode "$dir" "711 root root"
  expect_ok "agentc-impl: can connect to its launch's push socket" connects_as agentc-impl "$dir/sock/push.sock"
  expect_fail "agentc-rev: cannot connect to the push socket" connects_as agentc-rev "$dir/sock/push.sock"
  expect_ok "agentc-impl outside the launch (another launch): helper refuses it" \
    refused_as_foreign agentc-impl "$dir/sock/push.sock"
  expect_ok "agentc-impl/claude: harness sees only its own push socket" \
    test "$(cat "$run/tmp/push-ready")" = "$dir/sock/push.sock"
  expect_fail "next launch-root swept the SIGKILLed launch's directory" \
    test -e "$STATE/push/${STALE_SESSION:-none}"
}

# A real implementer launch-root (R-P3b.2) through push bin $1: the helper
# runs as its own account beside the launch, and both are gone afterwards.
check_push_launch() {
  local bin=$1 run=$STATE/impl/runs/suite-push session dir log=/root/agentc-push.log status
  session=$(cat /proc/sys/kernel/random/uuid) dir=$STATE/push/$session
  fresh_impl_run "$run"
  start_push_launch "$bin" "$session" "$run" "$log"
  if await_file "$run/tmp/push-ready" 60 "$PUSH_PID"; then
    pass "agentc-impl/claude: launch-root started the helper and the harness checks passed"
    check_live_helper "$session" "$dir" "$run"
  else
    fail "agentc-impl/claude: launch-root started the helper and the harness checks passed (log: $log)"
    tail -n 40 "$log" | sed 's/^/    /'
  fi
  finish_push_launch "$run" "$PUSH_PID"; status=$?
  expect_ok "agentc-impl: launch-root exits 0 with the launch" test "$status" -eq 0
  expect_fail "agentc-push: per-launch directory removed after the launch" test -e "$dir"
  expect_fail "agentc-push: helper exited after the launch" \
    pgrep -f -- "--launch=$session"
}

# Whenever host-setup's APPARMOR_BWRAP=1 copy or profile exists: the
# supervisor uses it, only the role accounts may run it, it is still the
# distribution binary, and its profile enforces.
check_apparmor_bwrap() {
  local copy=$PREFIX/bin/bwrap
  [ -e "$copy" ] || [ -e /etc/apparmor.d/agentc-bwrap ] || return 0
  expect_ok "supervisor.toml uses the agentc bwrap" \
    grep -qx "bubblewrap = \"$copy\"" /etc/agentc/supervisor.toml
  expect_ok "agentc bwrap is 750 root:agentc-bwrap" has_mode "$copy" "750 root agentc-bwrap"
  expect_ok "agentc bwrap matches /usr/bin/bwrap (re-run host-setup after bwrap updates)" \
    cmp -s /usr/bin/bwrap "$copy"
  expect_ok "agentc-bwrap AppArmor profile is enforced" \
    grep -qx 'agentc-bwrap (enforce)' /sys/kernel/security/apparmor/profiles
  expect_ok "agentc-bwrap group is exactly agentc-impl,agentc-rev" bwrap_group_exact
  expect_fail "agentc-egress: cannot run the agentc bwrap" as agentc-egress "$copy" --version
  expect_fail "$PUSH_USER: cannot run the agentc bwrap" as "$PUSH_USER" "$copy" --version
}

# Succeeds when gid $1 is some account's primary group. awk reads all of
# getent's output, so pipefail never sees a SIGPIPE from an early grep -q exit.
primary_group() {
  getent passwd | awk -F: -v gid="$1" '$4 == gid { found = 1 } END { exit !found }'
}

# The agentc-bwrap group lists only the role accounts and is nobody's primary group.
bwrap_group_exact() {
  local entry gid
  entry=$(getent group agentc-bwrap) || return 1
  gid=$(echo "$entry" | cut -d: -f3)
  [ "$(echo "$entry" | cut -d: -f4 | tr ',' '\n' | sort | paste -sd,)" = agentc-impl,agentc-rev ] &&
    ! primary_group "$gid"
}

# Role $1's Claude token, read at spawn by the supervisor running as $1: a
# root-owned 0440 file $1 can read but not write and role $2 cannot read,
# which the real launch (check_claude_launch) received as
# CLAUDE_CODE_OAUTH_TOKEN. With the suite's dummy the layout checks still
# hold, and a NOTE says real-token authentication is not exercised. Never
# reads the token's bytes (is_dummy_token consults the suite's records).
check_claude_token() {
  local user=$1 other=$2 token=$STATE/${1#agentc-}/claude-token
  if [ ! -e "$token" ]; then fail "$user: claude-token exists at $token"; return; fi
  if is_dummy_token "$token"; then
    echo "NOTE $user: claude-token checks use the suite's dummy token; real-token authentication is not exercised"
  fi
  expect_ok "$user: claude-token is 440 root:$user" has_mode "$token" "440 root $user"
  expect_ok "$user: can read its claude-token" as "$user" test -r "$token"
  expect_fail "$user: cannot write its claude-token" as "$user" test -w "$token"
  expect_fail "$other: cannot read ${user}'s claude-token" as "$other" test -r "$token"
  expect_ok "$user/claude: real launch received CLAUDE_CODE_OAUTH_TOKEN" \
    test -e "$STATE/${1#agentc-}/runs/suite-launch/tmp/suite-harness-token"
}

# The reviewer's candidate sandbox (R-P3b.3) can neither read rev/claude-token
# nor see CLAUDE_CODE_OAUTH_TOKEN; the mock harness's candidate probe in the
# real launch records that. A real or dummy token must exist, or the probe
# proves nothing.
check_candidate_token() {
  local token=$STATE/rev/claude-token
  if [ ! -e "$token" ]; then fail "agentc-rev: claude-token exists at $token"; return; fi
  expect_ok "agentc-rev: candidate sandbox cannot read claude-token or CLAUDE_CODE_OAUTH_TOKEN" \
    test -e "$STATE/rev/runs/suite-launch/tmp/suite-token-hidden"
}

# Only the helper account can read the push App key; skipped without one.
check_push_key() {
  if [ ! -e "$PUSH_KEY" ]; then echo "SKIP push App key checks (no $PUSH_KEY on this host)"; return; fi
  expect_ok "push App key is 400 $PUSH_USER:$PUSH_USER" has_mode "$PUSH_KEY" "400 $PUSH_USER $PUSH_USER"
  expect_ok "$PUSH_USER: can read the push App key" as "$PUSH_USER" test -r "$PUSH_KEY"
  expect_fail "agentc-impl: cannot read the push App key" as agentc-impl test -r "$PUSH_KEY"
  expect_fail "agentc-rev: cannot read the push App key" as agentc-rev test -r "$PUSH_KEY"
}

# The owner-minted coordinator token is root-only: neither role can read it
# (launches never hold a coordinator credential); skipped without one.
check_attention_token() {
  if [ ! -e "$ATTENTION_TOKEN" ]; then echo "SKIP attention token checks (no $ATTENTION_TOKEN on this host)"; return; fi
  expect_ok "attention token is 400 root:root" has_mode "$ATTENTION_TOKEN" "400 root root"
  expect_fail "agentc-impl: cannot read the attention token" as agentc-impl test -r "$ATTENTION_TOKEN"
  expect_fail "agentc-rev: cannot read the attention token" as agentc-rev test -r "$ATTENTION_TOKEN"
}

# Succeeds when $1 resolves into /snap or is a script that hands off to a snap
# (Ubuntu's /usr/bin/chromium-browser), which cannot run as a role account.
is_snap_wrapper() {
  local resolved
  resolved=$(readlink -f -- "$1")
  case $resolved in /snap/*) return 0 ;; esac
  [ "$(head -c 2 -- "$resolved")" = '#!' ] && grep -q '/snap/' -- "$resolved"
}

# The configured browser $1 is no snap wrapper, and host-setup's pinned
# headless shell is root-owned and closed to group/world writes.
check_browser_install() {
  local browser=$1
  expect_fail "reviewer browser $browser is not a snap wrapper" is_snap_wrapper "$browser"
  case $browser in "$PREFIX"/browsers/*) check_pinned_shell "$browser" ;; esac
}

# With the agentc-browser profile loaded (it grants user namespaces to whoever
# runs the shell), only the role accounts may run the pinned shell: it is
# 750 root:agentc-bwrap and neither agentc-egress nor the push account can
# execute it. Without the profile it is 755 root:root.
check_pinned_shell() {
  local shell=$1
  if [ ! -e /etc/apparmor.d/agentc-browser ]; then
    expect_ok "pinned headless shell is 755 root:root" has_mode "$shell" "755 root root"; return
  fi
  expect_ok "pinned headless shell is 750 root:agentc-bwrap" has_mode "$shell" "750 root agentc-bwrap"
  expect_fail "agentc-egress: cannot run the pinned headless shell" as agentc-egress "$shell" --version
  expect_fail "$PUSH_USER: cannot run the pinned headless shell" as "$PUSH_USER" "$shell" --version
}

# The reviewer's pinned headless browser renders the staging dashboard under
# uid + firewall (plan M2), and the implementer cannot read the reviewer's
# verification logins. Skipped when no staging coordinator is listening.
check_browser() {
  local browser run=$STATE/rev/runs/suite
  browser=$(sed -n 's/^browser = "\(.*\)"$/\1/p' /etc/agentc/supervisor.toml)
  browser=${browser:-/usr/bin/chromium}
  expect_fail "agentc-impl: cannot read the reviewer's verification logins" as agentc-impl ls "$STATE/rev/verification"
  check_browser_install "$browser"
  if ! curl -sf --max-time 3 "$STAGING/healthz" >/dev/null; then
    echo "SKIP agentc-rev: browser check (no staging coordinator at $STAGING)"; return
  fi
  expect_ok_logged "agentc-rev: headless browser renders the staging dashboard" /root/agentc-browser.log \
    as agentc-rev sh -c "$browser --headless=new --disable-gpu --no-first-run \
      --user-data-dir=$run/tmp/chrome --dump-dom $STAGING/ | grep -q 'Agent Coordinator'"
}

# The Bubblewrap the project's real-sandbox tests run: the agentc copy when
# host-setup installed one (Ubuntu's AppArmor userns restriction refuses the
# unconfined /usr/bin/bwrap), else the tests' own default.
test_bwrap_env() {
  if [ -x "$PREFIX/bin/bwrap" ]; then echo "AGENTC_TEST_BWRAP=$PREFIX/bin/bwrap"; fi
}

# Optional: the project's tests pass under the implementer profile, both
# plainly (Claude: uid + firewall) and inside the Codex sandbox. Inside the
# Codex sandbox Bubblewrap cannot nest, so AGENTC_TEST_NESTED_SANDBOX=1 makes
# the real-Bubblewrap tests skip, each with a note in the leg's log; the plain
# leg runs them.
check_cargo_test() {
  local base=$STATE/impl clone=$STATE/impl/clones/suite run=$STATE/impl/runs/suite
  local env="PATH=$PREFIX/bin:$PREFIX/cargo/bin:/usr/bin:/bin RUSTUP_HOME=$PREFIX/rustup CARGO_HOME=$run/state/cargo CARGO_TARGET_DIR=$run/target HTTPS_PROXY=$PROXY HTTP_PROXY=$PROXY NO_PROXY=127.0.0.1,localhost $(test_bwrap_env)"
  expect_ok_logged "impl: cargo test (uid + firewall)" /root/agentc-cargo-test.log \
    as agentc-impl sh -c "cd $clone && env $env cargo test --workspace --locked --no-fail-fast"
  local roots="sandbox_workspace_write.writable_roots=[\"$run\",\"$run/state/cargo\"]"
  expect_ok_logged "impl: cargo test inside codex sandbox" /root/agentc-cargo-test-codex.log as agentc-impl sh -c \
    "cd $clone && env $env AGENTC_TEST_NESTED_SANDBOX=1 CODEX_HOME=$base/codex-home $PREFIX/bin/codex sandbox -c sandbox_mode=workspace-write -c sandbox_workspace_write.network_access=true -c '$roots' -- cargo test --workspace --locked --no-fail-fast"
  expect_ok "impl: codex leg noted its skipped real-Bubblewrap tests" \
    grep -q '^note: skipping .* (AGENTC_TEST_NESTED_SANDBOX is set)$' /root/agentc-cargo-test-codex.log
}

main() {
  [ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
  local cargo_test=${1:-} relay_bin stub_bin push_bin
  trap suite_cleanup EXIT
  install_dummy_tokens
  make_suite_bin "$SUP"; relay_bin=$SUITE_BIN
  make_suite_bin /bin/false; stub_bin=$SUITE_BIN
  make_suite_bin "$SUP"; push_bin=$SUITE_BIN
  fill_push_bin "$push_bin" || { echo "cannot create the push suite bin dir" >&2; exit 1; }
  check_apparmor_bwrap
  for pair in "agentc-impl agentc-rev" "agentc-rev agentc-impl"; do
    set -- $pair
    prepare_clone "$1"
    check_profiles "$1"
    check_git "$1"
    check_writes "$1" "$2"
    check_seeds "$1"
    check_egress "$1"
    check_codex_sandbox "$1"
    check_claude_sandbox "$1"
    check_claude_launch "$1" "$relay_bin"
    check_claude_token "$1" "$2"
    check_relay_probe "$1" "$stub_bin"
  done
  check_candidate_token
  check_push_kill "$push_bin"
  check_push_launch "$push_bin"
  check_push_key
  check_attention_token
  check_browser
  if [ "$cargo_test" = "--cargo-test" ]; then check_cargo_test; fi
  [ "$FAILED" -eq 0 ] && echo "containment suite: all checks passed" || echo "containment suite: FAILURES above"
  exit "$FAILED"
}

main "$@"
