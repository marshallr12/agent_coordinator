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
Account creation is the only mutation whose receipt is bound to the human
administrator principal instead of the individual browser session. This permits
replay after signing in again following an uncertain response. The service still
rechecks the new browser session and current administrator role under the writer
lock. Revoked, disabled, or demoted administrators cannot replay it. All other
mutation fingerprints retain their existing session binding.

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

## Account email addresses

Every human account carries an email address. `POST /api/v1/admin/operators`
requires an `email` field, and a first administrator or an account that predates
this feature may have none until its owner sets one. An address is `unverified`
until its owner opens the mailed link; `GET /api/v1/auth/account` returns the
account's `email`, `email_verified`, `email_verified_at`, an `email_verification`
object (`status` is `missing`, `unverified` or `verified`; while unverified it also
has `sent_at`, `send_error`, `link_expires_at` and `resend_available_at`), and
`mail_configured`. The migration that added the column recorded
`marshall.rosenstein@gmail.com` as verified on every administrator that already
existed.

- `POST /api/v1/auth/email` (human browser session) takes
  `{"expected_revision":N,"email":"name@example.com"}`, makes the address
  unverified, and mails a link. Submitting the address already on the account is
  `email_unchanged`.
- `POST /api/v1/auth/email/resend` (human browser session, body `{}`) mails a new
  link when the address is unverified and the previous link is at least one minute
  old; otherwise it returns `verification_resend_too_soon`,
  `email_already_verified` or `email_missing`. Issuing a link, by either
  operation or by account creation, invalidates every earlier unused link for the
  account at once.
- `GET /api/v1/auth/email/verify?token=` needs no credential and immediately
  marks the address verified, then shows the success page with a link to
  **My account**. There is no second verification button. A mail scanner that
  opens the URL also performs this verification; `HEAD` does not consume the
  link. The legacy `POST` form field `token` remains supported. Both methods
  perform the same writer-locked, single-use transition. An unknown, used,
  replaced or expired token (links last 24 hours) produces an "expired" page.

Tokens are 256 random bits, stored only as a SHA-256 hash, and appear only in the
email. The message is sent after the database transaction commits; the outcome is
recorded as `sent_at` or a short `send_error` code (`mail_not_configured`,
`send_failed`), never relay output. A failed send is retried by asking for another
link. The address is never used to sign in.

Verification and password-reset messages are MIME `multipart/alternative`
emails with UTF-8 plain-text and HTML bodies. HTML uses anchor links with the
complete token in `href`; mail readers decode the declared transfer encoding
before presenting either body. Account names and link attributes are escaped
in HTML. Password-reset links still open the new-password form and change the
password only when that form is submitted.

### Password recovery by email

The sign-in page offers **Forgot password?** beneath **Sign in**. It asks for an
email address and calls `POST /api/v1/auth/password/forgot` with `{"email":"..."}`
(no credential; the request must carry the service origin). The reply is always
`{"requested":true}` when the address is well formed, whether or not an account has
it, so the form cannot be used to discover accounts. Each enabled human account
whose **verified** address matches (compared without regard to case) receives a
random single-use token, stored only as a SHA-256 hash, and one message lists a
link for every such account. An unverified address never receives a reset link.
Requests are limited to five a minute per address, and an account gets at most one
message a minute; a newer link cancels every earlier one.

- `GET /api/v1/auth/password/reset?token=` needs no credential and shows a form
  naming the account, so mail scanners that fetch the link change nothing.
- `POST /api/v1/auth/password/reset` takes the form fields `token`, `password` and
  `confirmation` (12 to 1024 bytes, matching). A rejected password shows the form
  again and leaves the link usable. Success replaces the password, signs the account
  out of every browser session, cancels its other links, and records
  `operator_password_reset`.

A link expires one hour after it is issued, or when the password is reset, whichever
comes first, and it also stops working if a newer link is issued, the account's
address changes or becomes unverified, or the account is disabled. An unknown, used,
replaced or expired token produces an "expired" page. The message is sent after the
database transaction commits; a failed send is logged and the person asks again.

The service sends through an SMTP relay it is configured with:
`COORDINATOR_SMTP_HOST`, `COORDINATOR_SMTP_PORT` (587 for `starttls`, the default,
or 465 for `tls`), `COORDINATOR_SMTP_TLS`, `COORDINATOR_SMTP_USER`,
`COORDINATOR_SMTP_PASSWORD_FILE` (a file only its owner can access) and
`COORDINATOR_MAIL_FROM`. For Gmail use `smtp.gmail.com`, the account's address as
user and sender, and an app password in the file. Without a relay the service runs
normally and verification emails are recorded as not sent.

## Passkeys

