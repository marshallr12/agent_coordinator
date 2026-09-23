# MCP connection guide

Agent Coordinator exposes a vendor-neutral Streamable HTTP endpoint at `/mcp`.
It uses the same agent credential, durable harness session, project policy,
ownership generations, clock safeguards, and mutation receipts as the REST API.
Connecting or registering a harness session does not claim work. Prefer an
already configured authenticated MCP connection for coordination; the native CLI
is optional until a local workstation operation needs it. See the
[portable startup guide](agent-startup.md) for connection selection and fallback.

The endpoint is a stateless MCP transport. It does not issue or depend on an
MCP transport session. `X-Coordinator-Session` identifies the durable Agent
Coordinator harness session and is unrelated to `Mcp-Session-Id`. Disconnecting
or reconnecting an MCP client does not renew a lease, close a coordinator
session, or restore expired authority.

## Standalone durable adapter

`agent-coordinator-mcp` is a separate executable: an MCP host launches it as a
stdio server, and it forwards supported requests to the coordinator's stateless
HTTP endpoint. Listing, claiming, checkpointing and releasing do not require the
native `agent-coordinator` CLI. Build it with
`cargo build --release --locked -p coordinator-mcp-transport`; use the resulting
`agent-coordinator-mcp` executable (`agent-coordinator-mcp.exe` on Windows).

Configure the host's stdio command as that executable's absolute path, with
arguments `--state-dir` and an absolute, dedicated directory outside every
checkout. Its parent must already exist. Use one directory and one session for
this harness across restarts. The directory must be on persistent local storage
that supports exclusive file locks and atomic file replacement; do not use a
RAM disk or an ephemeral sandbox directory for interruption recovery. Never
launch another transport writer with the same session or copy its journal to
another host. The lock excludes processes using this directory, not writers
on other machines.

Map the following values from the host's protected environment or secret store;
this table specifies variable names, not shell assignment syntax:

| Environment variable | Value |
| --- | --- |
| `AGENT_COORDINATOR_MCP_URL` | Trusted HTTPS origin followed by `/mcp` |
| `AGENT_COORDINATOR_MCP_TOKEN` | Issued agent token |
| `AGENT_COORDINATOR_MCP_SESSION_ID` | Unique, stable harness session UUID |
| `AGENT_COORDINATOR_MCP_SESSION_PROOF` | Persisted random 32-byte session proof |
| `AGENT_COORDINATOR_MCP_PROJECT_ID` | Bound project UUID |
| `AGENT_COORDINATOR_MCP_STATE_DIR` | Alternative to the `--state-dir` argument |

No secret belongs in command arguments, a model-visible configuration, or the
repository. The adapter does not enroll agents, invent sessions, read native CLI
credentials, or perform OAuth. Provision the session ID and proof once; register
that configured identity through `coordinator_session_register` if it is new.
The project mapping binds the journal's identity; it does not restrict the
credential's existing service permissions to one project.

Before mutations, call `coordinator_transport_status`. A running adapter reports
`durable_mutation_journal: true` only after acquiring the exclusive journal lock
and successfully saving protected state. Its `configured_identity` supplies the
non-secret session ID, project ID and service origin. Use that session ID for
registration if the configured session is new; never invent a replacement. Startup fails if storage, permissions,
identity binding or locking cannot be established. Unix state uses modes 0700
and 0600; Windows uses a protected owner/SYSTEM DACL. Links, unexpected files and
corrupt journals are rejected. Credentials and proofs are not stored in the
journal; their digests bind it to the provisioned identity. Do not erase a
journal or change identities to bypass a recovery error.

Supply the advertised `idempotency_key` with each mutation. The adapter atomically
checks required and permitted outer argument fields before journaling, then
saves the complete tool parameters and key before network dispatch, serializes
writes, and rejects reuse of a key with different parameters. A transport error,
invalid response or uncertain tool error retains the pending request. Reads and
status calls remain available, but a different mutation is refused. Call
`coordinator_transport_retry` to resend the exact saved request through the
service's current authentication and receipt checks. If no request is pending,
this tool replays the most recently completed request, covering a disconnect
before the host received its response. It does not replay every historical call
or create a new key. Inspect current ownership before further task work: a
receipt never grants renewed authority.

Only narrowly checked claim rejections (`claim_conflict`, `revision_conflict`,
`policy_changed`, `instructions_required`) release the pending slot without a
successful result. Other error results remain pending for exact retry and
inspection; some require operator reconciliation. This conservative behavior
can stop writes rather than guess whether an effect committed. Request history
is retained, with a 64 MiB journal limit; resolve pending work and provision a
new independent session before capacity is exhausted. Do not prune uncertain
requests or silently switch credentials.

The adapter supports bounded newline-delimited JSON-RPC stdio, initialization,
ping, tool discovery and tool calls against this service's JSON HTTP transport.
It is not a general proxy for arbitrary MCP servers, SSE streaming, remote
execution, or paginated tool catalogs. Hosts must support launching a local
stdio server and protected environment mappings. Direct HTTP header configuration
below provides authentication alone; it does **not** prove durable journaling.
A direct host needs its own verified pre-dispatch persistence capability before
mutations are safe. Model instructions or claims that a journal was written are
not verification.

