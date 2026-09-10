ALTER TABLE policy_revisions ADD COLUMN provenance TEXT NOT NULL DEFAULT ''
    CHECK(length(provenance) <= 4096 AND instr(provenance, char(0)) = 0);

CREATE TRIGGER policy_revisions_immutable_update
BEFORE UPDATE ON policy_revisions
BEGIN
    SELECT RAISE(ABORT, 'policy revisions are immutable');
END;

CREATE TRIGGER policy_revisions_immutable_delete
BEFORE DELETE ON policy_revisions
BEGIN
    SELECT RAISE(ABORT, 'policy revisions are immutable');
END;

CREATE TABLE knowledge_records (
    id TEXT PRIMARY KEY NOT NULL,
    source_project_id TEXT NOT NULL REFERENCES projects(id),
    collection TEXT NOT NULL CHECK(collection IN ('project','shared')),
    current_revision INTEGER NOT NULL CHECK(current_revision > 0),
    kind TEXT NOT NULL CHECK(kind IN ('lesson','fact','rejected_approach','checkpoint')),
    status TEXT NOT NULL CHECK(status IN ('observed','validated','deprecated','superseded')),
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 255),
    body TEXT NOT NULL CHECK(length(body) BETWEEN 1 AND 32768),
    scope_json TEXT NOT NULL CHECK(json_valid(scope_json) AND json_type(scope_json)='object'),
    tags_json TEXT NOT NULL CHECK(json_valid(tags_json) AND json_type(tags_json)='array'),
    applicability TEXT NOT NULL CHECK(length(applicability) <= 4096),
    provenance_json TEXT NOT NULL CHECK(json_valid(provenance_json) AND json_type(provenance_json)='object'),
    superseded_by_id TEXT REFERENCES knowledge_records(id),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(source_project_id,id),
    CHECK(id != superseded_by_id)
);
CREATE INDEX knowledge_project_current
    ON knowledge_records(source_project_id,updated_at,id);
CREATE INDEX knowledge_shared_current
    ON knowledge_records(collection,updated_at,id);

CREATE TABLE knowledge_revisions (
    knowledge_id TEXT NOT NULL REFERENCES knowledge_records(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    kind TEXT NOT NULL CHECK(kind IN ('lesson','fact','rejected_approach','checkpoint')),
    status TEXT NOT NULL CHECK(status IN ('observed','validated','deprecated','superseded')),
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 255),
    body TEXT NOT NULL CHECK(length(body) BETWEEN 1 AND 32768),
    scope_json TEXT NOT NULL CHECK(json_valid(scope_json) AND json_type(scope_json)='object'),
    tags_json TEXT NOT NULL CHECK(json_valid(tags_json) AND json_type(tags_json)='array'),
    applicability TEXT NOT NULL CHECK(length(applicability) <= 4096),
    provenance_json TEXT NOT NULL CHECK(json_valid(provenance_json) AND json_type(provenance_json)='object'),
    superseded_by_id TEXT REFERENCES knowledge_records(id),
    actor_id TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(knowledge_id,revision),
    CHECK(knowledge_id != superseded_by_id)
);

CREATE TRIGGER knowledge_revisions_immutable_update
BEFORE UPDATE ON knowledge_revisions
BEGIN
    SELECT RAISE(ABORT, 'knowledge revisions are immutable');
END;

CREATE TRIGGER knowledge_revisions_immutable_delete
BEFORE DELETE ON knowledge_revisions
BEGIN
    SELECT RAISE(ABORT, 'knowledge revisions are immutable');
END;

CREATE TRIGGER knowledge_projection_requires_revision
BEFORE UPDATE ON knowledge_records
BEGIN
    SELECT CASE WHEN
        new.id != old.id OR
        new.source_project_id != old.source_project_id OR
        new.collection != old.collection OR
        new.kind != old.kind OR
        new.created_by != old.created_by OR
        new.created_at != old.created_at OR
        new.current_revision != old.current_revision + 1 OR
        NOT EXISTS (
            SELECT 1 FROM knowledge_revisions r
            WHERE r.knowledge_id=new.id
              AND r.revision=new.current_revision
              AND r.kind=new.kind
              AND r.status=new.status
              AND r.title=new.title
              AND r.body=new.body
              AND r.scope_json=new.scope_json
              AND r.tags_json=new.tags_json
              AND r.applicability=new.applicability
              AND r.provenance_json=new.provenance_json
              AND r.superseded_by_id IS new.superseded_by_id
              AND r.created_at=new.updated_at
        )
    THEN RAISE(ABORT, 'knowledge projection requires its immutable next revision') END;
