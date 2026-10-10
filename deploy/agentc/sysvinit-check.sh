#!/bin/bash
# Live check of host-setup.sh's sysvinit pieces on a host without systemd, run
# as root from the repository: sudo deploy/agentc/sysvinit-check.sh
#
# It never installs or enables the real loop. A fake supervisor stands in for
# agentc-supervisor, everything else lives in a temporary directory, and only
# three system paths are touched and restored: /run/agentc-run.pid and
# /var/log/agentc-run.log (both must be absent: no agentc-run may be running),
# a temporary /etc/init.d/agentc-run for insserv's dry run (no rc links), and a
# temporary /etc/cron.d entry. Checks:
#   1. insserv accepts the LSB script's boot order (after agentc-firewall and
#      agentc-egress) in a dry run;
#   2. start runs the wrapper, a crash restarts the loop after 30 s, status works;
#   3. stop passes SIGTERM for the drain, then kills what is left in the group;
#   4. cron runs agentc-cron from /etc/cron.d with an environment file.
set -euo pipefail

HERE=$(dirname "$(readlink -f "$0")")
T=$(mktemp -d /tmp/agentc-sysvinit-check.XXXXXX)
PIDFILE=/run/agentc-run.pid
LOG=/var/log/agentc-run.log
INITD=/etc/init.d/agentc-run
CRON_CHECK=/etc/cron.d/agentc-sysvinit-check
FAILED=0

# Prints PASS or FAIL for check $1, decided by running the rest as a command.
check() {
  local what=$1; shift
  if "$@"; then echo "PASS $what"; else echo "FAIL $what"; FAILED=1; fi
}

# Exits unless this is root on a host without systemd and no agentc-run runs.
preflight() {
  [ "$(id -u)" = 0 ] || { echo "run as root (sudo $0)" >&2; exit 2; }
  [ ! -d /run/systemd/system ] || { echo "this host runs systemd; the check is for sysvinit hosts" >&2; exit 2; }
  for path in "$PIDFILE" "$LOG" "$INITD" "$CRON_CHECK"; do
    [ ! -e "$path" ] || { echo "$path exists: is agentc-run installed or running? stop and remove it first" >&2; exit 2; }
  done
  command -v start-stop-daemon insserv cron >/dev/null || { echo "needs start-stop-daemon, insserv and cron" >&2; exit 2; }
}

# Stops the fake loop and removes every file the check created.
cleanup() {
  [ ! -x "$T/agentc-run" ] || "$T/agentc-run" stop >/dev/null 2>&1 || true
  rm -f -- "$INITD" "$CRON_CHECK" "$PIDFILE" "$LOG"
  rm -rf -- "$T"
}

# The fake `agentc-supervisor run`: records each run, crashes (exit 3) once
# when $T/crash exists, otherwise starts a TERM-ignoring "launch" in its
# process group and drains for 2 s on SIGTERM.
fake_supervisor() {
  cat <<EOF
#!/bin/sh
echo \$\$ >> $T/runs
if [ -f $T/crash ]; then rm -f $T/crash; exit 3; fi
sh -c 'trap "" TERM; echo \$\$ > $T/launch; exec sleep 600' &
trap 'sleep 2; echo drained >> $T/drained; exit 0' TERM
while :; do sleep 0.2; done
EOF
}

# Builds the init script with host-setup.sh's own generator, pointed at the
# fake supervisor and the wrapper copied into $T. host-setup.sh is sourced in
# a subshell, so its functions (its own main among them) never replace ours.
build_fixture() {
  mkdir -p "$T/bin"
  install -m 0755 "$HERE/agentc-run-sysv.sh" "$T/agentc-run-sysv"
  fake_supervisor > "$T/bin/agentc-supervisor"; chmod 0755 "$T/bin/agentc-supervisor"
  # shellcheck source=host-setup.sh
  (source "$HERE/host-setup.sh"; PREFIX=$T; RUN_WRAPPER=$T/agentc-run-sysv; run_sysv_script) > "$T/agentc-run"
  chmod 0755 "$T/agentc-run"
}

# Succeeds once command $2... succeeds, polling for up to $1 seconds.
within() {
  local seconds=$1; shift
  for _ in $(seq $((seconds * 5))); do "$@" && return 0; sleep 0.2; done
  return 1
}

runs() { [ "$(wc -l < "$T/runs" 2>/dev/null || echo 0)" -ge "$1" ]; }
alive() { kill -0 "$(cat "$1")" 2>/dev/null; }
dead() { ! alive "$1"; }

# insserv dry run: the script's Required-Start/Stop resolve on this host.
check_boot_order() {
  install -m 0755 "$T/agentc-run" "$INITD"
  check "insserv accepts the boot order (dry run)" insserv -n "$INITD"
  rm -f -- "$INITD"
}

# start, crash restart and status.
check_start_and_restart() {
  touch "$T/crash"
  "$T/agentc-run" start
  check "start writes the pidfile and the wrapper runs" within 5 alive "$PIDFILE"
  check "status reports running" "$T/agentc-run" status
  echo "waiting about 30 s for the crash restart..."
  check "a crash restarts the loop after the delay" within 45 runs 2
  check "the restart is logged" grep -q "exited 3; restarting in 30s" "$LOG"
  check "the restarted loop started a launch" within 5 test -s "$T/launch"
}

# stop: drain, then the group kill takes the TERM-ignoring launch.
check_stop() {
  "$T/agentc-run" stop
  check "stop let the loop drain" grep -q drained "$T/drained"
  check "stop killed the leftover launch in the group" within 5 dead "$T/launch"
  check "stop removed the pidfile" test ! -e "$PIDFILE"
  check "status reports stopped" bash -c "! '$T/agentc-run' status"
}

# cron runs agentc-cron from /etc/cron.d with an environment file.
check_cron() {
  install -m 0755 "$HERE/agentc-cron.sh" "$T/agentc-cron"
  printf 'CHECK_VALUE="from env file"\n' > "$T/check.env"
  # Single quotes keep cron's own shell from expanding $CHECK_VALUE: only
  # the job's shell, after agentc-cron loaded check.env, may.
  printf 'AGENTC_CRON_LOG_DIR=%s\n* * * * * root %s --name check --env %s -- /bin/sh -c %s\n' \
    "$T" "$T/agentc-cron" "$T/check.env" "'echo value=\$CHECK_VALUE'" > "$CRON_CHECK"
  echo "waiting up to 70 s for cron..."
  check "cron ran agentc-cron with the environment file" within 70 grep -q "value=from env file" "$T/agentc-check.log"
  rm -f -- "$CRON_CHECK"
}

main() {
  preflight
  trap cleanup EXIT
  build_fixture
  check_boot_order
  check_start_and_restart
  check_stop
  check_cron
  if [ "$FAILED" = 0 ]; then echo "sysvinit check: all PASS"; return 0; fi
  echo "--- $LOG"; cat "$LOG" 2>/dev/null || true
  echo "--- $T/agentc-check.log"; cat "$T/agentc-check.log" 2>/dev/null || true
  echo "sysvinit check: FAILURES above"; exit 1
}

main "$@"
