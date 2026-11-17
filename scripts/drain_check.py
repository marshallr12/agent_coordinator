#!/usr/bin/env python3
"""Refuse a production deploy while the coordinator database shows live work.

Usage: drain_check.py DATABASE --task ID --attempt ID --job ID --reservation ID

The deploy passes the ids of its own task, attempt, job and reservation; those rows
never block it. The database is opened read-only (SQLite `mode=ro` plus
`query_only`), so the check cannot change it. Exit status: 0 drained, 1 not drained,
2 the check could not run (a missing file, an unknown schema or a failed read; it
never reports "drained" without having read every category).

Every category prints its count; every blocking or parked row prints its ids and state,
never task text, summaries, evidence or credentials.

A `recovery_required` activity is parked, not blocking: it runs nothing, holds no lease
and survives a service restart unchanged (owner decision of 2026-10-28), so swapping the
service cannot harm it. It is still read, counted and printed, on a `parked` line instead
of a `blocking` line, and it never makes the status 1. Every other category blocks.

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
    # A parked category is reported but never makes the exit status 1.
    parked: bool = False


CATEGORIES = (
    Category(
        "attempts",
        "another task's current attempt, whatever its state",
        # An expired or revoked current attempt still needs recovery, so the task's
        # pointer blocks, not the attempt's `active` state (deploy/agentc/integrator-preflight.py).
        "SELECT t.current_attempt_id, t.id, COALESCE(a.state, 'missing') FROM tasks t"
        " LEFT JOIN attempts a ON a.id = t.current_attempt_id"
        " WHERE t.current_attempt_id IS NOT NULL AND t.id != ? AND t.current_attempt_id != ?"
        " ORDER BY t.created_at, t.id",
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
        "a parked recovery-required activity; reported, does not block",
        "SELECT w.id, w.subject_task_id, w.state FROM workflow_activities w"
        " WHERE w.state = 'recovery_required'"
        " AND w.subject_task_id != ? AND w.activity_task_id != ? ORDER BY w.created_at, w.id",
        ("task", "task"),
        ("activity", "subject_task"),
        parked=True,
    ),
    Category(
        "publication_intents",
        "a publication intent with no reconciliation whose result is missing or uncertain",
        "SELECT p.activity_id, p.submission_id, COALESCE(i.publication_state, 'no_result')"
        " FROM publication_intents p"
        " JOIN workflow_activities w ON w.id = p.activity_id"
        " LEFT JOIN integration_results i ON i.activity_id = p.activity_id"
        " LEFT JOIN publication_reconciliations r ON r.activity_id = p.activity_id"
        " WHERE (i.activity_id IS NULL OR i.publication_state = 'uncertain') AND r.activity_id IS NULL"
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
    blocked = parked = 0
    for category, rows in results:
        print(f"{category.name}: {len(rows)} ({category.description})", file=out)
        if category.parked:
            parked += len(rows)
        else:
            blocked += len(rows)
    for category, rows in results:
        for row in rows:
            fields = " ".join(f"{label}={value}" for label, value in zip(category.labels, row))
            kind = "parked" if category.parked else "blocking"
            print(f"{kind} {category.name}: {fields} state={row[-1]}", file=out)
    summary = "drained" if not blocked else f"not drained: {blocked} blocking row(s)"
    if parked:
        summary += f"; {parked} parked row(s) do not block"
    print(summary, file=out)
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
