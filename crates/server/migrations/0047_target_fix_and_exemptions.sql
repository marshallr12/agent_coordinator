-- Fix tasks the service opens itself when the integrator reports that a
-- required check fails on the target branch (a `fix_target` report with
-- verdict `target_failing`). One row per task, naming the target and check it
-- repairs, so a second report for the same repository, target branch and
-- check does not open a second task while one is unfinished.
CREATE TABLE integrator_fix_tasks (
    task_id TEXT PRIMARY KEY NOT NULL REFERENCES tasks(id),
    project_id TEXT NOT NULL REFERENCES projects(id),
    report_id TEXT NOT NULL REFERENCES integrator_reports(id),
    repository_url TEXT NOT NULL,
    target_branch TEXT NOT NULL,
    check_name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX integrator_fix_tasks_target
    ON integrator_fix_tasks(project_id,repository_url,target_branch,check_name);

-- A required check that fails on a result's R although every input path the
-- roster declares for it is identical in R and in the target tip X, while the
-- check fails on X too. The integrator attests the facts; push authority
-- verifies them again before it counts the check as exempt. One row per
-- (result, check identity).
CREATE TABLE integrator_exemptions (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    result_id TEXT NOT NULL REFERENCES integrator_results(id),
    identity TEXT NOT NULL,
    check_name TEXT NOT NULL,
    r TEXT NOT NULL,
    x TEXT NOT NULL,
    report_id TEXT NOT NULL REFERENCES integrator_reports(id),
    inputs_json TEXT NOT NULL CHECK(json_valid(inputs_json)),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    UNIQUE(result_id,identity)
);
CREATE INDEX integrator_exemptions_created ON integrator_exemptions(project_id,created_at);
