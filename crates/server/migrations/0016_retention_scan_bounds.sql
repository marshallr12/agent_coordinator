-- Record every old observation considered by retention so ineligible semantic
-- evidence is not rescanned under the writer lock on every maintenance run.
ALTER TABLE job_observations ADD COLUMN retention_checked_at INTEGER;
CREATE INDEX job_observations_retention_scan
ON job_observations(retention_checked_at,observed_at,reporter_id,sequence);

ALTER TABLE maintenance_runs
ADD COLUMN observation_rows_inspected INTEGER NOT NULL DEFAULT 0
CHECK(observation_rows_inspected >= 0);
