# Knowledge and decision contract

This contract defines the implemented revisioned knowledge, bounded context, binding-rule history, and scoped decision service. Knowledge is advisory evidence. Project `rules` remain the binding policy, and decision answers remain narrow workflow gates. Neither record changes credentials, roles, permissions, review requirements, integration authority, or any other authorization.

## Knowledge records

Each knowledge identity belongs to its source project and has an immutable sequence of revisions. The current projection is searchable, while older revisions remain available through cursor pagination. Supported kinds are `lesson`, `fact`, `rejected_approach`, and `checkpoint`. Supported statuses are `observed`, `validated`, `deprecated`, and `superseded`. A correction always creates the next revision; it never overwrites text, attribution, scope, provenance, or status in an earlier revision.

The request fields are:

| Field | Meaning |
| --- | --- |
| `kind`, `title`, `body`, `status` | Classified content and evidence status |
| `scope.task_ids` | Source-project task identities to which the record applies |
| `scope.components`, `scope.environments`, `scope.versions` | Bounded applicability filters |
| `tags`, `applicability` | Search and human-readable applicability |
| `provenance.summary`, `source_uri`, `source_task_id`, `source_submission_id` | Origin and exact service records supporting the statement |
| `collection` | `project` or `shared` |
| `share_across_projects` | Required explicit opt-in for `collection=shared`; must be false for project-only records |

A shared record keeps its source project and provenance. It appears in another project's list or context only when that caller sends `include_shared=true`. Shared collection membership is not an access grant: every authenticated principal can already read all projects. It is an explicit relevance choice.

`PATCH /api/v1/projects/{project}/knowledge/{id}` requires `expected_revision` and the complete new title, body, status, scope, tags, applicability, and provenance. The kind and collection are stable identity fields. `status=superseded` requires `superseded_by_id`; the referenced replacement remains a separate record with its own history. Deprecated, superseded, and observed results are returned with their status and are never presented as current binding policy.

Usefulness feedback requires `expected_revision`, is append-only, and is bound to the exact knowledge revision the caller read. A concurrent correction produces `revision_conflict` instead of attributing stale feedback to the new revision. A later correction does not rewrite earlier feedback. The detail response reports bounded revision history plus useful/not-useful totals.

The routes are:

- `GET|POST /api/v1/projects/{project}/knowledge`
- `GET|PATCH /api/v1/projects/{project}/knowledge/{id}`
- `POST /api/v1/projects/{project}/knowledge/{id}/feedback`

Lists use a stable opaque `updated_at:id` cursor and a limit from 1 through 200. Knowledge history uses a descending revision cursor and the same limit range.

## Submission lessons

A submission may include up to 50 `SubmissionLessonInput` values. The server validates the complete batch, all source-project task references, and the explicit shared opt-in before inserting any record. It then inserts every lesson and its revision 1 inside the submission's existing SQLite writer transaction. Provenance is pinned to the exact project, task, submission, actor, and service time. A validation error rolls back the submission and all linked lessons together.

The workflow integration calls:

```rust
insert_submission_lessons(
    conn,
    actor,
    now,
    project_id,
    task_id,
    submission_id,
    lessons,
)
```

The returned values are the same flat knowledge objects returned by the create route.

## Binding-rule provenance

The existing `projects.rules` and `projects.policy_revision` projection remains the only binding project policy. Every API policy change keeps the existing authority rule: a human may change policy, while an agent may change rules only when `agent_rule_editing` is already enabled and may not alter that delegation or integration permission.

Each `policy_revisions` row stores the complete projection, actor, service time, and an optional bounded provenance explanation. Database triggers reject updates and deletes of those historical rows. `GET /api/v1/projects/{project}/policy/history?cursor=&limit=` returns descending immutable revisions. Empty provenance is retained for revisions created before provenance was supplied.

No knowledge import, knowledge correction, feedback, or decision answer promotes text into `projects.rules`. Promotion is an explicit policy PATCH under the current policy revision and existing delegated authority.

## Bounded context search

`GET /api/v1/projects/{project}/context` accepts:

- required `q`, limited to 1,024 bytes and 32 search terms;
- `limit` from 1 through 100;
- `budget` from 1,024 through 131,072 bytes;
- `include_shared=true` for explicitly shared cross-project knowledge;
- optional `task_id`, `component`, `environment`, and `version` knowledge-scope filters.

