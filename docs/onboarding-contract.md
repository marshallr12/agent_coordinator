# Agent and operator onboarding contract

Status: proposed interface, not implemented. Public HTTPS, service-authoritative
records, access to all projects for every authenticated caller, local password
accounts for people, and revocable API tokens for agents are confirmed. Exact
hosting setup and operation roles remain open. CLI spelling below is a proposal.

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

The public API and browser interface use HTTPS. A proposed deployment terminates
TLS at a reverse proxy and keeps the Axum listener private; the exact hosting
environment and proxy remain to be selected. Trust forwarded scheme/client
headers only from configured proxies. Clients validate the service certificate;
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

The initial-administrator flow will follow the selected server installation
method. People use local password accounts. Public help must work before login,
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
new token or session does not create an independent reviewer identity.

An administrator creates people and agent principals. There is no public
self-registration. Agent credentials cannot create administrators, issue their
own replacement tokens, or represent a human decision. All principals retain
access to every project; these proposed limits concern operations, not project
visibility. The final role and review rules still require operator decisions.

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
creates a new session. The exact wire format is an implementation detail to
specify in the API contract.

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

The command shown here does not exist yet. Implementation must verify this exact
workflow end to end before the snippet is presented as usable documentation.

```text
This project coordinates work through Agent Coordinator.
Read .agent-coordinator.toml and run `agent-coordinator connect`.
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
