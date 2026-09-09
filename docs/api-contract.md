# Proposed HTTP and CLI contract

Status: design, not an implemented API. Names and examples below are the proposed
first-release interface. Implementation will publish matching OpenAPI schemas,
CLI help, and service-delivered agent instructions from shared contract types.

## Conventions

Use `/api/v1` for the JSON API over HTTPS. IDs are opaque strings. Project IDs
are explicit in project operations; there is no global current project. Dates
are UTC RFC 3339 values; revision and ownership-generation fields are integers.
Pagination uses opaque cursors with a proposed default of 50 and maximum of 200
items. Responses include `request_id` and `server_time`.

Authenticate agents with `Authorization: Bearer <agent-token>`. Browser sessions
use the cookie/CSRF contract in [onboarding-contract.md](onboarding-contract.md).
Never put authentication or session secrets in query strings. Resolve
authentication before disclosing whether a requested project/record exists.

Every retryable mutation requires an `Idempotency-Key` generated and persisted
before the first request. The key is scoped to principal, method, and operation
path. Its stored request fingerprint includes relevant input, expected revisions,
session identity, and a verifier for any submitted secret, never raw secrets.
Reusing the key with different input is a conflict. Receipt access still checks
current authentication and authorization.

Propose retaining full mutation receipts for at least 30 days, plus compact
principal/operation/key/fingerprint tombstones after that. A retry after receipt
expiry returns `idempotency_receipt_expired` and reconciliation instructions;
it cannot execute the old operation as new. A principal's tombstones can be
removed when the principal is permanently retired and all credentials revoked.

Use these HTTP/error families:

| HTTP status | Representative code | Client action |
| --- | --- | --- |
| 400 | `invalid_request` | Correct the indicated field |
| 401 | `authentication_required` | Show public setup help or reconnect with configured credentials |
| 403 | `operation_not_permitted` | Show the missing operation permission; do not re-enroll an already authenticated client |
| 404 | `record_not_found` | Reconcile the authenticated project's binding/reference |
| 409 | `claim_conflict`, `revision_conflict`, `lease_expired`, `policy_changed` | Follow the supplied current-state and next-action links |
| 413 | `payload_too_large` | Reduce/split the report or use an external artifact link |
| 422 | `requirements_unsatisfied` | Address explicit blockers or validation details |
| 429 | `rate_limited` | Honor `Retry-After`; a delayed retry never extends an expired lease |
| 503 | `temporarily_unavailable` | Back off, retain the original mutation key, and observe the existing lease deadline |

An error body contains `error.code`, a concise `error.message`, structured
`error.details`, `error.next_actions`, and `retryable`. Never recommend blind
retries for expired authority, denied permission, or missing verification.

## Public help, credentials, and sessions

`GET /api/v1/info` returns product/API/instruction versions and public setup help.
`GET /api/v1/help/authentication` returns human-readable enrollment steps and
non-secret configuration examples. Neither returns private project information.
An unauthenticated private request returns the same useful help reference:

```json
{
  "request_id": "request-example",
  "server_time": "2026-09-09T18:00:00Z",
  "error": {
    "code": "authentication_required",
    "message": "Configure this workstation's agent credential, then reconnect.",
    "details": {"help_path": "/api/v1/help/authentication"},
    "next_actions": [{"action": "show_operator_setup_help"}],
    "retryable": false
  }
}
```

Browser sign-in/out and password-change endpoints operate on local human
accounts. Admin endpoints manage principals and issue/revoke tokens. First-admin
creation and account recovery use a host-local command, never public enrollment.
All issued agent tokens have agent identity; an agent token cannot record a human
approval even when a human operator created it.

Credential issuance displays a newly generated token once. Its mutation receipt
retains the issued credential ID and metadata, never a replayable token value.
If the issuing response is lost, replay returns the original issuance identity
with `secret_unavailable` and instructions to revoke that unused credential and
issue a replacement. It cannot silently create a second token or claim the old
secret can be recovered from its verifier.

