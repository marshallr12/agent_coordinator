ALTER TABLE tasks ADD COLUMN archived_at INTEGER;
ALTER TABLE tasks ADD COLUMN deleted_at INTEGER;
CREATE INDEX task_archive ON tasks(project_id,archived_at,id);
