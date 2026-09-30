#!/usr/bin/env python3
"""Owner-run, read-only cutover preflight on the coordinator database.

Pause new work before running; repeat immediately before changing ownership.
Zero counts are necessary, not a lock against subsequent work.
"""
import argparse
import json
import sqlite3
from pathlib import Path

QUERIES = {
    'publication_intents': "SELECT count(*) FROM publication_intents pi LEFT JOIN integration_results ir ON ir.activity_id=pi.activity_id LEFT JOIN publication_reconciliations pr ON pr.activity_id=pi.activity_id WHERE (ir.activity_id IS NULL OR ir.publication_state='uncertain') AND pr.activity_id IS NULL",
    'holds': "SELECT count(*) FROM integration_holds WHERE state='held'",
    'reservations': "SELECT count(*) FROM reservations WHERE state='held'",
    'jobs': "SELECT count(*) FROM jobs WHERE state IN ('registered','running','unknown')",
    'recovery': "SELECT count(*) FROM workflow_activities WHERE state='recovery_required'",
    'active_integrations': "SELECT count(*) FROM workflow_activities WHERE kind='integration' AND state='active'",
    # Recovery is also projected from expired/revoked current attempts, not
    # necessarily persisted as workflow_activities.state='recovery_required'.
    # Require complete drain rather than duplicating the service's projection.
    'current_attempts': "SELECT count(*) FROM tasks WHERE current_attempt_id IS NOT NULL",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--database', required=True, type=Path)
    args = parser.parse_args()
    with sqlite3.connect(args.database.resolve().as_uri() + '?mode=ro', uri=True) as db:
        db.execute('PRAGMA query_only=ON')
        db.execute('BEGIN')
        counts = {name: db.execute(query).fetchone()[0] for name, query in QUERIES.items()}
    print(json.dumps(counts, sort_keys=True))
    return int(any(counts.values()))


if __name__ == '__main__':
    raise SystemExit(main())
