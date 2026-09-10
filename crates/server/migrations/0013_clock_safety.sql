-- Service time is an authority input. Persist its high-water mark so a wall
-- clock rollback cannot revive a deadline after either a process or host restart.
CREATE TABLE clock_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton=1),
    last_safe_time_ms INTEGER NOT NULL DEFAULT 0 CHECK(last_safe_time_ms >= 0),
    status TEXT NOT NULL CHECK(status IN ('ready','clock_reconciliation')),
    incident_id TEXT,
    observed_wall_time_ms INTEGER,
    detected_at INTEGER,
    CHECK(status='ready' OR
          (incident_id IS NOT NULL AND observed_wall_time_ms IS NOT NULL AND detected_at IS NOT NULL))
);
INSERT INTO clock_state(singleton,last_safe_time_ms,status)
VALUES(1,0,'ready');

CREATE TABLE clock_incidents (
    id TEXT PRIMARY KEY NOT NULL,
    observed_wall_time_ms INTEGER NOT NULL,
    high_water_time_ms INTEGER NOT NULL,
    detected_at INTEGER NOT NULL,
    recovered_at INTEGER,
    recovery_reason TEXT,
    recovered_by TEXT REFERENCES principals(id),
    recovery_kind TEXT CHECK(recovery_kind IN ('authenticated_admin','host_operator')),
    CHECK((recovered_at IS NULL AND recovery_reason IS NULL AND recovered_by IS NULL AND recovery_kind IS NULL)
       OR (recovered_at IS NOT NULL AND recovery_reason IS NOT NULL AND recovery_kind IS NOT NULL))
);

CREATE TABLE clock_reconciliation_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    incident_id TEXT NOT NULL REFERENCES clock_incidents(id),
    kind TEXT NOT NULL CHECK(kind IN ('rollback_detected','clock_reconciled')),
    initiator_kind TEXT NOT NULL CHECK(initiator_kind IN ('service','authenticated_admin','host_operator')),
    actor_id TEXT REFERENCES principals(id),
    observed_wall_time_ms INTEGER NOT NULL,
    high_water_time_ms INTEGER NOT NULL,
    reason TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 2000),
    created_at INTEGER NOT NULL
);
CREATE INDEX clock_reconciliation_events_incident
ON clock_reconciliation_events(incident_id,seq);
