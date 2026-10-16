-- The daily spend caps (last 24 hours) a supervisor host enforces per role,
-- as it reports them, so the console can show cost per day against the caps.
-- One row per (project, host); the newest report replaces the row. Millionths
-- of a US dollar; NULL when the host reported no cap for that role (0 on the
-- host means no cap, and is stored as 0).
CREATE TABLE host_caps (
    project_id TEXT NOT NULL REFERENCES projects(id),
    host TEXT NOT NULL CHECK(length(host) BETWEEN 1 AND 253),
    impl_daily_micros INTEGER CHECK(impl_daily_micros IS NULL OR impl_daily_micros >= 0),
    rev_daily_micros INTEGER CHECK(rev_daily_micros IS NULL OR rev_daily_micros >= 0),
    reported_by TEXT NOT NULL REFERENCES principals(id),
    reported_at INTEGER NOT NULL,
    PRIMARY KEY(project_id, host)
);
