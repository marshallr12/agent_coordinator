# Operator access contract

Operator access keeps a stable principal identity separate from passwords,
browser sessions, and agent tokens. Human accounts have role `admin` or
`operator`; agent principals retain role `agent`. Every authenticated principal
can see all projects, while only a currently enabled human administrator can
manage accounts and agent credentials. There is no public enrollment or password
recovery route.

All HTTP responses use the standard `data`, `request_id`, and `server_time`
envelope. Mutations require `Idempotency-Key`; browser mutations also require the
configured origin and CSRF token. Account and credential events contain no
passwords, password hashes, API tokens, token verifiers, cookies, or CSRF values.

## Human accounts and passwords

`GET /api/v1/auth/account` returns the current human's own account as
`data.operator`:

```json
{
  "id": "principal-id",
  "name": "operator-name",
  "kind": "human",
  "role": "operator",
  "enabled": true,
  "revision": 2,
  "created_at": "2026-09-10T00:00:00.000Z",
  "disabled_at": null
}
```

`GET /api/v1/admin/operators?cursor=...` lists at most 200 human accounts with a
bounded `next_cursor`. `GET /api/v1/admin/operators/{id}` returns one account.
Both require an administrator.

`POST /api/v1/admin/operators` creates a human account from:

```json
{"name":"reviewer","role":"operator","password":"a private initial password"}
```

The password must contain 12 through 1024 bytes. It is hashed with uniquely
salted Argon2id outside the database writer transaction. The transaction then
rechecks the administrator and creates the principal, receipt, and event
atomically. The response contains `data.operator` and never echoes the password.
A retry must re-enter the original password. The service checks it against the
resulting account's slow password hash and returns `idempotency_secret_mismatch`
for a different password. If that account has since changed its password, the
original creation request can no longer be secret-verified; inspect the account
instead. The receipt retains neither the password nor another password-checking
oracle. A successful replay returns freshly read account metadata, so the old
receipt cannot represent later role or enabled-state changes as current.

`POST /api/v1/admin/operators/{id}/access` accepts:

```json
{"expected_revision":2,"role":"admin","enabled":true}
```

The writer-locked transaction rechecks the administrator, target revision, and
last-active-administrator invariant. Changing access revokes all target browser
sessions. Two concurrent changes cannot both remove the final administrators.
Stale input returns `revision_conflict`; an attempt to remove the sole enabled
administrator returns `last_active_admin`.

`POST /api/v1/auth/password` is available only to a human browser session and
accepts:

```json
{
  "current_password": "the current private password",
  "new_password": "a different private password",
  "expected_revision": 2
}
```

Current-password verification and new-password hashing run outside the writer
transaction. Under the writer lock the service rechecks the live browser session,
unchanged old hash, and account revision, then replaces the hash and revokes all
of the account's browser sessions, including the caller. The response clears the
browser cookie. If the response is lost, sign in with the new password and inspect
`GET /api/v1/auth/account`; a revoked old session cannot use a receipt as renewed
authority.

## Browser sessions

`GET /api/v1/browser-sessions?cursor=...` lists the caller's sessions. An
administrator may add `principal_id` to inspect another human. Items contain only
`id`, `principal_id`, `created_at`, `expires_at`, `revoked_at`, and `current`.
Cookie tokens and their hashes are never returned.

`POST /api/v1/browser-sessions/{id}/revoke` with `{}` revokes one of the caller's
sessions or, for an administrator, another human's session. Revoking the current
session clears its cookie. Existing task, attempt, job, and resource records are
unchanged.

## Agent token rotation

Existing `POST /api/v1/admin/agents` enrollment creates a stable agent principal
with an initial named credential. `POST
/api/v1/admin/credentials/{old_credential_id}/rotate` accepts:

```json
{"name":"linux-builder-2026-09","revoke_old":true}
```

`revoke_old` defaults to `true`. Rotation requires an active credential belonging
to an enabled agent principal. It creates a separately named credential for that
same principal and, by default, revokes the old credential and closes its agent
sessions. It never transfers or revives an attempt and never resolves or releases
a job or resource hold.

The first response contains `principal_id`, `principal_name`, `credential`,
`replaced_credential_id`, `replaced_credential_revoked`, and the new `token`.
Only that response exposes the token. A retry returns the same credential identity
without `token`, adds `secret_unavailable: true`, and directs the administrator to
rotate the newly created replacement credential with a new idempotency key. The
default rotation then revokes that credential while issuing another token.

## Host-local recovery

Lost-password recovery is exported for the server's local command and has no HTTP
route:

```text
recover_operator_password(state, username, new_password, reason)
```

The command validates and hashes the new password before acquiring SQLite's
writer lock. Its single transaction enables the named existing human principal,
increments its revision, revokes all browser sessions, and records a bounded
reason. The principal's ID and role remain unchanged. The transaction does not
modify agent credentials or sessions, attempts, jobs, submissions, or resource
holds. The old password and all prior browser sessions remain invalid.
