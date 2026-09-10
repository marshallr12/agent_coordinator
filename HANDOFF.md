# Implementation handoff — 2026-09-09

Backlog item 2 is implemented: immutable submissions, independent review, serialized
integration, exact-result checks, publication recovery, and done/dependency release.
Continue with item 3 in BACKLOG.md: shared lessons, binding rules/provenance,
decision records, bounded context, artifacts, and Markdown import/export.
The complete design remains in PLAN.md; product decisions are settled.

Code owners submit clean committed candidates with acceptance evidence and a
handoff. Submission ends implementation ownership and creates separate review and
integration activities. Projects require independent agent, human, both, or no
review; independent agent review is the default. Contributor principals and
sessions cannot independently review their own work. Changes requested cancel the
candidate's pending activities; a new submission receives fresh reviews.

A human configures the canonical repository key and an explicit required-check
roster. Projects sharing a repository/target share one integration hold. Agents
prepare the exact integrated source in a separate worktree, run registered checks
with matching identity/version/environment, release job resources, then request
fresh publication authority. Native Git requires a clean candidate-containing
result, an exact expected target, and a conservative remaining deadline. Once
push intent is durable, retry only observes; it never launches a second push.

Only known publication, exact successful check receipts, fresh remote observation,
and quiescent resources permit finalization and dependency release. Human recovery
records old-publisher termination/isolation and remote evidence; it preserves the
original result and transfers the hold to a fresh integration activity. That
activity prepares against the actual target and reruns checks. Recovery cannot
manufacture a passing result or quietly discard already published work.

Instruction version 3 returns the implemented completion sequence and CLI help.
Use docs/CLI.md and docs/completion-contract.md for exact commands and bodies.
Independent reviewers need separately enrolled principals. The CLI now performs
instruction acknowledgment before review/integration claims as well as ordinary
claims. Git 2.39.5 was used locally; native integration requires Git 2.39 or newer.

Validation results are recorded in docs/implementation-status.md. The complete
Linux service/CLI exercise passes real Git preparation/publication, both reviews,
human authorization, refusal before required checks, exact producer evidence,
historical publish retry, and dependency release only after finalization. Browser
checks use disposable data; no source projects or histories were modified.

No public deployment occurred. Host, DNS name, and backup destination remain
installation choices. Backups/restore fencing, clock rollback, retention/load,
and two physical-workstation release acceptance remain backlog work. Source
snapshots must be clean and committed. Candidate source travels through Git
remotes; submission does not upload it. Logs remain local and service artifact
uploads remain deferred. Unknown remote trees require fetching the target for
inspection; a deleted target requires operator repair before continuing.
