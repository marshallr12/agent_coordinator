-- Admission control. A task records where it came from (`origin`: the human or
-- agent principal that created it, or the service itself) and may carry an
-- `admission_class` that marks a fix the budget never holds. An agent-created
-- task without a class is admitted only while fewer than the weekly budget of
-- such tasks were admitted in the current ISO week across all projects;
-- otherwise it is created planned. `budget_admitted_at` marks the tasks that
-- consumed the budget and `budget_held_at` those the budget held.
ALTER TABLE tasks ADD COLUMN origin TEXT NOT NULL DEFAULT 'human'
    CHECK(origin IN ('human','agent','service'));
ALTER TABLE tasks ADD COLUMN admission_class TEXT
    CHECK(admission_class IS NULL
       OR admission_class IN ('revert','fix_target','deflake','refusal_fix'));
ALTER TABLE tasks ADD COLUMN budget_admitted_at INTEGER;
ALTER TABLE tasks ADD COLUMN budget_held_at INTEGER;

-- Existing tasks take the kind of the principal that wrote their first
-- revision; internal workflow tasks and reverts were created by the service.
UPDATE tasks SET origin='agent' WHERE EXISTS(
    SELECT 1 FROM task_revisions r JOIN principals p ON p.id=r.actor_id
    WHERE r.task_id=tasks.id AND r.revision=1 AND p.kind='agent');
UPDATE tasks SET origin='service' WHERE id IN (SELECT activity_task_id FROM workflow_activities)
    OR id IN (SELECT task_id FROM task_reverts);

CREATE INDEX tasks_budget_admitted ON tasks(budget_admitted_at) WHERE budget_admitted_at IS NOT NULL;
CREATE INDEX tasks_budget_held ON tasks(budget_held_at) WHERE budget_held_at IS NOT NULL;
