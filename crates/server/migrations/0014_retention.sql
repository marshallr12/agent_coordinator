-- Payload retention never releases an idempotency key or removes workflow
-- provenance.  The remaining receipt columns are the permanent tombstone.
ALTER TABLE mutation_receipts ADD COLUMN compacted_at INTEGER;
CREATE INDEX mutation_receipts_payload_retention
ON mutation_receipts(compacted_at,created_at,principal_id,operation,key);

-- Only summaries from strictly redundant intermediate producer heartbeats may
-- be cleared.  Sequence and request identity remain permanent.
ALTER TABLE job_observations ADD COLUMN payload_compacted_at INTEGER;
CREATE INDEX job_observations_payload_retention
ON job_observations(payload_compacted_at,observed_at,reporter_id,sequence);

-- One bounded maintenance invocation is auditable without creating another
-- idempotent API mutation or a project-scoped semantic event.
CREATE TABLE maintenance_runs (
    id TEXT PRIMARY KEY NOT NULL,
    cutoff_at INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    completed_at INTEGER,
    batch_size INTEGER NOT NULL CHECK(batch_size BETWEEN 1 AND 1000),
    max_batches INTEGER NOT NULL CHECK(max_batches BETWEEN 1 AND 100),
    batches INTEGER NOT NULL DEFAULT 0 CHECK(batches BETWEEN 0 AND max_batches),
    receipt_results_compacted INTEGER NOT NULL DEFAULT 0 CHECK(receipt_results_compacted >= 0),
    observation_payloads_compacted INTEGER NOT NULL DEFAULT 0 CHECK(observation_payloads_compacted >= 0),
    receipts_remaining INTEGER CHECK(receipts_remaining IN (0,1)),
    observations_remaining INTEGER CHECK(observations_remaining IN (0,1)),
    state TEXT NOT NULL DEFAULT 'running' CHECK(state IN ('running','complete')),
    CHECK((state='running' AND completed_at IS NULL)
       OR (state='complete' AND completed_at IS NOT NULL
           AND receipts_remaining IS NOT NULL AND observations_remaining IS NOT NULL))
);
CREATE INDEX maintenance_runs_started ON maintenance_runs(started_at,id);
