-- Per-session lookups for the connected-sessions list (coordinator task
-- 33a3a8dd): find a session's attempts in a project without scanning the
-- project's whole attempt history, and enumerate open sessions without
-- scanning closed ones.
CREATE INDEX attempts_session ON attempts(session_id,project_id);
CREATE INDEX agent_sessions_open ON agent_sessions(created_at) WHERE closed_at IS NULL;