SQLite FTS5 searches the current task title, description, and acceptance criteria and the current knowledge title, body, tags, and applicability. Results are ordered by FTS rank with a stable identity tie-break. Pending or invalidated decisions for the requested `task_id`, or all project decisions when it is omitted, are placed before text matches. An empty scope dimension means generally applicable; a populated dimension must match the corresponding requested filter. Items are included only as complete JSON objects, stopping at the item count or byte budget. Empty and truncated results include a concrete `next_actions` value.

TypeSafe reranking can reorder the task and knowledge items already selected by this bounded search. It runs whenever a nonempty `TYPESAFE_API_KEY` (surrounding whitespace is ignored) is present in the server process environment when `serve` starts. When the key is unset, empty, or only whitespace, the server logs one warning at startup, makes no TypeSafe request, and returns every context response in the original FTS order. Administrative subcommands never read the key.

With the key present, an admitted context request with 2 through 40 task and knowledge items sends data to TypeSafe, a third party, over HTTPS: the query and, for each of those items, an excerpt of at most 1,800 characters made of its title, its task description or knowledge body, and its task acceptance criteria or knowledge applicability. Decisions and the policy are never sent. Decisions keep their original positions; the task and knowledge items are reordered by descending score within their own positions, with ties kept in FTS order. Fewer than 2 or more than 40 candidates, a connection failure, a timeout (1 second to connect, 3 seconds in total), a redirect or other non-success status, a response larger than 256 KiB, or a response missing a score from 0 through 3 for any candidate retains the original FTS order and the same response items and budget.

The reranker shared by the server's requests permits at most four concurrent provider calls by default, including response-body reading. A saturated request skips immediately with reason `busy`, sends nothing to the provider, and returns a successful context response in the original FTS order. After three consecutive completed provider or score-validation failures, the shared circuit opens for 60 seconds by default. Eligible requests during that window skip with reason `circuit_open`, make no provider call, and preserve the complete selected items, policy, and budget. Success resets the failure count; candidate-count and busy skips do not count as failures.

After the open window, exactly one request may serve as a recovery probe while other requests continue to skip. A successful probe closes the circuit and resumes relevance ordering; a failed or cancelled probe starts a fresh open window. If the probe cannot acquire a concurrency permit, its reservation is released so another request can retry when capacity becomes available. Completions from requests admitted before a newer circuit transition cannot change that newer circuit state. These controls do not add retries, requests, or additional data fields sent to TypeSafe.

Each attempt logs its outcome, skip or failure reason when applicable, candidate count, and elapsed time, never item text or the key. The read transaction is released before the external request. This reranking does not expand the FTS candidate set or change scope, policy, or decision authority.

The complete current `projects.rules` value is always returned separately as `policy`. It is never truncated. `instructions_complete=false` and `truncated=true` indicate that the requested byte budget was smaller than the mandatory policy object; in that case no optional context items are returned. Callers must increase the budget before treating the packet as a complete orientation.

## Decisions

A decision is a structured question, 2–20 exact answer options, rationale, required actor (`human`, `agent`, or `either`), and one or more affected task IDs pinned to exact task revisions. Each generation also pins the current project policy revision, environment, conditions, optional expiry, reopening rationale, actor, and service time.

An answer requires:

```json
{
  "expected_generation": 2,
  "disposition": "allow",
  "answer": "Proceed",
  "rationale": "The recorded rollback owner is present.",
  "conditions_confirmed": true
}
```

`answer` must exactly match one listed option. `disposition` is `allow`, `deny`, or `defer`. An `allow` requires `conditions_confirmed=true`; `deny` and `defer` are durable answers but keep affected work blocked. Actor type is checked after the SQLite writer lock is obtained, and an agent answer is attributed to a live agent session. Human-required answers require a human principal, except as relayed below.

### Relayed human answers

