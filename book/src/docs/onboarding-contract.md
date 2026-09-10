# Agent and operator onboarding contract

Status: mixed implementation and release design. Authentication, native session
connection, task coordination, the repository bootstrap, service-authoritative
records, project access, local human accounts, and revocable agent tokens are
implemented. Linux systemd packaging, trusted-HTTPS installation, backups, and
restore controls have passed the disposable release exercises. An actual
production host, domain, and off-server backup destination remain operator
installation choices. Automated CI validates Linux and native Windows packages;
the final operator-initiated physical Windows workstation exercise remains
pending acceptance evidence. Operation roles are specified in
[implementation-spec.md](implementation-spec.md). Use [the CLI guide](CLI.md) for
implemented commands.

## Repository binding and workstation credentials

Keep a small, non-secret `.agent-coordinator.toml` in each repository. It identifies
the service URL and stable project ID. It must not contain an API credential,
an operator password, or a session secret. Separate worktrees inherit the same
project binding; their paths and branches identify their distinct checkouts.

Credentials live in workstation configuration outside the repository, or are
supplied through the process environment. The implementation must choose and
document the supported secure storage mechanism for each selected client OS.
The agent receives credential references and setup status, not secret values in
its orientation text. Direct HTTP clients can use locally configured credentials
without depending on the CLI or an agent-vendor integration.

A credential is bound locally to its trusted service origin. Editing a repository
binding cannot redirect an existing credential to another host, and the HTTP
client must not forward credentials across an origin-changing redirect. Project
bindings help route requests; the service still performs authorization itself.

## Public HTTPS deployment

The public API and browser interface use HTTPS. The selected native Linux
deployment runs Axum under systemd. Terminate TLS at a reverse proxy and keep the
Axum listener private. Use Caddy for the documented baseline; an existing proxy
can use the same HTTP upstream contract. Trust forwarded scheme/client headers
only from configured proxies. Clients validate the service certificate;
the documented normal workflow must not require disabling certificate checks.

