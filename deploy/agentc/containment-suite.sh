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
DISABLED_PUSH=disabled://push-only-via-agent-coordinator
FAILED=0

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
  rm -rf "$base/clones/suite" "$base/runs/suite"
  expect_ok "$user: hardened clone at mirror HEAD" as "$user" "$SUP" clone \
    --url "$STATE/mirror.git" --revision "$(git -C "$STATE/mirror.git" rev-parse HEAD)" \
    --dest "$base/clones/suite"
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

# Optional: the project's tests pass under the implementer profile, both
# plainly (Claude: uid + firewall) and inside the Codex sandbox.
check_cargo_test() {
  local base=$STATE/impl clone=$STATE/impl/clones/suite run=$STATE/impl/runs/suite
  local env="PATH=$PREFIX/bin:$PREFIX/cargo/bin:/usr/bin:/bin RUSTUP_HOME=$PREFIX/rustup CARGO_HOME=$base/cargo CARGO_TARGET_DIR=$run/target HTTPS_PROXY=$PROXY HTTP_PROXY=$PROXY NO_PROXY=127.0.0.1,localhost"
  expect_ok_logged "impl: cargo test (uid + firewall)" /root/agentc-cargo-test.log \
    as agentc-impl sh -c "cd $clone && env $env cargo test --workspace --locked --no-fail-fast"
  local roots="sandbox_workspace_write.writable_roots=[\"$run\",\"$base/cargo\"]"
  expect_ok_logged "impl: cargo test inside codex sandbox" /root/agentc-cargo-test-codex.log as agentc-impl sh -c \
    "cd $clone && env $env CODEX_HOME=$base/codex-home $PREFIX/bin/codex sandbox -c sandbox_mode=workspace-write -c sandbox_workspace_write.network_access=true -c '$roots' -- cargo test --workspace --locked --no-fail-fast"
}

main() {
  [ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
  local cargo_test=${1:-}
  for pair in "agentc-impl agentc-rev" "agentc-rev agentc-impl"; do
    set -- $pair
    prepare_clone "$1"
    check_profiles "$1"
    check_git "$1"
    check_writes "$1" "$2"
    check_egress "$1"
    check_codex_sandbox "$1"
  done
  if [ "$cargo_test" = "--cargo-test" ]; then check_cargo_test; fi
  [ "$FAILED" -eq 0 ] && echo "containment suite: all checks passed" || echo "containment suite: FAILURES above"
  exit "$FAILED"
}

main "$@"
