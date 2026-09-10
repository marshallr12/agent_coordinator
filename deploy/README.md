# Service deployment examples

These examples configure one service process with SQLite on a local filesystem.
Caddy terminates HTTPS; the service accepts only loopback listener addresses and
ignores forwarded headers. For checksum verification, package layout, supported
binary platform, upgrade boundaries, and removal, start with the
[Linux release installation guide](../docs/linux-installation.md).

1. Verify and extract the reviewed release package. Install its
   `agent-coordinator-server` and `agent-coordinator` binaries under
   `/usr/local/bin`. Assets and migrations are embedded in the server.
2. Create an unprivileged `agent-coordinator` system account and group, a
   `/var/lib/agent-coordinator` directory owned by that account with mode 0700,
   and `/etc/agent-coordinator` owned by root.
3. Copy `service.env.example` to `/etc/agent-coordinator/service.env`, set the
   real HTTPS origin, and protect it with mode 0640 and group `agent-coordinator`.
4. As the service account, initialize the administrator before starting the
   service (the password is entered through a hidden prompt):

   ```sh
   sudo -u agent-coordinator /usr/local/bin/agent-coordinator-server \
     --database /var/lib/agent-coordinator/coordinator.sqlite3 \
     --public-origin https://coordinator.example.com \
     init-admin --username admin
   ```

   Automation may add `--password-stdin` and supply the password through a
   protected pipe. There is deliberately no password command-line argument.
   Initialization succeeds only on an empty installation; it is not an account
   recovery command. Use **My account** for password changes, or the audited host
   `recover-operator-password` command described in [the operator guide](../docs/operator-guide.md)
   for account recovery.
5. Install `agent-coordinator.service` under `/etc/systemd/system`, reload
   systemd, and enable/start the service. Install Caddy with the matching public
   hostname from `Caddyfile.example`. Permit public HTTPS to Caddy and keep port
   8080 private. Configure DNS and normal certificate issuance for that hostname.
   Validate the actual configuration with `caddy validate --config /etc/caddy/Caddyfile`
   before reloading Caddy; see its [request-body size limit documentation](https://caddyserver.com/docs/caddyfile/directives/request_body).
6. Open the HTTPS site, sign in, and issue a named agent credential. The token
   is displayed once. If that response is lost, retry the same request/key to
   recover its identity, then rotate that credential under **Access** to issue
   a replacement for the same agent principal. A token is never replayed.
7. Create the separate backup repository and install the hourly backup units:

   ```sh
   sudo install -d -o agent-coordinator -g agent-coordinator -m 0700 \
     /var/lib/agent-coordinator-backups
   sudo install -o root -g root -m 0644 \
     deploy/agent-coordinator-backup.service \
     deploy/agent-coordinator-backup.timer \
     /etc/systemd/system/
   sudo systemctl daemon-reload
   sudo systemctl enable --now agent-coordinator-backup.timer
   sudo systemctl start agent-coordinator-backup.service
   ```

   Confirm that the first oneshot succeeded and verify its reported snapshot.
   Timer activation alone is not backup evidence. Configure an alert for a failed
   backup unit and stale verified-snapshot age. See the
   [backup and restore guide](../docs/backup-restore-guide.md) for retention,
   advisory-locked off-server copies, restore authority invalidation, and the
   required recovery exercise.

For local development only, use
`--public-origin http://127.0.0.1:8080 --allow-insecure-loopback`. The explicit
opt-in permits a non-Secure development cookie solely for a loopback origin.
HTTPS uses a Secure, HttpOnly, SameSite=Strict cookie with a 12-hour absolute
lifetime and a `__Host-` prefix. Browser writes require the exact configured
Origin, `X-CSRF-Token`, and `Idempotency-Key` headers. Login requires the origin
but has no idempotency receipt because it creates a fresh browser session.

Password hashes use Argon2id with 19 MiB memory, two iterations, and one lane,
matching the [OWASP password-storage minimum reviewed on 2026-09-09](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html).
Password checks run away from async workers, with at most two concurrent checks.
Sign-in is limited to five attempts per username and 30 total attempts per minute,
using a bounded in-process table and monotonic time. Restart resets that limiter;
use proxy/network controls for internet-scale traffic. The service does not trust
client-supplied forwarding headers to bypass its global limit.

The server stores only credential/session verifiers and redacts issued tokens
from mutation receipts and events. Do not enable HTTP access logs that record
headers, bodies, or query strings. Protect the database, its WAL files, artifacts,
and every backup snapshot as credentials. Database initialization uses mode 0600;
systemd uses umask 0077. The installed backup repository is local protection only.
Do not claim host-loss protection until a complete destination copy has passed
destination-side verification, and do not claim the one-hour restore target until
the documented recovery exercise has measured it end to end.
