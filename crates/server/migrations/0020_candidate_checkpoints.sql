-- Old code submissions remain immutable history. A NULL checkpoint marks the
-- pre-checkpoint format and requires explicit reopening/re-submission.
ALTER TABLE submissions ADD COLUMN candidate_ref TEXT;

CREATE INDEX submissions_candidate_ref
    ON submissions(canonical_repository_key, candidate_ref)
    WHERE candidate_ref IS NOT NULL;
