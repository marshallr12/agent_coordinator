-- The integrator's latest poll of the required checks on a result's R. A
-- receipt records a completed check run; this snapshot records the runs that
-- are still queued or in progress, so the task list can tell active
-- verification from waiting. One row per result, replaced by each poll and
-- bound to R (head_sha). Only the observation time and the bounded list of
-- unfinished runs are kept; completed runs stay in integrator_receipts.
CREATE TABLE integrator_check_progress (
    result_id TEXT PRIMARY KEY NOT NULL REFERENCES integrator_results(id),
    head_sha TEXT NOT NULL,
    runs_json TEXT NOT NULL CHECK(json_valid(runs_json)),
    observed_at INTEGER NOT NULL
);
