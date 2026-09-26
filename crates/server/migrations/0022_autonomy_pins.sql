-- Submissions pin a digest of the judged task fields (title, description,
-- acceptance criteria, kind) instead of whole task and policy revisions, so
-- priority edits and unrelated policy changes no longer strand candidates.
-- Existing rows are backfilled at startup from their pinned task revision.
ALTER TABLE submissions ADD COLUMN task_digest TEXT;

-- The required-check roster revision captured when publication is intended.
-- Integration results and finalization validate against it, never the current
-- roster; NULL on legacy intents means the submission's pinned roster.
ALTER TABLE publication_intents ADD COLUMN roster_revision INTEGER;

-- A contributor's proposed acceptance-criteria change ({old,new,rationale}) and
-- each reviewer's explicit decision on it (accepted or rejected).
ALTER TABLE submissions ADD COLUMN ac_amendment_json TEXT;
ALTER TABLE review_decisions ADD COLUMN amendment_decision TEXT;