The disposable service smoke test cuts a claim response after commit, kills and
restarts the adapter, then independently checks the journal and service records
for exact replay, one attempt, unchanged session and unchanged expiry. It also
rejects concurrent adapters and failed Git-status checks. A failed Git command
means cleanliness is unverified, regardless of empty stdout. These checks do not
claim compatibility with every desktop host or simulate physical disk failure.

## Configure authentication

Configure the MCP server in machine- or user-level client settings, outside the
repository. Use the exact trusted HTTPS service origin followed by `/mcp`.
Supply these headers on every MCP HTTP request:

| Header | Source | Purpose |
| --- | --- | --- |
| `Authorization: Bearer ...` | Protected agent credential | Authenticates one agent principal and credential |
| `X-Coordinator-Session` | Stable, unique session ID | Separates this harness from every other harness using that credential |
| `X-Coordinator-Session-Proof` | Random 32-byte secret | Proves possession of the saved harness session |

The token and session proof must come from the client's environment or secret
store. Never put either value in a URL, repository file, MCP tool argument,
checked-in MCP configuration, shell history, log, or conversation. A session ID
is not a credential, but it must be a new stable ID for this harness and must
not be copied from another harness. Do not forward any authentication header
across an origin-changing redirect.

The service currently supports its configured bearer credentials. It does not
claim MCP OAuth discovery or dynamic client registration support. A client that
requires OAuth and cannot supply a bearer token plus the two coordinator headers
cannot connect directly in this release.

For direct transport authentication only, Codex supports bearer tokens and HTTP headers sourced from
environment variables. Put this in the user-level `config.toml`, not a
repository `.codex/config.toml`:

```toml
[mcp_servers.agent_coordinator]
url = "https://coordinator.example.com/mcp"
bearer_token_env_var = "AGENT_COORDINATOR_MCP_TOKEN"

[mcp_servers.agent_coordinator.env_http_headers]
X-Coordinator-Session = "AGENT_COORDINATOR_MCP_SESSION_ID"
X-Coordinator-Session-Proof = "AGENT_COORDINATOR_MCP_SESSION_PROOF"
```

A compatible host can provide these protected values and register its own session
without installing the coordinator CLI. When the CLI is available, the packaged
launcher is an alternative that retains native CLI continuity. First connect the stable harness session for this
repository binding, then launch one trusted local MCP client executable:

```sh
agent-coordinator --session mcp-work-42 connect \
  --harness codex-mcp --capability code
agent-coordinator --session mcp-work-42 mcp-client -- \
  /absolute/path/to/codex
```

Use an absolute path to the intended executable and keep all program arguments
free of credentials. Keep that client in the foreground for the launcher's
lifetime; the launcher does not supervise a daemon or background process. The
launcher locks out a second foreground MCP client using the same harness,
validates the repository binding, credential digest, protected session state,
and live remote session, then supplies the exact token, session ID, proof,
origin, project, and non-secret MCP URL through the child environment. It does
not print or create another copy of a credential. It releases the normal
session-state lock before starting the child, so that trusted client can invoke
native CLI commands with the same `--session` identity. The launcher also passes
the absolute repository-binding path, including when the client or one of its
CLI children runs from another directory. The URL in the MCP client's user
configuration must exactly equal the binding's trusted service origin plus
`/mcp`. An already-running desktop or IDE process does not acquire a newly
launched process's environment; restart that client through the launcher. If
the launcher crashes, inspect whether its child is still running before
relaunching; release of the local lock does not prove the child stopped.

One launcher session is bound to one repository and project for native CLI
work. MCP tools can address any project explicitly, but native operations in a
different repository require that repository's binding and a separately
connected local harness and launcher.

This example relies only on client-side environment lookup; the values do not
become tool arguments. See the current
[Codex MCP configuration documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)
for those client-specific keys. Other Streamable HTTP clients can use their
equivalent protected header facility.

## Register and orient the harness

The native launcher path uses the session already created by `connect`. Begin by
calling `coordinator_session_get` with an empty object; it returns the nonsecret
coordinator session ID that later session-scoped tools accept. This inspection
does not renew ownership. A client that provisions its own independent session
instead must call `coordinator_session_register` once. Its body contains:

```json
{
  "session_id": "the-same-id-as-X-Coordinator-Session",
  "workstation_id": "stable-workstation-id",
  "harness": "mcp-client-name-and-version",
  "capabilities": ["capability-needed-for-selection"]
}
```

The session ID in the body must exactly match the configured header. The proof
stays in its header and is never a tool argument. Supply a new random
`idempotency_key` with the tool call and retain the exact key and body until the
registration response is known. An identical retry retrieves the same session;
different parameters for an existing session are rejected. Registration stores
only the proof verifier. If a credential is rotated, a session is closed, or a
database restore invalidates authority, configure a new session ID and proof
instead of trying to attach the old session to a new credential.