Credentials travel in authentication headers or the login request body, never
URLs. The service redacts credentials from logs, limits request sizes and request
rates, and validates allowed operations on the server. These choices follow
[OWASP's REST security guidance](https://cheatsheetseries.owasp.org/cheatsheets/REST_Security_Cheat_Sheet.html).

For the selected local password accounts, use salted Argon2id password hashes
with an implementation-time work-factor review, following
[OWASP's password-storage guidance](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html).
For browser sessions, the proposal is server-managed sessions using Secure,
HttpOnly, explicitly SameSite cookies with CSRF protection and server-side
revocation. Do not store long-lived agent tokens in browser local storage. See
[OWASP's session guidance](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html).

Task and lesson content renders as text or sanitized Markdown, with raw active
HTML disabled. Public setup help provides orientation, not remote execution.
Service-internal management actions require the selected administrative role,
even though all authenticated callers can access all projects.

### Native Linux installation proposal

Ship a versioned release containing the Rust server, CLI, database migrations,
embedded browser assets, a systemd unit, and example proxy configuration. The
installed service should not require a source checkout or frontend build tools.
Run it under a dedicated unprivileged service account; keep configuration under
`/etc/agent-coordinator` and writable data under `/var/lib/agent-coordinator`.
Use a local filesystem for SQLite. One active service process owns coordination;
this first-release design does not include active/active server replication.

Use Caddy as the documented default proxy, while retaining a standard HTTP
upstream for an existing alternative. Caddy can obtain and renew certificates
for an appropriately configured public hostname; DNS, network reachability, and
persistent certificate storage are deployment prerequisites. See the
[official automatic HTTPS documentation](https://caddyserver.com/docs/automatic-https).
The final hostname and production Linux distribution are installation inputs,
not project IDs or assumptions embedded in application code. The test baseline
is Ubuntu 24.04 LTS on x86_64 with 2 CPU cores and 4 GB RAM.

Provide a database-aware backup command suitable for a systemd timer, using
[SQLite's supported online snapshot facilities](https://sqlite.org/backup.html). Do not document
copying only a live database file while ignoring its WAL. The operator selected
hourly backups, 24 hourly and 30 daily retained copies, documented off-server
copying, and a one-hour restore target.
The selected log/report uploads require the backup manifest to cover their storage
and referenced digests as well as database records.

An upgrade checks schema compatibility, creates a verified backup, applies
migrations under exclusive maintenance access, and verifies readiness before
accepting work. Running jobs remain on workstations; service maintenance cannot
declare them stopped. Document the expected coordination outage and lease
recovery behavior. Do not automatically roll back the binary across an
incompatible database migration.

Restoring an older backup is an explicit maintenance operation. It must invalidate
pre-restore sessions and attempt authority, preserve uncertain-job resource holds,
and require credential/account reconciliation before public access resumes.
Restoring old authentication tables must not silently reactivate a subsequently
revoked credential or account. Use the restore procedure below, including a new
authority epoch and host-local account/credential recovery.

### Backup and restore procedure

The systemd backup timer starts an hourly snapshot job under a single-job lock.
The command creates a consistent SQLite backup and an artifact manifest listing
the finalized files referenced by that snapshot. Preserve referenced immutable
artifact bytes while the snapshot is being assembled: artifact deletion/garbage
collection must respect the active backup hold. Do not keep a database writer
transaction open while copying files.

Store a complete immutable artifact copy within each snapshot directory. The
first release favors independently verifiable and transferable bundles; shared
blob deduplication is deferred and storage sizing must include full retained copies.
Each snapshot has its own database image,
manifest, schema/service versions, timestamps, digests, and completion marker.
Only publish the completion marker after database integrity and all referenced
file digests are verified. A partial snapshot is never counted as a usable backup.
Retain the newest snapshot from each of the most recent 24 hourly buckets plus one successful snapshot for each
of the most recent 30 days. Prune only complete snapshot directories outside the retention set. Insufficient space or a failed backup leaves prior usable
snapshots intact and raises a visible operator alert.

Document copying the completed backup repository to an operator-selected server
or storage destination, with authentication/encryption supplied by that transfer
mechanism. Include manifests, database images, and referenced blobs. Copying only
the manifest is insufficient. Track local snapshot time separately from the
operator's verified off-server copy time. Hourly local backups do not establish
one-hour data-loss protection against host loss unless off-server copying also
meets that schedule. The destination and its credentials are installation inputs.

The restore procedure is:

1. Stop the service and keep public access in maintenance mode. Preserve the
   current damaged state separately; select a completed compatible snapshot.
2. Verify its database and artifact digests, then restore into a staging data
   directory with the service account's permissions. Reject incomplete snapshots.
3. Generate a fresh authority epoch. In the restored database, revoke all agent
   tokens/reporters/browser sessions, invalidate agent sessions and attempts,
   and suspend restored human accounts pending reconciliation. Treat known
   in-flight jobs/resources as uncertain, retaining their recovery holds.
4. A host-local recovery command establishes a fresh administrator credential.
   That administrator explicitly reconciles people and issues fresh agent
   credentials. Never re-enable a restored password/token just because its
   historical record predates a revocation.
5. Check schema/readiness, atomically promote the staged data directory while
   the service is stopped, start the service, and verify new authentication and
   old-credential rejection before restoring public access.
6. Agents reconnect with fresh credentials, inspect saved work and jobs, and use
   the normal recovery workflow. Reconcile actual Git targets before admitting
   conflicting integration work; a restored database can lag external effects.

Measure the one-hour restore target from starting this documented procedure on
an available compatible host with access to a completed backup and the operator's
host credentials, through restored service availability and one recovered client.
Include data verification/copying and credential recovery in the exercise. New
server procurement and recovery of unavailable off-server storage are external
dependencies; record them explicitly in a real incident. Restoring task data
does not promise that all formerly running workstation jobs finish within an hour.

Ordinary service restarts do not rotate the restore epoch or reset credentials.
They preserve stored lease deadlines; elapsed downtime can cause normal expiry.

## First connection without authentication

The service returns a consistent authentication-required response, including
when an unauthenticated caller names an unknown project. It supplies:

- A stable error code and supported protocol version.
- A short human-readable explanation that authentication is missing or invalid.
- Public setup instructions and a same-service help location.
- The configuration fields needed, using placeholders rather than credentials.
- An explicit next action: show the operator the instructions, then reconnect
  after local credential configuration has been completed.

Example human message:

> This workstation is not authenticated with Agent Coordinator. Open this
> service's setup page, sign in with an operator account, and register the
> workstation. Configure the issued credential in the local client, then retry
> the connection. Keep the credential out of this conversation and repository.

Initial administrator setup uses the host-local installation command. People use
local password accounts. Public help must work before login,
but cannot include project names, users, tasks, permission grants, or credential
values.
Authentication failure is not an instruction to continue selecting work offline.

## Operator setup flow

Proposed normal sequence:

1. Set up the service and its first administrator using the selected installation
method. Initial administrator creation must not be an unauthenticated public
   operation available after setup.
2. Create a project and assign its stable ID and repository binding.
3. Configure project workflow and completion requirements.
4. Register a workstation/agent credential. It can access all projects; its
   operation role determines any administrative privileges separately.
5. Configure that credential locally using the supported client mechanism.
6. Generate the non-secret repository binding and short orientation snippet.
7. Verify connection and project visibility before importing or claiming work.

Credential rotation and revocation must be available without editing committed
repository instructions. Failure to connect, insufficient permission for an
operation, and a missing/mismatched project binding have distinct remedies.

## Proposed identity and credential lifecycle

Keep a principal's identity separate from its credentials. Revoking a token must
preserve attribution on tasks, reviews, and lessons. Multiple tokens may belong
to one agent principal, with workstation labels and individual revocation; a
new token alone does not establish reviewer independence. A review records its
actual principal and session separately from the credential used to authenticate.

An administrator creates people and agent principals. There is no public
self-registration. Agent credentials cannot create administrators, issue their
own replacement tokens, or represent a human decision. All principals retain
access to every project; these proposed limits concern operations, not project
visibility. Projects may separately delegate binding-rule changes to agents;
that delegation does not confer credential-administration privileges.

Generate agent tokens with cryptographic randomness, display them only when
issued, and persist a verifier rather than the original token. Keep a non-secret
credential ID, issuer, created/last-used timestamps, optional expiry, and
revocation timestamp for management. Never return the verifier through the API.
Rotation creates a replacement credential; the operator may then revoke the old
one. Revocation takes effect on subsequent authenticated operations, including
retries of previously successful requests.

Distinguish the agent credential, the harness session, and the task attempt.
Two sessions using one workstation credential must not accidentally share task
authority. A session needs a locally protected resume credential, and
ownership-dependent requests must authenticate that session as well as its
principal. Compaction/resume reuses session identity; starting another harness
creates a new session. The wire format is specified in
[api-contract.md](api-contract.md).

Credential revocation invalidates dependent session authority. It does not prove
that a local job stopped or release an uncertain shared resource. The task enters
the selected recovery process, retaining its checkpoints and job observations.
A replacement credential can discover recoverable work, but cannot silently
revive an expired attempt.

Initial-admin creation and lost-password recovery use a documented command on
the service host in the first-release proposal. This avoids requiring email
delivery as infrastructure. The browser supports password changes, session
revocation, and token issuance/revocation for authorized people. Password changes
invalidate existing browser sessions; agent-token revocation is a separate,
explicit operation.

## Linux and native Windows client contract

Distribute native client binaries. Windows users must not need WSL, Bash, or a
Unix process supervisor to connect, claim work, or report a job. Git operations
use the locally installed Git executable and existing authentication. The
coordinator never needs the workstation's Git private keys on its server.

Give every substantive CLI operation a JSON output mode and a way to read
structured input from a file or standard input. Document both PowerShell and
Linux-shell examples without making correctness depend on complex shell quoting.
Failures have stable exit codes and structured remedies; never turn a failed
child command into a successful job report.

Keep credential and session files outside repositories. Use the selected OS
credential facility or explicitly protected local storage, including permissions
on Linux and ACLs on Windows. Headless Linux installations must have a supported
noninteractive mechanism. Do not require desktop keyring prompts during a task.

Identify a checkout using workstation identity and resolved Git/worktree metadata.
Paths are displayed in the workstation's native form, including Windows drive
letters and spaces. Path strings alone cannot prove two checkouts are separate.
Keep process instance identity distinct from its numeric PID so PID reuse cannot
make a new process appear to be an old running job.

The local reporter observes explicitly registered local jobs and reconnects to
their durable records. It does not accept remote shell-execution requests. A
platform-specific observer that cannot determine process state reports unknown,
retains its last evidence, and requests reconciliation rather than inventing a
terminal status.

## Authenticated orientation

A successful connection returns a bounded orientation packet with:

| Field group | Purpose |
| --- | --- |
| Protocol and instruction versions | Let clients detect incompatibility and fetch changed workflow instructions |
| Project identity and policy revision | Confirm the intended project and applicable configuration |
| Session/attempt recovery | Reconnect to existing ownership and known jobs before asking for new work |
| Server time and ownership status | Report authoritative lease state; the client derives a conservative local deadline |
| Pending decisions and blockers | Explain what requires attention and what can proceed independently |
| Current work and candidate tasks | Provide a small useful set with eligibility reasons; listing reserves nothing |
| Applicable policy and relevant knowledge | Separate current rules from lessons, observations, and historical narratives |
| Next actions | Describe available operations, required fields, and expected success/conflict responses |

Do not dump full archives or every project's memory into the response. Optional
history can be summarized or paginated with provenance. Required policy cannot
be silently truncated: if more required instructions must be read, mark the
orientation incomplete and supply continuation steps before a new claim.

Repeated connect/resume/compaction must not create duplicate attempts. Connection
does not itself select work or authorize publishing, deployment, or data changes.
An authenticated user lacking permission for an administrative or ownership-
dependent operation receives a permission remedy, not a fresh enrollment loop.
Every authenticated user can select and access any project.

## Proposed short AGENTS.md / CLAUDE.md snippet

After installing the native CLI and configuring the repository binding and an
origin-bound workstation credential, use this snippet in AGENTS.md or CLAUDE.md.
Replace UNIQUE_HARNESS_NAME with a name unique to this harness on this workstation;
keep it stable when resuming and pass it on every command.

```text
This project coordinates work through Agent Coordinator.
Read .agent-coordinator.toml. Select a unique, stable name for this harness.
Run `agent-coordinator --session UNIQUE_HARNESS_NAME connect`; use that
same --session value on every following command. Connection reserves no task.
Follow the returned workflow alongside this repository's applicable rules.
Claim a task before changing code; use its separate worktree.
Report progress and renew ownership as instructed; submit results and lessons.
If authentication is required, show the setup message to the human operator.
If ownership expires, stop changing the task and follow the recovery steps.
```

The generated service help must also provide complete HTTP examples for agents
without the CLI. Examples cover connect, claim, checkpoint, renew, observe a job,
submit, release, retrieve lessons, and resolve a conflict. They use explicit field
names and long CLI flags, require no vendor-specific tools, and interpolate
credentials locally without displaying their values.

## Acceptance scenarios

- Starting with only the repository snippet and a preconfigured credential, an
  agent can connect, claim, checkpoint, submit, and retrieve relevant lessons.
- An unconfigured workstation receives setup help without private project data.
- A configured workstation can select any project. A refused administrative or
  ownership-dependent operation gets a permission-specific remedy.
- Revoking or rotating a credential does not require a repository change.
- Moving a checkout to another path does not lose its project or shared memory.
- A changed service URL or redirect cannot receive another origin's credential.
- A repeated connection restores the current attempt instead of claiming again.
- A short context response retains required policy or explicitly requires the
  agent to fetch the remaining instructions before taking new work.
- The same workflow succeeds through documented HTTP calls without a CLI, hook,
  MCP server, or agent-vendor account.
- Public deployment rejects insecure credential transport; untrusted forwarded
  headers cannot impersonate the trusted HTTPS proxy or bypass request limits.
- Browser content cannot execute stored task/lesson HTML, and cookie-authenticated
  mutations require the configured CSRF protection.
