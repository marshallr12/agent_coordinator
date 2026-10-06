-- Idle agent sessions are closed by storage maintenance (coordinator task
-- 601d1601). Each run counts the sessions it closed; the reporter index lets
-- the idle check find a session's live job reporters without a table scan.
ALTER TABLE maintenance_runs ADD COLUMN sessions_closed INTEGER NOT NULL DEFAULT 0
    CHECK(sessions_closed >= 0);
CREATE INDEX reporters_session ON reporters(session_id);