END;

CREATE TRIGGER knowledge_records_immutable_delete
BEFORE DELETE ON knowledge_records
BEGIN
    SELECT RAISE(ABORT, 'knowledge records are retained');
END;

CREATE TABLE knowledge_feedback (
    id TEXT PRIMARY KEY NOT NULL,
    knowledge_id TEXT NOT NULL REFERENCES knowledge_records(id),
    revision INTEGER NOT NULL,
    actor_id TEXT NOT NULL REFERENCES principals(id),
    useful INTEGER NOT NULL CHECK(useful IN (0,1)),
    comment TEXT NOT NULL CHECK(length(comment) <= 2048),
    created_at INTEGER NOT NULL,
    FOREIGN KEY(knowledge_id,revision) REFERENCES knowledge_revisions(knowledge_id,revision)
);
CREATE INDEX knowledge_feedback_record
    ON knowledge_feedback(knowledge_id,created_at,id);

CREATE TRIGGER knowledge_feedback_immutable_update
BEFORE UPDATE ON knowledge_feedback
BEGIN
    SELECT RAISE(ABORT, 'knowledge feedback is immutable');
END;

CREATE TRIGGER knowledge_feedback_immutable_delete
BEFORE DELETE ON knowledge_feedback
BEGIN
    SELECT RAISE(ABORT, 'knowledge feedback is immutable');
END;

CREATE VIRTUAL TABLE knowledge_search USING fts5(
    record_id UNINDEXED,
    source_project_id UNINDEXED,
    collection UNINDEXED,
    title,
    body,
    tags,
    applicability,
    tokenize='unicode61'
);

CREATE TRIGGER knowledge_search_insert
AFTER INSERT ON knowledge_records
BEGIN
    INSERT INTO knowledge_search(record_id,source_project_id,collection,title,body,tags,applicability)
    VALUES(new.id,new.source_project_id,new.collection,new.title,new.body,new.tags_json,new.applicability);
END;

CREATE TRIGGER knowledge_search_update
AFTER UPDATE OF current_revision ON knowledge_records
BEGIN
    DELETE FROM knowledge_search WHERE record_id=old.id;
    INSERT INTO knowledge_search(record_id,source_project_id,collection,title,body,tags,applicability)
    VALUES(new.id,new.source_project_id,new.collection,new.title,new.body,new.tags_json,new.applicability);
END;

CREATE VIRTUAL TABLE task_search USING fts5(
    task_id UNINDEXED,
    project_id UNINDEXED,
    title,
    description,
    acceptance,
    tokenize='unicode61'
);

INSERT INTO task_search(task_id,project_id,title,description,acceptance)
SELECT id,project_id,title,description,acceptance_json FROM tasks;

CREATE TRIGGER task_search_insert
AFTER INSERT ON tasks
BEGIN
    INSERT INTO task_search(task_id,project_id,title,description,acceptance)
    VALUES(new.id,new.project_id,new.title,new.description,new.acceptance_json);
END;

CREATE TRIGGER task_search_update
AFTER UPDATE OF title,description,acceptance_json ON tasks
BEGIN
    DELETE FROM task_search WHERE task_id=old.id;
    INSERT INTO task_search(task_id,project_id,title,description,acceptance)
    VALUES(new.id,new.project_id,new.title,new.description,new.acceptance_json);
END;

CREATE TRIGGER task_search_delete
AFTER DELETE ON tasks
BEGIN
    DELETE FROM task_search WHERE task_id=old.id;
END;

