#!/usr/bin/env bash
# Owner-run bootstrap on oracle-1. Installs but never enables/starts a unit.
# sudo INTEGRATOR=/absolute/path/agentc-integrator deploy/agentc/integrator-host-setup.sh
set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo 'run with sudo' >&2; exit 1; }
: "${INTEGRATOR:?set INTEGRATOR to the built host-native binary}"
[ -d /run/systemd/system ] || { echo 'systemd is required' >&2; exit 1; }
systemd_version=$(systemctl --version | awk 'NR==1 {print $2}')
[[ "$systemd_version" =~ ^[0-9]+$ ]] && [ "$systemd_version" -ge 247 ] || {
  echo 'systemd >=247 is required for LoadCredential' >&2; exit 1;
}
[ -x "$INTEGRATOR" ] || { echo 'integrator binary is not executable' >&2; exit 1; }
# Refuse symlinks at every destination root uses; keep existing secrets/config.
for path in /opt/agentc /opt/agentc/bin /opt/agentc/bin/agentc-integrator \
  /etc/agentc /etc/agentc/integrator.toml /etc/agentc/integrator-app.pem \
  /etc/agentc/integrator-credentials.toml /var/lib/agentc \
  /var/lib/agentc/integrator /etc/systemd/system/agentc-integrator@.service; do
  [ ! -L "$path" ] || { echo "refusing symlink: $path" >&2; exit 1; }
done
for secret in /etc/agentc/integrator-app.pem /etc/agentc/integrator-credentials.toml; do
  if [ -e "$secret" ]; then
    [ -f "$secret" ] || { echo "not a regular file: $secret" >&2; exit 1; }
    case "$(stat -c '%u:%g:%a' "$secret")" in
      0:0:400|0:0:600) ;;
      *) echo "require root:root mode 0400 or 0600: $secret" >&2; exit 1 ;;
    esac
  fi
done
if [ -e /etc/agentc/integrator.toml ]; then
  [ -f /etc/agentc/integrator.toml ] && [ -O /etc/agentc/integrator.toml ] || {
    echo 'require root-owned regular integrator.toml' >&2; exit 1;
  }
  [ "$(stat -c '%a' /etc/agentc/integrator.toml)" = 644 ] || {
    echo 'require integrator.toml mode 0644' >&2; exit 1;
  }
fi
id agentc-integrator >/dev/null 2>&1 || useradd --system --user-group \
  --home-dir /var/lib/agentc/integrator --no-create-home \
  --shell /usr/sbin/nologin agentc-integrator
install -d -o root -g root -m 0755 /opt/agentc /opt/agentc/bin /etc/agentc /var/lib/agentc
# Let the uid create its own subdirectories; root must not traverse uid-owned state.
install -d -o agentc-integrator -g agentc-integrator -m 0700 /var/lib/agentc/integrator
for mode in shadow run; do
  if systemctl is-active --quiet "agentc-integrator@$mode.service"; then
    systemctl stop "agentc-integrator@$mode.service"
  fi
done
install -o root -g root -m 0755 "$INTEGRATOR" /opt/agentc/bin/agentc-integrator
if [ ! -e /etc/agentc/integrator.toml ]; then
  install -o root -g root -m 0644 /dev/null /etc/agentc/integrator.toml
  cat > /etc/agentc/integrator.toml <<'CONFIG'
origin = "https://agents.sithbit.com"
allow_insecure_loopback = false
projects = [] # owner: insert the production project id before starting
checks = "github"
credential_file = "/run/credentials/agentc-integrator@shadow.service/coordinator"
[github]
app_id = 5127380
installation_id = 166293403
private_key = "/run/credentials/agentc-integrator@shadow.service/github-key"
CONFIG
fi
cat > /etc/systemd/system/agentc-integrator@.service <<'UNIT'
[Unit]
Description=Agent Coordinator integrator (%i)
Wants=network-online.target
After=network-online.target
[Service]
User=agentc-integrator
Group=agentc-integrator
ExecStart=/usr/bin/flock --nonblock /var/lib/agentc/integrator/daemon.lock /opt/agentc/bin/agentc-integrator --config /etc/agentc/integrator.toml %i
LoadCredential=github-key:/etc/agentc/integrator-app.pem
LoadCredential=coordinator:/etc/agentc/integrator-credentials.toml
Environment=HOME=/var/lib/agentc/integrator
Environment=GIT_CONFIG_GLOBAL=/dev/null
Environment=GIT_CONFIG_NOSYSTEM=1
UMask=0077
Restart=always
RestartSec=30
MemoryMax=256M
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=/var/lib/agentc/integrator
[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
printf '%s\n' 'Installed; no unit enabled or started. Follow the integrator cutover runbook.'
