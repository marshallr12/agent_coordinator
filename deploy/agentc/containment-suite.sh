#!/usr/bin/env bash
# Verifies supervised-launch containment on this host (autonomy plan P2 exit
# criterion). No LLM is launched. Run as root after host-setup.sh:
#
#   sudo deploy/agentc/containment-suite.sh [--cargo-test]
#
# Prints one PASS/FAIL line per check and exits non-zero if any check fails.
set -uo pipefail

PREFIX=/opt/agentc
STATE=/var/lib/agentc
SUP=$PREFIX/bin/agentc-supervisor
PROXY=http://127.0.0.1:${PROXY_PORT:-3128}
STAGING=http://127.0.0.1:${STAGING_PORT:-18080}
DISABLED_PUSH=disabled://push-only-via-agent-coordinator
FAILED=0
SUITE_BINS=()

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

# Preflight and the candidate-instruction flags, per harness.
check_profiles() {
  local user=$1 base=$STATE/${1#agentc-} role
  role=$(role_of "$user")
  local spec=(--role "$role" --clone "$base/clones/suite" --run "$base/runs/suite")
  for harness in claude codex; do
    as "$user" "$SUP" prepare "${spec[@]}" --harness "$harness" >/dev/null 2>&1
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
# Probe create permission with harmless names; never unlink real auth or seeds.
check_seeds() {
  local user=$1 base=$STATE/${1#agentc-} path
  for path in "$base" "$base/claude-config" /etc/agentc; do
    expect_fail "$user: cannot create entries in protected $path" as "$user" touch "$path/.suite-seed-write"
  done
  for path in "$base/claude-config/settings.json" "$base/claude-config/CLAUDE.md" /etc/agentc/cargo-config.toml; do
    expect_fail "$user: protected seed is not writable: $path" as "$user" test -w "$path"
  done
  expect_ok "$user: credential file supports in-place writes" as "$user" test -w "$base/claude-config/.credentials.json"
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
write_mock_claude() {
  cat > "$1" <<EOF
#!/bin/sh
[ "\${1:-}" = --version ] && exec $PREFIX/bin/claude --version
set -eu
curl -sSf -o /dev/null --max-time 20 -x "\$HTTPS_PROXY" https://github.com/ || exit 3
if curl -sS -o /dev/null --max-time 5 --noproxy '*' https://github.com/; then exit 4; fi
[ -n "\${CLAUDE_CODE_SHELL_PREFIX:-}" ] || exit 0
"\$CLAUDE_CODE_SHELL_PREFIX" "curl -sSf -o /dev/null --max-time 20 -x \$HTTPS_PROXY https://github.com/ && pwd -P >| \$TMPDIR/suite-relay-cwd" || exit 5
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

# The reviewer's pinned headless browser renders the staging dashboard under
# uid + firewall (plan M2), and the implementer cannot read the reviewer's
# verification logins. Skipped when no staging coordinator is listening.
check_browser() {
  local browser run=$STATE/rev/runs/suite
  browser=$(sed -n 's/^browser = "\(.*\)"$/\1/p' /etc/agentc/supervisor.toml)
  expect_fail "agentc-impl: cannot read the reviewer's verification logins" as agentc-impl ls "$STATE/rev/verification"
  if ! curl -sf --max-time 3 "$STAGING/healthz" >/dev/null; then
    echo "SKIP agentc-rev: browser check (no staging coordinator at $STAGING)"; return
  fi
  expect_ok_logged "agentc-rev: headless browser renders the staging dashboard" /root/agentc-browser.log \
    as agentc-rev sh -c "${browser:-/usr/bin/chromium} --headless=new --disable-gpu --no-first-run \
      --user-data-dir=$run/tmp/chrome --dump-dom $STAGING/ | grep -q 'Agent Coordinator'"
}

# Optional: the project's tests pass under the implementer profile, both
# plainly (Claude: uid + firewall) and inside the Codex sandbox.
check_cargo_test() {
  local base=$STATE/impl clone=$STATE/impl/clones/suite run=$STATE/impl/runs/suite
  local env="PATH=$PREFIX/bin:$PREFIX/cargo/bin:/usr/bin:/bin RUSTUP_HOME=$PREFIX/rustup CARGO_HOME=$run/state/cargo CARGO_TARGET_DIR=$run/target HTTPS_PROXY=$PROXY HTTP_PROXY=$PROXY NO_PROXY=127.0.0.1,localhost"
  expect_ok_logged "impl: cargo test (uid + firewall)" /root/agentc-cargo-test.log \
    as agentc-impl sh -c "cd $clone && env $env cargo test --workspace --locked --no-fail-fast"
  local roots="sandbox_workspace_write.writable_roots=[\"$run\",\"$run/state/cargo\"]"
  expect_ok_logged "impl: cargo test inside codex sandbox" /root/agentc-cargo-test-codex.log as agentc-impl sh -c \
    "cd $clone && env $env CODEX_HOME=$base/codex-home $PREFIX/bin/codex sandbox -c sandbox_mode=workspace-write -c sandbox_workspace_write.network_access=true -c '$roots' -- cargo test --workspace --locked --no-fail-fast"
}

main() {
  [ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
  local cargo_test=${1:-} relay_bin stub_bin
  trap remove_suite_bins EXIT
  make_suite_bin "$SUP"; relay_bin=$SUITE_BIN
  make_suite_bin /bin/false; stub_bin=$SUITE_BIN
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
    check_relay_probe "$1" "$stub_bin"
  done
  check_browser
  if [ "$cargo_test" = "--cargo-test" ]; then check_cargo_test; fi
  [ "$FAILED" -eq 0 ] && echo "containment suite: all checks passed" || echo "containment suite: FAILURES above"
  exit "$FAILED"
}

main "$@"