A human account may hold up to ten passkeys (WebAuthn credentials) and sign in
with one instead of its password. The relying party id is the host of the
configured public origin, and every ceremony checks that origin. Only ES256 is
accepted, no attestation is requested, user presence and user verification are
required, and the service stores only the credential id, public key and signature
counter. A synced passkey whose counter stays zero is accepted; a counter that
fails to advance is refused.

- `POST /api/v1/auth/passkeys/options {}` (browser session) returns
  `challenge_id` and WebAuthn creation options with a random challenge valid for
  five minutes.
- `POST /api/v1/auth/passkeys {challenge_id,name?,credential}` (browser session,
  `Idempotency-Key`) verifies the browser's attestation and saves the passkey.
  `credential` is `{id, response:{clientDataJSON, attestationObject}}`, base64url.
  It records `passkey_registered`.
- `GET /api/v1/auth/passkeys` lists the caller's passkeys (`id`, `name`,
  `created_at`, `last_used_at`); `POST /api/v1/auth/passkeys/{id}/delete {}`
  removes one and records `passkey_deleted`.
- `POST /api/v1/auth/passkey-login/options {}` needs no credential, only the
  service origin, and returns `challenge_id` and request options with no
  `allowCredentials`, so the browser offers whichever passkey it holds for the
  site and the reply discloses nothing about accounts.
- `POST /api/v1/auth/passkey-login {challenge_id,credential}` verifies the signed
  challenge and answers like `POST /api/v1/auth/login`, with the session cookie
  and CSRF token, and records `browser_signed_in` with `method: passkey`. A
  challenge allows one attempt; an unknown credential, wrong signature, wrong
  origin, spent or expired challenge, or disabled account returns
  `authentication_required`.

The sign-in page shows **Sign in with a passkey**. If this browser has not
created a passkey it also offers to sign in with the password and then create
one; if it has, opening the page signed out starts the passkey prompt itself.
Removing a passkey does not end sessions it started.

## Browser sessions

`GET /api/v1/browser-sessions?cursor=...` lists the caller's sessions. An
administrator may add `principal_id` to inspect another human. Items contain only
`id`, `principal_id`, `created_at`, `expires_at`, `revoked_at`, and `current`.
Cookie tokens and their hashes are never returned.

`POST /api/v1/browser-sessions/{id}/revoke` with `{}` revokes one of the caller's
sessions or, for an administrator, another human's session. Revoking the current
session clears its cookie. Existing task, attempt, job, and resource records are
unchanged.

## Agent credential class and access

Every agent credential carries two attributes, chosen at issuance
(`POST /api/v1/admin/agents` or `POST /api/v1/admin/agents/{principal_id}/credentials`)
and kept by rotation:

- `class`: `interactive` (default; a person drives the harness), `supervised`
  (launched unattended by a host supervisor) or `integrator` (the deterministic
  integrator; it may call only the integrator API, and only it may, except
  that humans resolve integrator reports). Every
  audited event records the class of the credential behind it, so autonomy can
  be measured from the log.
- `access`: `write` (default) or `read`. A read-only credential may read any
  state its principal can see and may open, acknowledge instructions for, and
  close its own top-level agent session. A session registration that names a
  subagent, and every other change, is refused with `operation_not_permitted`,
  so a read-only credential never creates or resumes a subagent identity. Reviewer launches and shadow host principals use
  read-only credentials; their verdicts are posted by a separate write
  credential.

```json
{"name":"mxmini-reviewer","class":"supervised","access":"read"}
```

On `POST /api/v1/admin/agents`, omitting both fields issues an interactive
write credential, which is how every credential issued before these attributes
existed behaves. On `POST /api/v1/admin/agents/{principal_id}/credentials`, each
omitted field is inherited from the principal's most recent credential, revoked
ones included, so a read-only reviewer re-issued a token after a restore stays
read-only; only a principal with no prior credential falls back to interactive
write. Explicit values always win.

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

After a restore revokes every credential, an administrator can preserve an
existing agent identity while issuing fresh authentication with:

```text
POST /api/v1/admin/agents/{principal_id}/credentials
{"name":"after-restore"}
```

The principal must still be an enabled agent. The first response contains its
unchanged `principal_id` and `principal_name`, the new `credential` record, and
the one-time `token`. Omitted `class` and `access` are inherited from the
principal's most recent credential (see above). A replay returns the saved credential identity without the
token and sets `secret_unavailable: true`. The operation is available during
restore reconciliation so a recovered administrator can reconnect clients, but
the global restore pause prevents those clients from claiming or changing work
until reconciliation finishes.

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

The recovery event identifies `initiator_kind` as `host_operator` and sets
`authenticated_principal_id` to `null`, because the local command has no
authenticated browser or agent principal. `subject_principal_id` identifies the
recovered account. The existing non-null `events.actor_id` foreign key also points
to that account solely as a `subject_reference`, as recorded by
`actor_id_role`; it must not be presented as the initiator of the recovery.
