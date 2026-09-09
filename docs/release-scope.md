# First-release scope and implementation readiness

Status: planning complete. Confirmed choices are in [PLAN.md](../PLAN.md).
Engineering defaults remain distinguishable from operator-selected requirements;
they will be validated through implementation and release testing.

## Operating targets

| Choice | Selected target | Status |
| --- | --- | --- |
| Initial operating size | 20 projects, 50 simultaneous agent sessions, 100,000 historical tasks | Confirmed |
| Backups | Hourly, retaining 24 hourly and 30 daily copies; documented off-server copying | Confirmed |
| Restore | One-hour target in the documented recovery exercise | Confirmed |
| Server baseline | Ubuntu 24.04 LTS, x86_64, 2 CPU cores, 4 GB RAM | Engineering default; production host undecided |

No blocking product questions remain. Native Linux systemd installation,
HTTPS, all-project access, authentication method, agent task creation/claiming,
worktree isolation, recovery mode, and integrated completion are already settled.
Linux and native Windows clients, HTTP API/web/CLI interfaces, and configurable
agent/human/both review are also confirmed. Projects may allow automatic agent
integration or require human authorization. Agents maintain shared lessons;
projects may additionally delegate changes to binding rules without human
approval. Bounded log/report uploads, Git source checkpoints, and artifact links
are confirmed. MCP, TUI, and source-worktree bundle storage are deferred.

## Proposed engineering defaults

These are recommendations for the implementation specification, not additional
operator confirmations. They may be adjusted when the product answers require it.

| Area | Default |
| --- | --- |
| API | Versioned JSON HTTP API; stable error codes, explicit next actions, cursor pagination, JSON-capable CLI |
| Task edits | Revision-checked updates; atomic claims and transitions; no generic ownership/status overwrite |
| Selection | Explicit project, ready prerequisites, compatible capabilities, priority, then oldest-ready time and stable ID |
| Task structure | Simple tasks by default; optional parent objectives and acyclic dependencies; separate review/integration activities referencing a submitted candidate |
| Deduplication | Stable source IDs, idempotency, search-before-create, durable duplicate/supersession links; no required model service |
| Agent context | Required current policy plus bounded relevant tasks/lessons; paginated history; no silent truncation of mandatory instructions |
| Knowledge retrieval | Full-text search, project/component/version tags, provenance, correction and supersession; optional cross-project search |
| Local rules | Repository instructions remain applicable; imported memory is evidence/context until explicitly adopted as policy |
| Markdown migration | Preview then apply; configurable filenames and source directories; stable import mappings; no automatic ready-task creation from unstructured historical prose |
| Dashboard updates | Periodic refresh with last-updated time; persistent attention queue for blockers/decisions; external email/chat/push integrations deferred unless requested |
| Local adapters | Vendor-neutral connect/resume/checkpoint/job-report operations with documented hook examples; no automatic alteration of global harness hooks |
| Permissions | Separate administrator, human operator, and agent operations; no project ACLs; decisions claiming human authorization require a human principal |
| Rule delegation | A project can grant agents permission to edit binding rules; permission grants and credential administration remain distinct from the rule text |
| Evidence | Immutable submissions, explicit check roster, exact input and integrated revision, producer identity, artifact accessibility |
| API retries | Stable mutation keys and transactional receipts; uncertain external effects require reconciliation |
| Persistence | SQLite on local storage, short transactions, migrations, audit events, tested backup/restore |
| Browser stack | Vanilla JavaScript modules and CSS, served by Axum; use Alpine only if implementation demonstrates a simplification |

Propose a one-minute reporting interval and ten-minute renewable ownership
window, configurable per project within server limits. The bounded-helper
contract is in [coordination-contract.md](coordination-contract.md). A
stale-progress warning is separate from lease expiry. Validate these defaults
against the long-running SithBit gates before treating them as release defaults.

For Markdown input, treat BACKOFF.md as a configurable additional filename.
Support BACKLOG.md directly. This avoids making a possible filename typo block
the design while still supporting a distinct file if one exists.

## Explicitly deferred features

- MCP and interactive TUI clients.
- Remote launching/supervision of agent harnesses and arbitrary server-run jobs.
- Shared editable checkouts for independent implementation tasks.
- Source-worktree/build-directory bundle storage and a hosted source repository.
- Required model APIs, embeddings, semantic deduplication, or a custom workflow DSL.
- Multiple active coordinator servers, database clustering, and project ACLs.
- Email/chat/push integrations, public account registration, and email-dependent
  account recovery.
- Automatic rewrites of existing project/global hooks or automatic migration of
  SithBit/Submission into the running service.

## Readiness evidence and implementation work

| Requirement for a complete plan | Design artifact |
| --- | --- |
| Confirmed product choices and milestone order | [PLAN.md](../PLAN.md) |
| Workflow states and completion guards | [workflow-spec.md](workflow-spec.md) |
| Ownership, recovery, worktrees, jobs, evidence, knowledge | [coordination-contract.md](coordination-contract.md) |
| Authentication/session flow, endpoints, errors, CLI conventions | [api-contract.md](api-contract.md) |
| Database constraints, transactions, permissions, packaging, acceptance | [implementation-spec.md](implementation-spec.md) |
| First-run operator/agent workflow, native clients, backup/restore | [onboarding-contract.md](onboarding-contract.md) |
| Revisions justified by real project workflows and hooks | [repository-review.md](repository-review.md) |

During implementation, derive OpenAPI schemas and executable Linux/PowerShell
examples from the shared request/response types; implement migrations and
automated tests alongside each milestone. These deliverables are specified work,
not unfinished product questions or claims of an existing implementation.

The final host, public hostname, TLS configuration, off-server backup destination,
and initial credentials will be supplied during installation. Local acceptance
uses isolated repositories, disposable databases, test credentials, and the
documented baseline. Production access is not needed to implement the service.

Implementation readiness means these contracts agree and have testable outcomes.
It does not require selecting every crate version, cosmetic UI detail, or table
index in advance. Those are implementation choices validated during the relevant
milestone.
