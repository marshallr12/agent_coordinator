# Markdown import and export contract

The service owns current task, knowledge, decision, policy, submission, and handoff
state. Markdown is bounded source evidence. Supplying a path never asks the service
to read that path, inspect a repository, run a hook, or execute imported text.
Clients read files locally and send explicit chunks with provenance.

## Preview

`POST /api/v1/projects/{project}/imports/preview` accepts:

```json
{
  "source": {
    "context": "stable operator-defined repository/source identity",
    "git_revision": "full 40- or 64-character commit ID",
    "observed_at": "2026-09-09T20:00:00-04:00",
    "branch": "development",
    "environment": "audited workstation"
  },
  "chunks": [{"path": "BACKLOG.md", "markdown": "# Tasks\n- [ ] Example"}],
  "historical_mappings": [{
    "path": "HANDOFF.md",
    "section_identity": "Session / Closed work",
    "title": "Earlier implementation",
    "disposition": "closed",
    "evidence": "Exact bounded historical evidence"
  }]
}
```

The request contains 1–32 chunks, at most 200 KiB of Markdown, and at most 200
parsed checklist items and historical mappings. Paths are normalized relative
metadata with no parent traversal. Source context, path, heading lineage, and an
item anchor form the stable identity. A checklist can carry an explicit stable
anchor such as `<!-- coordinator-id: release-check -->`; this is required when
titles may change or duplicate inside one section. A number such as `26` is never
the identity by itself.

Only explicit Markdown checkboxes become task records. Unchecked items import as
`planned`, never ready. Checked items import as `done` with durable closure
provenance tied to the source revision, exact preview source digest,
observation time, branch, environment, path, and section. Ordinary prose cannot
create a ready or done task. Explicit historical mappings create searchable
project knowledge with `closed`, `rejected`, or `superseded` disposition; they do
not create tasks or execution authority.

The response is an immutable preview with `id`, `digest`,
`project_event_revision`, normalized items, conflicts, unresolved Markdown links,
source provenance, and application time. It also captures each matching import
record revision. Duplicate identities and generated-export input are blocking
conflicts. Newer service edits are reported and preserved. Missing relative links
are reported without causing the service to follow or read them.

Preview creation is idempotent with the normal persisted `Idempotency-Key`. Its
audit event is project-scoped, but preview events are excluded from the
authoritative project revision used by imports and exports. Creating or reading a
draft preview therefore does not stale that preview or another draft preview.

## Apply

`POST /api/v1/projects/{project}/imports/{preview}/apply` accepts:

```json
{
  "preview_digest": "exact digest returned by preview",
  "expected_project_event_revision": 42
}
```

A currently authenticated human applies a preview. Agent credentials may create
and inspect previews and read exports, but cannot apply them. This conservative
initial migration boundary exists because a checked historical checklist creates
durable closure outside the live completion protocol; widening it requires an
explicit delegated-import policy. Under the SQLite writer lock,
the service rechecks the preview identity and digest, project event revision,
prior import-record revisions, actor authority, and blocking conflicts. Any
project mutation after preview causes a stale-preview conflict. Application,
import records, task/knowledge revisions, the idempotency receipt, and audit event
commit atomically.

Reimport never changes a linked task or knowledge record that has a newer service
revision. If the service projection is still at the last imported revision, a
reimport may update it. Once an imported item or its service task is closed, later
unchecked, missing, archived, or renamed source cannot reopen it. A checked item
is imported closure evidence, not a live completion endpoint and not a substitute
for the normal review/integration workflow on service-created work. Rejected
historical prose remains noneligible.

Imported `AGENTS.md`, `CLAUDE.md`, handoff text, command examples, and hook names
remain inert source content. Imports do not change project or workflow policy,
grant authority, install hooks, or run commands.

## Read and export

`GET /api/v1/projects/{project}/imports/{preview}` reads the immutable preview and
its application status.

`GET /api/v1/projects/{project}/exports?limit=50&cursor=...` returns at most 200
records and caps the complete response page at 256 KiB. The projection includes current
service tasks, immutable submission handoffs, project and workflow policy
revisions with provenance, current searchable knowledge, decisions and answers,
and imported historical records. It returns full record contents without silent
truncation in structured `records` plus `markdown`, `snapshot_event_revision`,
`generated_at`, `generated: true`, `next_cursor`, `page_complete`, and an explicit
empty `omissions` list. If one record alone cannot fit, export fails with
`export_record_too_large` instead of dropping content.

The opaque cursor binds the last sort key to the project event revision. If the
project changes between pages, the next page returns `export_snapshot_changed`;
the client restarts rather than labeling pages from different states as one
snapshot. Generated Markdown begins with
`agent-coordinator-generated-export`. Feeding it to preview produces a blocking
conflict because exports are views of authority, never replacement authority.

## Audited fixtures and limits

The tests use small sanitized excerpts read with `git show` from SithBit
`20368b6fdb8c457cd822480f44c509253b9ea385` and Submission
`bbbdf8b6dbeee80ff0d1b87afaf5a91597c953c9`. They cover the stale Item 26/do-not-requeue
closure, repeated numerical labels, Submission's stale July 20 resume prose,
later merge/branch context, and unresolved memory links. The source repositories
were read only; no checkout, hook, or source file was changed.

The parser intentionally supports headings, bullet checkboxes, explicit stable
anchors, and explicit historical mappings rather than general Markdown semantics.
Source archives that need richer interpretation must provide reviewed mappings.
