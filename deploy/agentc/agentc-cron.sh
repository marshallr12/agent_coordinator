#!/bin/sh
# Runs one agentc timer job from cron on hosts without systemd, in place of
# the systemd service host-setup.sh installs elsewhere: it loads the job's
# environment files the way systemd's EnvironmentFile= does, runs the command
# and appends its output, with start and exit lines, to
# /var/log/agentc-<name>.log.
#
#   agentc-cron --name NAME [--env FILE]... [--env-optional FILE]... -- COMMAND [ARG]...
#
# --name comes first: it names the log every later message goes to.
set -u

NAME=
LOG_DIR=${AGENTC_CRON_LOG_DIR:-/var/log}

# Prints a timestamped line for this job.
note() {
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) agentc-$NAME: $*"
}

# Exports each KEY=VALUE line of file $1 as systemd's EnvironmentFile= reads
# the plain lines host-setup writes: blanks, comments and lines without "="
# are skipped, whitespace around the line, the key and the value is trimmed,
# and one pair of surrounding quotes is removed. Values are taken literally:
# nothing is expanded or executed. Unlike systemd it does not join lines
# ending in a backslash or unquote several quoted words.
load_env() {
  local_line=
  while read -r local_line || [ -n "$local_line" ]; do
    case $local_line in ''|'#'*|';'*) continue ;; *=*) ;; *) note "ignoring line without = in $1"; continue ;; esac
    local_key=$(printf '%s' "${local_line%%=*}" | sed 's/[[:space:]]*$//')
    local_value=$(printf '%s' "${local_line#*=}" | sed 's/^[[:space:]]*//')
    case $local_key in ''|[0-9]*|*[!A-Za-z0-9_]*) note "ignoring line in $1: $local_key"; continue ;; esac
    case $local_value in \"*\"|\'*\') local_value=${local_value#?}; local_value=${local_value%?} ;; esac
    export "$local_key=$local_value"
  done < "$1"
}

# Reads the options up to `--`, loading environment files as it goes; leaves
# the command in "$@" via SHIFT_BY.
parse() {
  SHIFT_BY=0
  while [ $# -gt 0 ]; do
    case $1 in
      --env) [ -f "$2" ] || { note "missing environment file $2"; exit 1; }; load_env "$2" ;;
      --env-optional) [ ! -f "$2" ] || load_env "$2" ;;
      --) SHIFT_BY=$((SHIFT_BY + 1)); return 0 ;;
      *) note "unknown option $1"; exit 2 ;;
    esac
    shift 2; SHIFT_BY=$((SHIFT_BY + 2))
  done
  note "no command after --"; exit 2
}

# Runs the job with its output in its log and reports the exit status.
main() {
  case ${1:-}:${2:-} in
    --name:*[!A-Za-z0-9_-]*|--name:) echo "agentc-cron: --name NAME (letters, digits, - and _) comes first" >&2; exit 2 ;;
    --name:*) NAME=$2 ;;
    *) echo "agentc-cron: --name NAME comes first" >&2; exit 2 ;;
  esac
  shift 2
  exec >>"$LOG_DIR/agentc-$NAME.log" 2>&1
  parse "$@"
  shift "$SHIFT_BY"
  note "start: $*"
  status=0
  "$@" || status=$?
  note "exit $status"
  exit "$status"
}

main "$@"
