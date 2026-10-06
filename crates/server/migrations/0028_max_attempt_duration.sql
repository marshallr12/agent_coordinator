-- Service-enforced maximum attempt duration (coordinator task 5c9f15eb, plan
-- §2.3 "Progress-gated renewal"). 0, the default, turns the limit off; any
-- other value stops renewals from extending an attempt past
-- created_at + max_attempt_seconds.
ALTER TABLE projects ADD COLUMN max_attempt_seconds INTEGER NOT NULL DEFAULT 0
    CHECK(max_attempt_seconds = 0 OR max_attempt_seconds BETWEEN 600 AND 604800);
