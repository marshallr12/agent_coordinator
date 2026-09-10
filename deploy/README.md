# Foundation deployment examples

These are installation examples for the first foundation slice, not evidence of
a production deployment or completion of the planned release. Use one service
process and a local filesystem for SQLite. Caddy terminates HTTPS; the service
only accepts loopback listener addresses and ignores forwarded headers.

1. Build `agent-coordinator-server` from the reviewed source and install it as
   `/usr/local/bin/agent-coordinator-server`. Assets and migrations are embedded.
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
headers, bodies, or query strings. Protect the database and its WAL files as
credentials. Database initialization uses mode 0600; systemd uses umask 0077.
Restore invalidation and verified off-server backup procedures are not implemented
in this foundation slice; do not represent these examples as a recovery plan.
