-- Project-scoped context must narrow full-text candidates before ranking them.
-- Rebuild only the derived search index; authoritative task/revision rows remain.
DROP TRIGGER task_search_insert;
DROP TRIGGER task_search_update;
DROP TRIGGER task_search_delete;
DROP TABLE task_search;
CREATE VIRTUAL TABLE task_search USING fts5(
    task_id UNINDEXED,
    project_id,
    title,
    description,
    acceptance,
    tokenize='unicode61'
);
INSERT INTO task_search(task_id,project_id,title,description,acceptance)
SELECT id,project_id,title,description,acceptance_json FROM tasks;
CREATE TRIGGER task_search_insert AFTER INSERT ON tasks BEGIN
    INSERT INTO task_search(task_id,project_id,title,description,acceptance)
    VALUES(new.id,new.project_id,new.title,new.description,new.acceptance_json);
END;
CREATE TRIGGER task_search_update AFTER UPDATE OF title,description,acceptance_json ON tasks BEGIN
    DELETE FROM task_search WHERE task_id=old.id;
    INSERT INTO task_search(task_id,project_id,title,description,acceptance)
    VALUES(new.id,new.project_id,new.title,new.description,new.acceptance_json);
END;
CREATE TRIGGER task_search_delete AFTER DELETE ON tasks BEGIN
    DELETE FROM task_search WHERE task_id=old.id;
END;