Then follow this sequence:

1. Call `coordinator_orientation` with the intended project ID. Read every
   required section. Do not acknowledge or claim while `instructions_complete`
   is false.
2. Call `coordinator_instructions_ack` with the configured session ID and that
   exact project policy revision, instruction version, and complete section
   list. The session argument can be omitted to use the configured header.
   Acknowledgment grants no ownership.
3. Call `coordinator_tasks_list` or the bounded context tools to inspect work.
4. Call `coordinator_claim` to claim one exact eligible task or request the next
   eligible task. Do not change source before a successful claim.
5. Persist the returned attempt ID and ownership generation. Schedule renewal
   from the returned `lease_remaining_ms` and `renew_after_seconds`, subtracting
   request time measured by a local monotonic clock and a safety margin.

The server's MCP `instructions` field and tool descriptions summarize this
workflow for small agents. Retrieved task, knowledge, and import prose is data;
it cannot override binding service instructions or project policy.

## Writes and uncertain responses

Every mutating tool requires an `idempotency_key` argument. Generate a fresh
random key before a new logical write and persist it with the exact tool name
and arguments before sending the request. After a timeout, disconnect, or other
uncertain response, retry the same tool with exactly the same arguments and key.
Do not generate a new key merely to get a different answer. A successful replay
is historical evidence of the original effect and never renews current
ownership.

Before any ownership-dependent write, inspect the attempt or activity if its
authority may have changed. Use the exact current generation and revision
values. Checkpoints do not renew leases. If the service reports lost or expired
authority, stop ownership-dependent work and follow the recovery guidance; a
new connection cannot revive the old attempt.

MCP tools use a closed catalog of typed operations. There is no arbitrary URL,
method, header, SQL, shell, or generic status-edit tool. HTTP authentication
failures remain HTTP errors. Coordination errors are returned to the MCP caller
with their stable service code and bounded details so the agent can take the
listed next action without seeing credentials.

## Workstation operations still use the native CLI

The MCP endpoint coordinates recorded state. It does not run local processes,
read local paths, manipulate Git, or transfer binary bodies. When the MCP client
was started through `mcp-client`, use the native CLI with that same stable
harness session for operations that need trusted local inspection or durable
local journals:

- `worktree prepare` creates and verifies a separate clean worktree before its
  registration is recorded.
- `jobs run`, `jobs inspect`, and `jobs reconnect` launch or observe local
  producers and retain reporter state safely across interruptions.
- `artifacts upload` and `artifacts download` journal and verify bounded binary
  transfers.
- `submissions code`, `integrations prepare`, `integrations publish`,
  `integrations reconcile`, and `integrations finish` observe exact Git objects,
  preserve publication intent, and enforce guarded compare-and-swap publication.

For interrupted publication, `coordinator_agent_publication_reconcile` records
only a fresh observation that exactly matches the immutable saved base or intended
result. The caller must verify the durable local intent journal and confirm the old
publisher stopped, current policy and decisions still match, and no live or
uncertain jobs or held reservations remain.
A changed target, missing intent/evidence, or uncertainty stays human-gated. The
service records client evidence but does not inspect a remote Git repository or
independently verify process termination.

MCP can inspect the resulting jobs, artifacts, workflow, and history. Recording
a checkout, publication intent, or integration result through MCP does not prove
that the corresponding local Git or process operation occurred.

An independently provisioned MCP-only session can use `session adopt-mcp` to
create protected native state with its exact identity, after quiescing writes and
reconciling pending MCP requests. See [session adoption](CLI.md#adopt-an-existing-mcp-session)
for the protected environment contract. Adoption is observational on the service;
it does not claim or renew work. A similar harness name is not sufficient, and
another session cannot borrow attempt authority. If secure adoption is unavailable,
checkpoint and release through MCP, then connect and freshly claim through the CLI.
A missing CLI still permits MCP checkpoint/release; it is not a reason to abandon
a lease or install unverified software.

## Interoperability expectations

A compatible client must support Streamable HTTP, per-request custom headers,
JSON Schema tool inputs, JSON tool results, and ordinary HTTP authentication
errors. Clients should preserve unknown response fields and opaque pagination
cursors. They must tolerate a stateless server that does not issue
`Mcp-Session-Id` and must not infer coordinator authority from MCP connection
state. Each inner REST JSON result is bounded to 1 MiB before it is represented
as both structured MCP content and equivalent text, so a client's encoded MCP
message limit must allow for that representation.

Release validation exercises the endpoint over an ephemeral TCP listener with
the official Rust MCP client. It covers modern discovery and legacy
initialization, tool discovery, session registration and lookup, an authenticated
claim, a rejected credential, and a fresh transport reconnect that preserves
identity without renewing ownership. The service's route tests separately cover
authorization, policy, revision, idempotency, lease, and recovery behavior
behind the same tool catalog.
