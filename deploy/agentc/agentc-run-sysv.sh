#!/bin/sh
# Keeps `agentc-supervisor run` running for the sysvinit agentc-run script,
# which host-setup.sh installs on hosts without systemd (MX Linux and others).
# It does what the systemd unit's Restart=on-failure and KillMode=mixed do:
# a crash (non-zero exit) restarts the loop after a delay; SIGTERM is passed
# to the loop, which drains, and the wrapper exits once the loop has.
#
#   agentc-run-sysv SUPERVISOR [RESTART_SECONDS]
set -u

SUPERVISOR=$1
DELAY=${2:-30}
CHILD=
STOPPING=

# Marks the stop and passes it to the running loop, which drains on SIGTERM.
on_term() {
  STOPPING=1
  [ -z "$CHILD" ] || kill -TERM "$CHILD" 2>/dev/null || true
}

# Prints a timestamped line to the log the init script redirects us to.
note() {
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) agentc-run: $*"
}

# Runs the loop until it exits, even when a signal interrupts the wait, and
# sets STATUS to its exit status.
run_once() {
  "$SUPERVISOR" run &
  CHILD=$!
  # A stop that arrived between the fork and CHILD=$! found no child to pass on to.
  [ -z "$STOPPING" ] || kill -TERM "$CHILD" 2>/dev/null || true
  STATUS=0
  wait "$CHILD" || STATUS=$?
  while kill -0 "$CHILD" 2>/dev/null; do
    STATUS=0
    wait "$CHILD" || STATUS=$?
  done
  CHILD=
}

# Sleeps DELAY seconds unless a stop arrives first.
pause() {
  sleep "$DELAY" &
  wait $! 2>/dev/null || true
  kill "$!" 2>/dev/null || true
}

# Restarts the loop after each crash; returns on a stop or a clean exit.
main() {
  trap on_term TERM INT
  while :; do
    run_once
    if [ -n "$STOPPING" ]; then note "stopped (exit $STATUS)"; return 0; fi
    if [ "$STATUS" -eq 0 ]; then note "exited cleanly"; return 0; fi
    note "exited $STATUS; restarting in ${DELAY}s"
    pause
    if [ -n "$STOPPING" ]; then note "stopped while waiting to restart"; return 0; fi
  done
}

main
