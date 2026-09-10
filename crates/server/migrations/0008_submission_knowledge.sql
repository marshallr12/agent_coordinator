-- A submission keeps its original lesson revision even when the current lesson is corrected.
CREATE TABLE submission_knowledge (
    project_id TEXT NOT NULL,
    submission_id TEXT NOT NULL,
    knowledge_id TEXT NOT NULL,
    knowledge_revision INTEGER NOT NULL,
    PRIMARY KEY(submission_id,knowledge_id),
    FOREIGN KEY(project_id,submission_id) REFERENCES submissions(project_id,id),
    FOREIGN KEY(project_id,knowledge_id) REFERENCES knowledge_records(source_project_id,id),
    FOREIGN KEY(knowledge_id,knowledge_revision) REFERENCES knowledge_revisions(knowledge_id,revision)
);
CREATE TRIGGER submission_knowledge_immutable_update BEFORE UPDATE ON submission_knowledge
BEGIN SELECT RAISE(ABORT, 'submission knowledge references are immutable'); END;
CREATE TRIGGER submission_knowledge_immutable_delete BEFORE DELETE ON submission_knowledge
BEGIN SELECT RAISE(ABORT, 'submission knowledge references are immutable'); END;
