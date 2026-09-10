-- Objectives wrap ordinary general tasks and add revisioned parent/child grouping.
CREATE TABLE objectives (
    project_id TEXT NOT NULL,
    task_id TEXT PRIMARY KEY NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(project_id,task_id),
    FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,id)
);

CREATE TABLE objective_revisions (
    objective_task_id TEXT NOT NULL REFERENCES objectives(task_id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    children_json TEXT NOT NULL CHECK(json_valid(children_json) AND json_type(children_json)='array'),
    actor_id TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(objective_task_id,revision)
);

CREATE TRIGGER objective_revisions_immutable_update
BEFORE UPDATE ON objective_revisions
BEGIN
    SELECT RAISE(ABORT, 'objective revisions are immutable');
END;

CREATE TRIGGER objective_revisions_immutable_delete
BEFORE DELETE ON objective_revisions
BEGIN
    SELECT RAISE(ABORT, 'objective revisions are immutable');
END;

CREATE TRIGGER objective_projection_requires_revision
BEFORE UPDATE ON objectives
BEGIN
    SELECT CASE WHEN
        new.project_id != old.project_id OR
        new.task_id != old.task_id OR
        new.created_by != old.created_by OR
        new.created_at != old.created_at OR
        new.revision != old.revision + 1 OR
        NOT EXISTS (
            SELECT 1 FROM objective_revisions r
            WHERE r.objective_task_id=new.task_id
              AND r.revision=new.revision
              AND r.created_at=new.updated_at
        )
    THEN RAISE(ABORT, 'objective projection requires its immutable next revision') END;
END;

CREATE TRIGGER objectives_immutable_delete
BEFORE DELETE ON objectives
BEGIN
    SELECT RAISE(ABORT, 'objectives are retained');
END;

CREATE TABLE objective_children (
    project_id TEXT NOT NULL,
    objective_task_id TEXT NOT NULL,
    child_task_id TEXT NOT NULL UNIQUE,
    required INTEGER NOT NULL CHECK(required IN (0,1)),
    position INTEGER NOT NULL CHECK(position >= 0),
    PRIMARY KEY(objective_task_id,child_task_id),
    CHECK(objective_task_id != child_task_id),
    FOREIGN KEY(project_id,objective_task_id) REFERENCES objectives(project_id,task_id),
    FOREIGN KEY(project_id,child_task_id) REFERENCES tasks(project_id,id)
);
CREATE INDEX objective_children_order
    ON objective_children(objective_task_id,position,child_task_id);
