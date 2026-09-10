# Objective grouping contract

Objectives provide optional parent grouping without introducing a completion
shortcut. An objective is an ordinary `general` task with its own description,
acceptance criteria, ownership attempt, immutable submission, and configured
review. Its objective record adds revisioned child membership and derived
readiness.

## Records and lifecycle

The objective ID is the wrapper task ID. Task APIs therefore expose the same
record and include these fields:

- `objective_id` is the task ID for an objective, otherwise `null`.
- `parent_objective_id` identifies a task's optional parent objective.
- `parent_objective_required` says whether that child gates its parent.
- `objective_children_ready` is true when every required direct child is done.

A task has at most one parent objective. Objectives may contain ordinary tasks
or nested objectives from the same project. Internal review and integration
activity tasks cannot be grouped. Required and optional children both
participate in cycle detection; only required children gate parent work.

An objective cannot be claimed until all required children have lifecycle
`done`. Optional children may remain open. The service repeats the required
child check inside submission and general-review completion transactions. A
parent therefore cannot become done through a stale claim or unchecked review.
Completing a required child updates the parent's ready time when the last
required child becomes done.

The parent still completes its own acceptance workflow. Grouping a set of done
children does not mark the parent done, create a branch, publish source, or
stand in for review.

## API

`POST /api/v1/projects/{project}/objectives` accepts:

```json
{
  "title": "Release objective",
  "description": "Coordinate the release outcomes.",
  "acceptance_criteria": ["Required outcomes and release evidence are reviewed"],
  "priority": 2,
  "planned": false,
  "children": [
    {"task_id": "task-id", "required": true}
  ]
}
```

Creation atomically inserts the general wrapper task, objective projection,
initial membership revision, task revision, receipt, and event. `priority`
defaults to 2, `description` to an empty string, `planned` to false, and
`children` to an empty list.

`GET /api/v1/projects/{project}/objectives?cursor=ID&limit=50` returns stable
ID-ordered summaries. Limits are 1–200. Each summary is a flat task record plus
`objective_revision`, child counts, completed required-child count,
`required_children_ready`, and `membership_frozen`.

`GET /api/v1/projects/{project}/objectives/{id}?cursor=REVISION&limit=50`
returns the summary plus all current children in caller-defined order. Current
membership is limited to 100 children. Each child includes its ID, title,
lifecycle, current task revision, derived work status, required flag, and
position. The response also includes at most 200 immutable membership-history
entries. `membership_history_next_cursor` is the exclusive revision cursor for
the next older page.

`PATCH /api/v1/projects/{project}/objectives/{id}/children` accepts:

```json
{
  "expected_revision": 1,
  "children": [
    {"task_id": "task-id", "required": true},
    {"task_id": "follow-up-id", "required": false}
  ]
}
```

`expected_revision` binds the objective's `objective_revision`. A successful
change appends immutable membership history and increments both
`objective_revision` and the wrapper task `revision`, so task history records
the changed completion scope.

## Mutation and freeze rules

Objective mutations use the normal SQLite writer transaction. Authentication,
current revision, scope, cycle, and freeze checks occur after the writer lock;
the effect, receipt, and event commit together.

Membership is editable only while the parent is open or planned and has never
had an ownership attempt or submission workflow. The first claim freezes it
permanently. This prevents later required/optional changes from invalidating a
submission, approval, or completed objective. `membership_frozen` exposes this
state for clients.

Task prerequisite edits and objective membership changes validate one combined
directed graph. An edge points from a task to its prerequisite and from an
objective to each child. The service rejects a change if the target can already
reach the source through either edge type. Cross-project children, duplicate
children, self-membership, and a second parent are rejected.

Ordinary task edits remain subject to task revision checks, active-work guards,
workflow guards, and the project's existing delegated rule-editing policy.
Objective membership cannot bypass those checks or weaken reviewed acceptance
requirements.
