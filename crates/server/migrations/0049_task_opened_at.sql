-- The moment a task first became open (claimable), as opposed to ready_since,
-- which is a queue-ordering value that is also reset on unblock, on release
-- from review and when dependencies finish. NULL while the task is planned.
--
-- Backfill: a planned task has no opened_at. Any other task takes the creation
-- time of its earliest saved revision whose lifecycle is 'open' (so a task
-- created planned and admitted later reads its admission time); a task with no
-- such revision (for example one that went straight from planned to done or
-- canceled) falls back to created_at.
ALTER TABLE tasks ADD COLUMN opened_at INTEGER;

UPDATE tasks SET opened_at = CASE
    WHEN lifecycle = 'planned' THEN NULL
    ELSE COALESCE(
        (SELECT MIN(r.created_at) FROM task_revisions r
          WHERE r.task_id = tasks.id
            AND json_extract(r.data_json, '$.lifecycle') = 'open'),
        created_at)
END;
