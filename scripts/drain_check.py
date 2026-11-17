#!/usr/bin/env python3
"""Refuse a production deploy while the coordinator database shows live work.

Usage: drain_check.py DATABASE --task ID --attempt ID --job ID --reservation ID

The deploy passes the ids of its own task, attempt, job and reservation; those rows
never block it. The database is opened read-only (SQLite `mode=ro` plus
`query_only`), so the check cannot change it. Exit status: 0 drained, 1 not drained,
2 the check could not run (a missing file, an unknown schema or a failed read; it
never reports "drained" without having read every category).

Every category prints its count; every blocking row prints its ids and state, never
task text, summaries, evidence or credentials.

A job blocks only by the service's own rule (crates/server/src/jobs.rs): its state is
not `succeeded`, `failed` or `not_started`, and `reconciled_at IS NULL`. A reconciled
job keeps its last state forever, because only the job's own reporter may post a
terminal observation, so state alone cannot say a job is gone.
"""

import argparse
from pathlib import Path
import sqlite3
import sys
from typing import NamedTuple

DRAINED, BLOCKED, FAILED = 0, 1, 2


class Category(NamedTuple):
    name: str
    description: str
    # Selects (id columns..., state) of the blocking rows; every `?` binds the
    # deploy's task, attempt, job and reservation ids by the names in `binds`.
    sql: str
    binds: tuple[str, ...]
    labels: tuple[str, ...]


CATEGORIES = (
    Category(
        "attempts",
        "another task's active attempt",
        "SELECT a.id, a.task_id, a.state FROM attempts a"
        " WHERE a.state = 'active' AND a.task_id != ? AND a.id != ? ORDER BY a.created_at, a.id",
        ("task", "attempt"),
        ("attempt", "task"),
    ),
    Category(
        "jobs",
        "an unreconciled job whose state is not succeeded, failed or not_started",
        "SELECT j.id, j.attempt_id, j.state FROM jobs j"
        " WHERE j.state NOT IN ('succeeded', 'failed', 'not_started') AND j.reconciled_at IS NULL"
        " AND j.id != ? ORDER BY j.created_at, j.id",
        ("job",),
        ("job", "attempt"),
    ),
    Category(
        "reservations",
        "a held reservation",
        "SELECT r.id, r.attempt_id, r.state FROM reservations r"
        " WHERE r.state = 'held' AND r.id != ? ORDER BY r.created_at, r.id",
        ("reservation",),
        ("reservation", "attempt"),
    ),
    Category(
        "held_integrations",
        "a held integration target",
        "SELECT h.id, h.activity_id, h.state FROM integration_holds h"
        " JOIN workflow_activities w ON w.id = h.activity_id"
        " WHERE h.state = 'held' AND w.subject_task_id != ? AND w.activity_task_id != ?"
        " ORDER BY h.acquired_at, h.id",
        ("task", "task"),
        ("hold", "activity"),
    ),
    Category(
        "active_integrations",
        "an active integration activity",
        "SELECT w.id, w.subject_task_id, w.state FROM workflow_activities w"
        " WHERE w.kind = 'integration' AND w.state = 'active'"
        " AND w.subject_task_id != ? AND w.activity_task_id != ? ORDER BY w.created_at, w.id",
        ("task", "task"),
        ("activity", "subject_task"),
    ),
    Category(
        "recovery_required",
        "a recovery-required activity",
        "SELECT w.id, w.subject_task_id, w.state FROM workflow_activities w"
        " WHERE w.state = 'recovery_required'"
        " AND w.subject_task_id != ? AND w.activity_task_id != ? ORDER BY w.created_at, w.id",
        ("task", "task"),
        ("activity", "subject_task"),
    ),
    Category(
        "publication_intents",
        "a publication intent with no reconciliation and no published result",
        "SELECT p.activity_id, p.submission_id, 'unreconciled' FROM publication_intents p"
        " JOIN workflow_activities w ON w.id = p.activity_id"
        " WHERE NOT EXISTS (SELECT 1 FROM publication_reconciliations r WHERE r.activity_id = p.activity_id)"
        " AND NOT EXISTS (SELECT 1 FROM integration_results i"
        "   WHERE i.activity_id = p.activity_id AND i.publication_state = 'published')"
        " AND w.subject_task_id != ? AND w.activity_task_id != ? ORDER BY p.created_at, p.activity_id",
        ("task", "task"),
        ("activity", "submission"),
    ),
)


def open_read_only(path: Path) -> sqlite3.Connection:
    if not path.is_file():
        raise sqlite3.Error("the database path is not a file")
    connection = sqlite3.connect(f"{path.resolve().as_uri()}?mode=ro", uri=True, timeout=10)
    connection.execute("PRAGMA query_only = ON")
    return connection


def check(connection: sqlite3.Connection, own: dict[str, str]) -> list[tuple[Category, list[tuple]]]:
    """Read every category in one snapshot; any failed read raises."""
    results = []
    connection.execute("BEGIN")
    try:
        for category in CATEGORIES:
            rows = connection.execute(category.sql, [own[name] for name in category.binds]).fetchall()
            results.append((category, rows))
    finally:
        connection.rollback()
    return results


def report(results: list[tuple[Category, list[tuple]]], out) -> int:
    blocked = 0
    for category, rows in results:
        print(f"{category.name}: {len(rows)} ({category.description})", file=out)
        blocked += len(rows)
    for category, rows in results:
        for row in rows:
            fields = " ".join(f"{label}={value}" for label, value in zip(category.labels, row))
            print(f"blocking {category.name}: {fields} state={row[-1]}", file=out)
    print("drained" if not blocked else f"not drained: {blocked} blocking row(s)", file=out)
    return BLOCKED if blocked else DRAINED


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("database", type=Path, help="path to the coordinator SQLite database")
    for name in ("task", "attempt", "job", "reservation"):
        parser.add_argument(f"--{name}", required=True, help=f"the deploy's own {name} id")
    args = parser.parse_args(argv)
    own = {name: getattr(args, name) for name in ("task", "attempt", "job", "reservation")}
    try:
        connection = open_read_only(args.database)
        try:
            results = check(connection, own)
        finally:
            connection.close()
    except sqlite3.Error as error:
        print(f"drain check could not read the database: {error}", file=sys.stderr)
        return FAILED
    return report(results, sys.stdout)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