Design note (task `d4ee5302`): a human often answers in the agent's terminal, so a
project's policy may carry the human-only switch `allow_relayed_human_answers`
(default off; agents cannot change it, like `integration_owner`). When it is on,
an agent session may add `"relay": {"prompt": "...", "response": "..."}` to an
answer for a `required_actor=human` decision. The relay object is rejected for any
other decision or caller. The service then records the answer under the agent
principal and live session (`actor_id`, `actor_session_id`), sets `relayed=true`,
and stores the verbatim `prompt` and `response` plus the human principal
(`authorized_by`) and policy revision that enabled relaying. Without `relay`, with
the switch off, or with a stale `expected_generation` the answer is refused.
`answer.relay` and each history entry's `relay` are null for a direct answer. The
evidence is the agent's attestation bounded by an explicit human opt-in and kept
for audit; a human can reopen any relayed decision. A stronger per-answer human
confirmation (a proposed answer the human confirms) was considered and left for
later.

### Recommendations and reversible decisions

A decision may carry a `recommendation` (exactly one of its `options`) and
`reversible: true`; a reversible decision needs a recommendation. When the
current cycle of such a decision has gone unanswered for 24 hours, the service
answers it: an `allow` with `answer` equal to the recommendation, conditions
confirmed, and `timed_out: true`, attributed to the principal that asked (there
is no live session). The sweep runs inside `serve` every five minutes under the
writer lock and skips a decision whose scope is stale or whose own `expires_at`
has passed, so a recommendation is never applied to a different scope. A human
can reopen a timed-out decision like any other.

Only a decision an agent may take itself can time out. A decision with
`required_actor: human` is reserved for a human, so it times out only when a
human created it or last reopened it. The service refuses `reversible: true`
together with `required_actor: human` when an agent creates the decision, and
refuses an agent's reopening of such a decision. The sweep also skips, and the
digest does not list as pending, a human-required decision whose current cycle an
agent opened, including rows recorded before this rule. Decisions with
`required_actor` `agent` or `either`, and decisions that are not reversible,
follow the rules above: the former may time out, the latter never do.

The project digest
(`GET /api/v1/projects/{project_id}/digest`, see the API contract) lists what
proceeded this way and what is about to.

A prior allow stops blocking only while all of the following remain true:

- every affected task's judged fields (title, description, acceptance criteria,
  kind) are unchanged since the pinned revision; priority and dependency edits do
  not count;
- the project's rules text is unchanged since the pinned policy revision; other
  policy changes such as the lease length or review mode do not count;
- the allow explicitly confirmed the recorded conditions and environment;
- the decision has not expired.

If any of these scoped pins changes or the allow expires, every affected task remains blocked until the decision is reopened against all current revisions and answered again. This prevents an answer for one task in a multi-task scope from silently carrying forward after another affected task changes. Reopening creates a new immutable generation and cannot change the question, options, or required actor, so it cannot weaken a human requirement. Older generations and answers remain inspectable.

Opening or reopening a decision is rejected while any affected subject or activity holds integration publication authority. This avoids issuing a new blocker after a publisher has already received exclusive authority for an external compare-and-swap operation.

The routes are:

- `GET|POST /api/v1/projects/{project}/decisions`
- `GET /api/v1/projects/{project}/decisions/{id}`
- `POST /api/v1/projects/{project}/decisions/{id}/answer`
- `POST /api/v1/projects/{project}/decisions/{id}/reopen`

Decision lists use stable `created_at:id` cursors. Detail history uses descending generation cursors. Current status is `pending`, `allowed`, `denied`, `deferred`, `expired`, or `stale`; `work_allowed` is true only for `allowed`.

## Workflow gate

After obtaining SQLite's writer lock and rechecking authentication, time, generation, ownership, and policy, ownership-dependent operations call:

```rust
ensure_decisions_resolved(conn, project_id, task_id, now)
```

The guard returns `decision_required` with at most 20 stable decision IDs and a truncation flag. It applies to ordinary work selection/claims, active ownership-dependent mutations, submission, workflow claims and workflow operations for both the subject and activity task, publication preparation/result/finalization, and completion. Renewal, checkpoint, release, and recovery inspection remain available so an owner can preserve or relinquish work safely. A recovery claim may inspect stale work, but ordinary work cannot resume until the decision gate passes.

Task readiness uses the same predicate as the transaction guard. Lists and orientation therefore report a decision-blocked task instead of advertising it as ready. Receipt replay never renews ownership or revives an expired decision allow.

All knowledge, feedback, decision, answer, and reopen mutations use the standard idempotency receipt and event transaction. The service stores no credentials or request headers in these records or events.
