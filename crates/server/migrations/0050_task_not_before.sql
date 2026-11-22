-- A planned task may carry a not-before time. Once the service clock passes it,
-- the service opens the task itself (see coordination::open_due_tasks), as a
-- human admission would. `not_before_by` is the principal who set the schedule,
-- recorded as the actor of the scheduled opening. Both columns are cleared when
-- the task opens, whether by schedule or by an edit, so a later return to
-- planned does not reopen it.
ALTER TABLE tasks ADD COLUMN not_before INTEGER;
ALTER TABLE tasks ADD COLUMN not_before_by TEXT REFERENCES principals(id);
CREATE INDEX tasks_not_before ON tasks(not_before) WHERE not_before IS NOT NULL;
