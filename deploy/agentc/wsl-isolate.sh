#!/usr/bin/env bash
# Dedicated WSL distro only. Install after disabling automount/interop in wsl.conf.
set -euo pipefail

isolate_mounts() {
  # WSL propagates shared mounts between distros. Detach this mount namespace
  # BEFORE unmounting anything, so Docker and other distros retain their mounts.
  mount --make-rprivate /
  local target
  for target in /mnt/wsl /mnt/wslg /mnt/c /usr/lib/wsl/drivers /usr/lib/wsl/lib; do
    # WSL can stack more than one mount at the same path. Never force or lazily
    # detach a busy tree: a failure must block the supervisor's dependency.
    while mountpoint -q -- "$target"; do
      umount --recursive -- "$target"
    done
  done
}

write_units() {
  local directory=$1
  mkdir -p "$directory/agentc-run.service.d"
  cat > "$directory/agentc-wsl-isolation.service" <<'UNIT'
[Unit]
Description=Isolate shared WSL mounts before supervised agent work
After=local-fs.target
Before=agentc-run.service

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/agentc-wsl-isolate
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT
  cat > "$directory/agentc-run.service.d/wsl-isolation.conf" <<'UNIT'
[Unit]
Requires=agentc-wsl-isolation.service
After=agentc-wsl-isolation.service
UNIT
}

protected_path() {
  local path=$1 mode
  [ ! -L "$path" ] || { echo "refusing symlink: $path" >&2; exit 1; }
  if [ -e "$path" ]; then
    [ "$(stat -c %u -- "$path")" = 0 ] || { echo "not root-owned: $path" >&2; exit 1; }
    mode=$(stat -c %a -- "$path")
    (( (8#$mode & 0022) == 0 )) || { echo "writable protected path: $path" >&2; exit 1; }
    [ -d "$path" ] || [ "$(stat -c %h -- "$path")" = 1 ] || { echo "linked protected file: $path" >&2; exit 1; }
  fi
}

main() {
  [ "$(id -u)" = 0 ] || { echo 'run with sudo in the dedicated distro' >&2; exit 1; }
  [[ $(uname -r) == *[Ww][Ss][Ll]2* ]] || { echo 'WSL2 is required' >&2; exit 1; }
  [ "$(wslinfo --networking-mode)" = nat ] || { echo 'actual NAT networking is required' >&2; exit 1; }
  case ${1:-} in
    --install)
      if systemctl is-active --quiet agentc-run.service; then
        echo 'drain agentc-run before changing distro isolation' >&2; exit 1
      fi
      local path
      for path in /usr /usr/local /usr/local/sbin /usr/local/sbin/agentc-wsl-isolate \
        /etc /etc/systemd /etc/systemd/system \
        /etc/systemd/system/agentc-wsl-isolation.service \
        /etc/systemd/system/agentc-run.service.d \
        /etc/systemd/system/agentc-run.service.d/wsl-isolation.conf; do
        protected_path "$path"
      done
      install -o root -g root -m 0755 "${BASH_SOURCE[0]}" /usr/local/sbin/agentc-wsl-isolate
      write_units /etc/systemd/system
      systemctl daemon-reload
      systemctl enable agentc-wsl-isolation.service
      echo 'Installed. Restart the dedicated distro to apply wsl.conf before checking admission.'
      ;;
    '') isolate_mounts ;;
    *) echo 'usage: wsl-isolate.sh [--install]' >&2; exit 2 ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
