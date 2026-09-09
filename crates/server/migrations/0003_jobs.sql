CREATE TABLE resources (
    id TEXT PRIMARY KEY NOT NULL,
    key TEXT NOT NULL UNIQUE CHECK(length(key) BETWEEN 1 AND 255),
    capacity INTEGER NOT NULL CHECK(capacity BETWEEN 1 AND 1000),
    description TEXT NOT NULL CHECK(length(description) <= 4096),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL
);

CREATE TABLE reservations (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'held' CHECK(state IN ('held','released','resolved')),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    released_at INTEGER,
    released_by TEXT REFERENCES principals(id),
    release_reason TEXT,
    resolved_at INTEGER,
    resolved_by TEXT REFERENCES principals(id),
    resolution_reason TEXT,
    resolution_evidence TEXT,
    UNIQUE(project_id,id),
    UNIQUE(project_id,attempt_id,id),
    FOREIGN KEY(project_id,attempt_id) REFERENCES attempts(project_id,id)
);
CREATE UNIQUE INDEX one_held_reservation_per_attempt
    ON reservations(attempt_id) WHERE state='held';
CREATE INDEX reservations_project_created
    ON reservations(project_id,created_at,id);

CREATE TABLE reservation_items (
    reservation_id TEXT NOT NULL REFERENCES reservations(id),
    resource_id TEXT NOT NULL REFERENCES resources(id),
    units INTEGER NOT NULL CHECK(units BETWEEN 1 AND 1000),
    PRIMARY KEY(reservation_id,resource_id)
);
CREATE INDEX reservation_items_resource ON reservation_items(resource_id);

CREATE TABLE jobs (
    id TEXT PRIMARY KEY NOT NULL,
    producer_id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    runner_instance_id TEXT NOT NULL,
    workstation_id TEXT NOT NULL,
    label TEXT NOT NULL CHECK(length(label) BETWEEN 1 AND 255),
    source_revision TEXT NOT NULL CHECK(length(source_revision) BETWEEN 1 AND 255),
    source_tree TEXT NOT NULL CHECK(length(source_tree) BETWEEN 1 AND 255),
    reservation_id TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'registered'
        CHECK(state IN ('registered','running','succeeded','failed','unknown','not_started')),
    last_sequence INTEGER NOT NULL DEFAULT 0 CHECK(last_sequence >= 0),
    last_observed_at INTEGER,
    pid INTEGER,
    process_started_at TEXT,
    exit_code INTEGER,
    inputs_unchanged INTEGER,
    summary TEXT NOT NULL DEFAULT '',
    reconciled_at INTEGER,
    reconciled_by TEXT REFERENCES principals(id),
    reconciliation_reason TEXT,
    reconciliation_evidence TEXT,
    created_at INTEGER NOT NULL,
    UNIQUE(project_id,id),
    FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,id),
    FOREIGN KEY(project_id,attempt_id) REFERENCES attempts(project_id,id),
    FOREIGN KEY(project_id,attempt_id,reservation_id)
        REFERENCES reservations(project_id,attempt_id,id)
);
CREATE INDEX jobs_project_created ON jobs(project_id,created_at,id);
CREATE INDEX jobs_attempt ON jobs(attempt_id);
CREATE INDEX jobs_reservation ON jobs(reservation_id);

CREATE TABLE reporters (
    id TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id),
    principal_id TEXT NOT NULL REFERENCES principals(id),
    credential_id TEXT NOT NULL REFERENCES credentials(id),
    session_id TEXT NOT NULL REFERENCES agent_sessions(id),
    proof_hash TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    renew_until INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX reporters_parent ON reporters(principal_id,credential_id);

CREATE TABLE job_observations (
    reporter_id TEXT NOT NULL REFERENCES reporters(id),
    sequence INTEGER NOT NULL CHECK(sequence > 0),
    request_hash TEXT NOT NULL,
    producer_id TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK(state IN ('registered','running','succeeded','failed','unknown','not_started')),
    pid INTEGER,
    process_started_at TEXT,
    exit_code INTEGER,
    inputs_unchanged INTEGER CHECK(inputs_unchanged IN (0,1)),
    summary TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    PRIMARY KEY(reporter_id,sequence)
);