For a harness session, the client generates and saves a session ID and a random
session proof before `POST /api/v1/sessions`. Send the proof in
`X-Coordinator-Session-Proof`; store only its verifier on the service. Return the
session ID and state without echoing the proof. Identical retries can recover the
session without a stored replayable secret. The session records its principal,
issuing credential, workstation, harness label/version, and declared capabilities.

Subsequent ownership operations send `X-Coordinator-Session` and the proof header
in addition to the agent token. A session belongs to its issuing credential;
revocation invalidates its authority. `GET /api/v1/sessions/{id}` reconciles it;
`POST /api/v1/sessions/{id}/close` closes it explicitly. Compaction and ordinary
turn completion do not close it or create another session automatically.

The CLI implements this as `agent-coordinator connect`, returning orientation
and current ownership. It reads repository binding and protected local session
state. First connection creates a session; resume reconciles the saved one. The
underlying HTTP workflow remains fully documented for clients without the CLI.

## Orientation and claiming

`GET /api/v1/projects` lists all projects for every authenticated principal.
`GET /api/v1/projects/{project_id}/orientation` returns current rules, policy and
instruction versions, session work, recovery candidates, blockers/decisions,
relevant lessons, candidate tasks, and next actions. Required rules may require
pagination; `instructions_complete: false` prevents new work until acknowledged.
`POST /api/v1/sessions/{id}/instruction-acknowledgments` records the project,
policy/instruction revisions, and required section IDs the client has received
and read. Claims require the current complete acknowledgment. This establishes
protocol acknowledgment, not proof that a model understood the prose.

`POST /api/v1/projects/{project_id}/claims` accepts exactly one task ID or a
next-eligible selector. For an explicit task, include its expected revision.
For next-eligible work, supply permitted kinds, capabilities, and the policy/
instruction versions read. The server applies the same eligibility checks to
both forms. `mode` is `work` or `recovery`.

```json
{
  "task_id": "task-example",
  "expected_task_revision": 4,
  "mode": "work",
  "policy_revision": 3,
  "instruction_version": "1"
}
```

A successful claim returns the task, attempt ID, ownership generation, server
lease deadline, remaining lease duration sampled before sending the response,
renewal recommendation, and worktree/resource preparation steps. The client
subtracts elapsed monotonic request time and a safety margin from the returned
remaining duration. It does not assume its wall clock matches the service.

A specific competing claim returns 409 with current status and alternatives.
Next-eligible selection with no available work returns 200 with `claim: null`,
structured queue reasons, and a suggested next check. An empty queue is normal.
Retrying a successful claim retrieves its original receipt and current authority
status; it cannot return a different task as a substitute.

CLI equivalents are `tasks list`, `claim --task <id> --revision <n>`, and
`claim --next`, with explicit project binding and JSON output available.

## Attempt operations

All paths below are under `/api/v1/projects/{project_id}`. Attempts have their
own IDs and ownership generation; ownership-dependent input must name the
expected generation. Authorized historical reads/late notes do not require a
live ownership grant. Human review uses browser authentication and CSRF with a
human-owned review attempt; it does not require an agent-token session proof.

| Operation | Endpoint | Required content |
| --- | --- | --- |
| Inspect current work | `GET /attempts/{id}` | Returns current authority and historical outcome |
| Renew | `POST /attempts/{id}/renew` | Generation and health observation; no invented progress |
| Register checkout | `POST /attempts/{id}/checkout` | Workstation, resolved worktree identity, branch, base revision, local path, clean/dirty state |
| Checkpoint | `POST /attempts/{id}/checkpoints` | Generation, summary, current action, next step, blockers, revision/job references |
| Reserve resources | `POST /attempts/{id}/reservations` | Complete requested resource/unit set; grant all or none |
| Delegate reporting | `POST /attempts/{id}/reporters` | Current generation, permitted attempt/job IDs, bounded reporting deadline, client-generated reporter-proof verifier |
| Submit | `POST /attempts/{id}/submit` | Generation, expected task/policy revisions, outcome evidence, handoff, lessons |
| Relinquish | `POST /attempts/{id}/release` | Generation, final checkpoint, ready/blocked disposition and reason |
| Resolve recovery | `POST /attempts/{id}/recovery-resolution` | Generation, inspected evidence, disposition and outstanding holds |
| Add late information | `POST /attempts/{id}/late-notes` | Historical attribution and text/evidence; grants no current authority |

