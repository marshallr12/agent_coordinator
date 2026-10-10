#!/bin/sh
# Quiet hours for a supervisor host that is also someone's workstation. Run
# every minute from /etc/cron.d/agentc-quiet-hours (host-setup.sh writes it
# when QUIET_HOURS is set). WINDOW is the local-time span in which the
# supervisor may claim work, such as 22:00-07:00; outside it this script holds
# the kill switch, so no new work is claimed while running launches finish.
#
#   agentc-quiet-hours WINDOW [KILL_SWITCH]   # apply the window now
#   agentc-quiet-hours --release [KILL_SWITCH]  # drop our switch (quiet hours off)
#
# The switch it creates holds exactly MARK, and it only ever removes a switch
# holding MARK: one the owner set (by touch, or with any other content) stays.
# QUIET_HOURS_NOW=HH:MM overrides the clock (tests).
set -u

MARK=quiet-hours

# Prints the minutes since midnight of HH:MM, or fails on a malformed time.
minutes() {
  case $1 in
    [0-2][0-9]:[0-5][0-9]) ;;
    *) return 1 ;;
  esac
  # The leading 1 keeps a zero-padded field from being read as octal.
  hour=$(( 1${1%:*} - 100 )); minute=$(( 1${1#*:} - 100 ))
  [ "$hour" -le 23 ] || return 1
  echo $(( hour * 60 + minute ))
}

# Succeeds when minute $1 lies in [start $2, end $3), wrapping past midnight.
inside() {
  if [ "$2" -lt "$3" ]; then [ "$1" -ge "$2" ] && [ "$1" -lt "$3" ]
  else [ "$1" -ge "$2" ] || [ "$1" -lt "$3" ]; fi
}

# Succeeds when the switch at $1 is ours (holds exactly MARK).
ours() {
  [ -f "$1" ] && [ ! -L "$1" ] && [ "$(cat -- "$1")" = "$MARK" ]
}

# Creates our switch at $1 unless some switch is already there.
hold() {
  [ -e "$1" ] || [ -L "$1" ] || printf '%s\n' "$MARK" > "$1"
}

# Removes the switch at $1 only when it is ours.
release() {
  if ours "$1"; then rm -f -- "$1"; fi
}

# Validates WINDOW ($1) and holds or releases the switch at $2 for now.
apply() {
  start=$(minutes "${1%-*}") && end=$(minutes "${1#*-}") && [ "$start" != "$end" ] ||
    { echo "agentc-quiet-hours: WINDOW must be HH:MM-HH:MM with different ends, not '$1'" >&2; exit 2; }
  now=$(minutes "${QUIET_HOURS_NOW:-$(date +%H:%M)}") || { echo "agentc-quiet-hours: bad QUIET_HOURS_NOW" >&2; exit 2; }
  if inside "$now" "$start" "$end"; then release "$2"; else hold "$2"; fi
}

main() {
  [ $# -ge 1 ] || { echo "usage: agentc-quiet-hours WINDOW|--release [KILL_SWITCH]" >&2; exit 2; }
  switch=${2:-/var/lib/agentc/kill-switch}
  if [ "$1" = --release ]; then release "$switch"; else apply "$1" "$switch"; fi
}

main "$@"
