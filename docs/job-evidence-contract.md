# Job/worktree milestone integration contract

This milestone implements backlog item 1. It does not enable task completion,
review, integration, artifact uploads, or automatic remote execution.

## Ownership of implementation files

- Service agent: `server/src/jobs.rs`, migration `0003_jobs.sql`, job tests.
- Local runner agent: new `crates/local/**`, local journal/process/report tests.
- Workstation CLI agent: `crates/cli/**`, `docs/CLI.md`; worktree preparation and
  resource/job commands, calling the local library for guardians.
- Root: manifests/lockfile, server auth/middleware/mutation integration and
  coordination gates, end-to-end checks, operator views and status/handoff docs.

## HTTP interface (successes use the existing envelope)

GET/POST `/api/v1/resources`: globally shared canonical `key`, `capacity` (1–1000),
`description`; creation human-only, uniqueness by key, immutable initial capacity.
GET/POST `/api/v1/projects/{p}/attempts/{a}/reservations`: create
`{generation,items:[{resource_id,units}]}` atomically, one active reservation per
attempt; same resource identity blocks across projects. All-or-none admission.
POST `/api/v1/projects/{p}/reservations/{r}/release` `{generation,reason}`:
owner may release only when every attached job has a terminal producer result.
POST `.../reservations/{r}/resolve` `{reason,evidence}`: explicit human resolution
of an uncertain physical resource, retaining provenance and affected job history.
GET `/api/v1/projects/{p}/reservations`: bounded cursor list with derived held /
recovery-required status. Never free holds on observer/lease/session loss.

POST `/api/v1/projects/{p}/attempts/{a}/jobs`:
`{generation,job_id,producer_id,runner_instance_id,workstation_id,label,
source_revision,source_tree,reservation_id,reporter_id,reporter_proof,
renew_for_seconds}`. IDs are UUIDs, reporter_proof is client-generated random
32 bytes encoded hex, persisted before registration. Store only its verifier;
do not store proof in receipt/event. `renew_for_seconds` 0–3600 (0 disables
delegated renewal); observation authorization lasts seven days, independently
of the task lease. Requires an active owned work-mode attempt, registered clean
checkout, matching workstation, and held reservation belonging to the attempt.
Result `{job,reporter:{id,expires_at,renew_until},renew_after_seconds}` never returns
the proof. Job IDs and producer IDs are unique; repeat IDs never mean relaunch.
GET `/api/v1/projects/{p}/jobs[?cursor=...]` and `/jobs/{j}` expose producer state,
last observation/freshness, source identity, reservation, and terminal result.

Reporter bearer namespace: `acr_<reporter UUID>.<proof>`; no agent token or
session headers in a guardian. Only these paths accept that bearer:
GET `/api/v1/reporters/{id}` returns its named job and current renewal/observation
authority (not its proof). POST `.../{id}/observations`
`{sequence,producer_id,state,pid,process_started_at,exit_code,inputs_unchanged,
summary}` where state is `registered|running|succeeded|failed|unknown|not_started`.
Sequence positive and strictly increasing; duplicate same sequence/body replays,
changed same sequence conflicts, old observations never overwrite newer/terminal.
PID is optional metadata, never sole identity. Terminal observations require
exit_code for succeeded/failed; not_started requires an explicit local failure
before launch; unknown is nonterminal. Observations may continue after attempt
expiry or session closure while parent credential/principal and reporter remain
authorized. Never automatically release a hold from an observation.
POST `.../{id}/renew` `{generation}` caps the normal lease to renew_until and
requires current attempt/generation, active parent session, and live deadline.
A reporter cannot create tasks/sessions, checkpoint, expand scope, or renew its
own window. Normal bearer credentials cannot impersonate reporter requests.

Root middleware calls `jobs::ReporterAuth::authenticate(parts,state)` for paths
beginning `/api/v1/reporters/`, before decoding bodies. Its `verify(connection,now)`
returns the parent Actor after checking subordinate/parent authorization. Root
provides `Mutation::begin_reporter(state,auth,headers,operation,input)` with the
same immediate-transaction, receipt/event guarantees and reporter-bound fingerprint.
Root exposes `coordination::{Attempt,owned}` as pub(crate) for service handlers.

Root gates release/ordinary requeue/recovery resolution/checkout changes on
unresolved job/resource evidence, using service helper
`jobs::ensure_attempt_quiescent(connection,project,task_id)` (checks all attempts
of this task for held reservations or nonterminal jobs; descriptive conflict).
Blocked release remains possible and retains holds. Recovery never cancels jobs.

## Native workstations and guardians

CLI prepares a new Git worktree with no automatic reset/stash/cleanup, records
resolved Git-dir identity, branch and full base SHA, and preserves other checkouts.
Persist a local preparation intent before Git worktree creation so retry reconciles
the same path/branch rather than creating a second worktree. Paths with spaces work
on Linux and Windows. Resolve repository identity against configured remote URL;
do not infer a project from a directory basename.

CLI `job run` receives a local program/argv JSON file, an attempt/generation,
reservation ID and prepared checkout. Require a clean committed source snapshot
for this milestone. Raw argv/environment/log bytes are not uploaded. Persist
job/producer/runner/reporter identities before registration or local launch.
A guardian subprocess of the installed CLI gets only the scoped reporter token
in a protected job state file. Strip coordinator agent credentials/session env
from guardian and child. It records a durable launch-intent before spawning.
If interrupted after intent, never spawn again; show uncertain/inspect-existing.
An OS file lock serializes guardians, but losing that lock never proves producer
termination. Record PID plus OS process start identity, distinct producer UUID,
and terminal exit journal; PID reuse must never attach to a replacement process.

The local library supplies public types/functions agreed with the CLI agent;
agents coordinate that Rust API directly. Job inspect/reconnect replays durable
pending observations to the same reporter/job and never launches a producer.
Capture terminal result and input stability even offline; log files are local,
explicit, bounded, and protected. Unknown observation state retains resources.

Optional renewal requires an explicitly supplied harness PID captured with its
OS start identity. The guardian stops renewal when that exact harness exits,
authorization expires, or renewal window ends, while job reporting may continue.
No anonymous forever-renewing helper and no automatic agent/task launching.