CREATE TABLE decisions (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    question TEXT NOT NULL CHECK(length(question) BETWEEN 1 AND 4096),
    options_json TEXT NOT NULL CHECK(json_valid(options_json) AND json_type(options_json)='array'),
    rationale TEXT NOT NULL CHECK(length(rationale) BETWEEN 1 AND 8192),
    required_actor TEXT NOT NULL CHECK(required_actor IN ('human','agent','either')),
    current_generation INTEGER NOT NULL DEFAULT 1 CHECK(current_generation > 0),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    UNIQUE(project_id,id)
);
CREATE INDEX decisions_project_created ON decisions(project_id,created_at,id);

CREATE TABLE decision_cycles (
    decision_id TEXT NOT NULL REFERENCES decisions(id),
    generation INTEGER NOT NULL CHECK(generation > 0),
    policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
    environment TEXT NOT NULL CHECK(length(environment) <= 2048),
    conditions TEXT NOT NULL CHECK(length(conditions) <= 4096),
    expires_at INTEGER,
    rationale TEXT NOT NULL CHECK(length(rationale) <= 8192),
    opened_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(decision_id,generation)
);

CREATE TABLE decision_affected_tasks (
    decision_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    task_revision INTEGER NOT NULL CHECK(task_revision > 0),
    PRIMARY KEY(decision_id,generation,task_id),
    FOREIGN KEY(decision_id,generation) REFERENCES decision_cycles(decision_id,generation),
    FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,id),
    FOREIGN KEY(task_id,task_revision) REFERENCES task_revisions(task_id,revision)
);
CREATE INDEX decision_tasks_current
    ON decision_affected_tasks(project_id,task_id,task_revision,decision_id,generation);

CREATE TABLE decision_answers (
    decision_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    disposition TEXT NOT NULL CHECK(disposition IN ('allow','deny','defer')),
    answer TEXT NOT NULL CHECK(length(answer) BETWEEN 1 AND 2048),
    rationale TEXT NOT NULL CHECK(length(rationale) BETWEEN 1 AND 8192),
    actor_id TEXT NOT NULL REFERENCES principals(id),
    actor_session_id TEXT,
    conditions_confirmed INTEGER NOT NULL CHECK(conditions_confirmed IN (0,1)),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(decision_id,generation),
    FOREIGN KEY(decision_id,generation) REFERENCES decision_cycles(decision_id,generation)
);

CREATE TRIGGER decisions_projection_requires_cycle
BEFORE UPDATE ON decisions
BEGIN
    SELECT CASE WHEN
        new.id != old.id OR
        new.project_id != old.project_id OR
        new.question != old.question OR
        new.options_json != old.options_json OR
        new.rationale != old.rationale OR
        new.required_actor != old.required_actor OR
        new.created_by != old.created_by OR
        new.created_at != old.created_at OR
        new.current_generation != old.current_generation + 1 OR
        NOT EXISTS (
            SELECT 1 FROM decision_cycles c
            WHERE c.decision_id=new.id AND c.generation=new.current_generation
        )
    THEN RAISE(ABORT, 'decision projection requires its immutable next cycle') END;
END;

CREATE TRIGGER decisions_immutable_delete
BEFORE DELETE ON decisions
BEGIN
    SELECT RAISE(ABORT, 'decisions are retained');
END;

CREATE TRIGGER decision_affected_tasks_immutable_update
BEFORE UPDATE ON decision_affected_tasks
BEGIN
    SELECT RAISE(ABORT, 'decision task scopes are immutable');
END;

CREATE TRIGGER decision_affected_tasks_immutable_delete
BEFORE DELETE ON decision_affected_tasks
BEGIN
    SELECT RAISE(ABORT, 'decision task scopes are immutable');
END;

CREATE TRIGGER decision_cycles_immutable_update
BEFORE UPDATE ON decision_cycles
BEGIN
    SELECT RAISE(ABORT, 'decision cycles are immutable');
END;

CREATE TRIGGER decision_cycles_immutable_delete
BEFORE DELETE ON decision_cycles
BEGIN
    SELECT RAISE(ABORT, 'decision cycles are immutable');
END;

CREATE TRIGGER decision_answers_immutable_update
BEFORE UPDATE ON decision_answers
BEGIN
    SELECT RAISE(ABORT, 'decision answers are immutable');
END;

CREATE TRIGGER decision_answers_immutable_delete
BEFORE DELETE ON decision_answers
BEGIN
    SELECT RAISE(ABORT, 'decision answers are immutable');
END;
