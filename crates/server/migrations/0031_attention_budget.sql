-- Attention budget. A decision may carry a recommendation and be marked
-- reversible; once its current cycle has gone unanswered for 24 hours the
-- service answers it with the recommendation and flags the answer as timed
-- out, attributed to the principal that asked. Tasks may declare the paths
-- they touch, and the files humans shipped are recorded, so `next` can skip
-- a task whose paths overlap a human change from the last 24 hours.
ALTER TABLE decisions ADD COLUMN recommendation TEXT
    CHECK(recommendation IS NULL OR length(recommendation) BETWEEN 1 AND 2048);
ALTER TABLE decisions ADD COLUMN reversible INTEGER NOT NULL DEFAULT 0
    CHECK(reversible IN (0,1));
ALTER TABLE decision_answers ADD COLUMN timed_out INTEGER NOT NULL DEFAULT 0
    CHECK(timed_out IN (0,1));

CREATE TABLE task_paths (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    path TEXT NOT NULL CHECK(length(path) BETWEEN 1 AND 1024),
    PRIMARY KEY(task_id,path),
    FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,id)
);

CREATE TABLE human_shipped_files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL REFERENCES projects(id),
    commit_sha TEXT NOT NULL CHECK(length(commit_sha) BETWEEN 7 AND 64),
    path TEXT NOT NULL CHECK(length(path) BETWEEN 1 AND 1024),
    recorded_by TEXT NOT NULL REFERENCES principals(id),
    shipped_at INTEGER NOT NULL
);
CREATE INDEX human_shipped_recent ON human_shipped_files(project_id,shipped_at,path);