Checkpointing does not implicitly renew ownership. The CLI may explicitly perform
both operations and must report each outcome. This avoids a retry of an old
checkpoint appearing to grant a fresh lease.

Submitting code includes a candidate repository/base/commit/tree identity and
check evidence. Review submission names the immutable reviewed submission and
decision. Integration submission names the target before/after revisions,
publication observation, and verification of the resulting tree. Shared domain
guards reject content that is inappropriate for the task kind or obsolete input.

## Jobs, artifacts, knowledge, and decisions

| Surface | Operations and behavior |
| --- | --- |
| Tasks | Create, get/list, revision-checked edit/admit, dependencies, cancel, supersede; no unrestricted status write |
| Jobs | Register before local launch; append authorized observations; report producer terminal status; inspect without relaunching |
| Artifacts | Explicit bounded upload, authenticated metadata/download, authorized deletion with retained tombstone; external link registration |
| Knowledge | Create/search/get, revision-checked correction and supersession, usefulness feedback, explicit rule adoption under current delegated permission |
| Decisions | Open a scoped question, inspect pending answers, record an answer under the required actor type, preserve the authorization it conveys |
| Project policy | Get current/history, revision-checked update under human or delegated agent authority |
| Events | Cursor-based authenticated history, filter by project/task, no credential values |
| Imports | Upload/inspect source, preview mappings/conflicts, then explicitly apply a versioned preview |
| Exports | Generate a snapshot at an identified record/event revision with provenance and generated-file markers |

Job observations have a separate authorized reporter identity and monotonically
increasing observation sequence. They do not require a still-active implementation
lease, but do require current reporter authorization. Late running observations
cannot overwrite a terminal result. Corrections to erroneous terminal reports
are explicit attributed amendments, not silent history replacement.

Task submission may reference only finalized artifacts. Knowledge updates made
independently use revision checks; new lessons included in a task submission are
stored atomically with that submission. Import apply must detect service changes
since its preview rather than overwriting them.

## CLI behavior and contract checks

Provide JSON input through `--input <file>` or standard input and JSON output
through `--json`. Native PowerShell and Linux-shell examples use the same payload
files. Keep API tokens/session proofs in the protected client configuration or
explicit process environment, not positional command-line arguments or output.

Use exit 0 for success, 2 for invalid input, 3 for authentication/setup required,
4 for denied operations, 5 for state/ownership conflicts, 6 for unsatisfied
requirements, and 7 for temporary transport/service failures. JSON includes the
stable API error code and remedy. The CLI never generates a new mutation key
merely because a response was lost.

Reporter credentials are subordinate to the issuing principal/agent credential.
They permit only the named observations and, when explicitly delegated, bounded
lease renewal. They cannot create sessions or extend their own scope/deadline.
Revoking the parent credential revokes reporters. Attempt expiry stops delegated
renewal; appropriately authorized job observations may continue without
reactivating the expired attempt. Use a separate bearer credential namespace so
reporter credentials cannot be mistaken for full agent API tokens.

Acceptance includes a complete workflow performed once through the CLI and once
through direct HTTP: connect, read instructions, claim, register checkout,
checkpoint/renew, report a job, submit, review, integrate, retrieve lessons.
Exercise lost responses, two-session ownership isolation, revocation, expiry,
stale policy, no-ready-work, and a reviewer who contributed to the candidate.
Every advertised next action and CLI command must match shipped schemas/help.
