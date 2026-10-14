-- Supervisor telemetry: launch cost rows and end-to-end canary results that
-- supervisor hosts report per project. The hosts' local ledgers
-- (costs.jsonl, e2e-canary.jsonl) stay the durable source; these tables only
-- hold what was reported. `row_id` is the reporter's stable identity of one
-- ledger row, so a retried or re-imported row is stored once per
-- (project, host). `task_id`, `session_id` and `attempt_id` are the
-- reporter's claims and are not foreign keys: a row may outlive or predate
-- the record it names.
CREATE TABLE cost_rows (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    host TEXT NOT NULL CHECK(length(host) BETWEEN 1 AND 253),
    row_id TEXT NOT NULL CHECK(length(row_id) BETWEEN 1 AND 128),
    role TEXT NOT NULL CHECK(role IN ('impl','rev')),
    harness TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL CHECK(input_tokens >= 0),
    cached_input_tokens INTEGER NOT NULL CHECK(cached_input_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK(output_tokens >= 0),
    -- Millionths of a US dollar; NULL when the launch was unpriced.
    usd_micros INTEGER CHECK(usd_micros IS NULL OR usd_micros >= 0),
    usd_estimated INTEGER NOT NULL DEFAULT 0 CHECK(usd_estimated IN (0,1)),
    canary INTEGER NOT NULL DEFAULT 0 CHECK(canary IN (0,1)),
    task_id TEXT,
    session_id TEXT,
    attempt_id TEXT,
    occurred_at INTEGER NOT NULL,
    reported_by TEXT NOT NULL REFERENCES principals(id),
    reported_at INTEGER NOT NULL,
    UNIQUE(project_id, host, row_id)
);
CREATE INDEX cost_rows_project_time ON cost_rows(project_id, occurred_at);
CREATE INDEX cost_rows_project_task ON cost_rows(project_id, task_id);

CREATE TABLE canary_results (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    host TEXT NOT NULL CHECK(length(host) BETWEEN 1 AND 253),
    row_id TEXT NOT NULL CHECK(length(row_id) BETWEEN 1 AND 128),
    harness TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK(outcome IN ('ok','failed','timeout')),
    duration_seconds REAL NOT NULL CHECK(duration_seconds >= 0),
    detail TEXT NOT NULL,
    task_id TEXT,
    -- JSON array of the harnesses that served the task, or NULL.
    served_by_json TEXT CHECK(served_by_json IS NULL OR json_valid(served_by_json)),
    occurred_at INTEGER NOT NULL,
    reported_by TEXT NOT NULL REFERENCES principals(id),
    reported_at INTEGER NOT NULL,
    UNIQUE(project_id, host, row_id)
);
CREATE INDEX canary_results_project_time ON canary_results(project_id, occurred_at);
