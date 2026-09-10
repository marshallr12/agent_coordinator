#!/usr/bin/env python3
"""Verify the offline restore stage used by the disposable capacity exercise."""

import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time


RESTORE_TIMEOUT_SECONDS = 2700
RECOVERY_TARGET_SECONDS = 3600
ADDRESS_SPACE_BYTES = 4 * 1024**3
MAX_REPORT_BYTES = 64 * 1024
HISTORICAL_TITLE_GLOB = "Historical capacity fixture " + "[0-9]" * 6
RESTORE_REASON = (
    "Disposable capacity restore-stage verification after stopping original service."
)


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def _command_environment() -> dict[str, str]:
    return {
        key: value
        for key, value in os.environ.items()
        if not key.upper().startswith(("AGENT_COORDINATOR_", "COORDINATOR_"))
    }


def _run_restore(
    server: Path, snapshot: Path, destination: Path, affinity: list[int]
) -> dict:
    taskset = shutil.which("taskset")
    prlimit = shutil.which("prlimit")
    _require(taskset is not None and prlimit is not None, "Restore-stage limits are unavailable.")
    command = [
        taskset,
        "--cpu-list",
        ",".join(str(cpu) for cpu in affinity),
        prlimit,
        f"--as={ADDRESS_SPACE_BYTES}",
        "--",
        str(server),
        "restore",
        "--snapshot",
        str(snapshot),
        "--destination",
        str(destination),
        "--reason",
        RESTORE_REASON,
    ]
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        try:
            completed = subprocess.run(
                command,
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                env=_command_environment(),
                timeout=RESTORE_TIMEOUT_SECONDS,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired):
            raise RuntimeError("Restore-stage command did not complete; output withheld.") from None
        _require(
            completed.returncode == 0,
            f"Restore-stage command failed with status {completed.returncode}; output withheld.",
        )
        size = stdout.tell()
        _require(size <= MAX_REPORT_BYTES, "Restore-stage report exceeded its fixed bound.")
        stdout.seek(0)
        try:
            report = json.loads(stdout.read().decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise RuntimeError("Restore-stage command returned an invalid report.") from None
    _require(isinstance(report, dict), "Restore-stage command returned an invalid report.")
    return report


def _readonly_restore_checks(
    database: Path, restored: dict, destination: Path, expected_history: int, deadline: float
) -> dict:
    _require(database.is_file() and not database.is_symlink(), "Restored database is missing.")
    try:
        reported_destination = Path(restored["data_directory"])
        reported_database = Path(restored["database_path"])
        artifact_count = restored["artifact_count"]
        artifact_bytes = restored["artifact_bytes"]
        authority = restored["authority"]
        snapshot_id = restored["snapshot_id"]
    except (KeyError, TypeError):
        raise RuntimeError("Restore-stage command returned an incomplete report.") from None
    _require(
        reported_destination == destination and reported_database == database,
        "Restore-stage command reported an unexpected destination.",
    )
    _require(
        isinstance(snapshot_id, str) and bool(snapshot_id),
        "Restore-stage command returned an invalid snapshot identity.",
    )
    _require(
        type(artifact_count) is int
        and artifact_count >= 0
        and type(artifact_bytes) is int
        and artifact_bytes >= 0,
        "Restore-stage command returned invalid artifact totals.",
    )
    _require(
        isinstance(authority, dict)
        and authority.get("coordination_state") == "restore_reconciliation"
        and isinstance(authority.get("restore_id"), str)
        and bool(authority["restore_id"]),
        "Restore-stage authority invalidation was not reported.",
    )

    artifact_root = destination / "coordinator.sqlite3.artifacts"
    blob_root = artifact_root / "blobs"
    _require(
        artifact_root.is_dir()
        and not artifact_root.is_symlink()
        and blob_root.is_dir()
        and not blob_root.is_symlink()
        and (artifact_root / "staging").is_dir()
        and not (artifact_root / "staging").is_symlink(),
        "Restored artifact store is missing.",
    )
    blob_count = 0
    blob_bytes = 0
    for blob in blob_root.glob("*/*.blob"):
        _require(
            blob.is_file() and not blob.is_symlink(),
            "Restored artifact store contains an invalid blob.",
        )
        blob_count += 1
        blob_bytes += blob.stat().st_size
    _require(
        blob_count == artifact_count and blob_bytes == artifact_bytes,
        "Restored artifact totals do not match the restore report.",
    )

    uri = database.resolve().as_uri() + "?mode=ro&immutable=1"
    try:
        connection = sqlite3.connect(uri, uri=True, timeout=5)
        connection.set_progress_handler(lambda: int(time.monotonic() >= deadline), 10_000)
        integrity_ok = connection.execute("PRAGMA integrity_check(1)").fetchone() == ("ok",)
        foreign_keys_ok = connection.execute("PRAGMA foreign_key_check").fetchone() is None
        history_count = connection.execute(
            "SELECT count(*) FROM tasks WHERE lifecycle='canceled' "
            "AND title GLOB ? AND description='Synthetic historical load record.'",
            (HISTORICAL_TITLE_GLOB,),
        ).fetchone()[0]
        service = connection.execute(
            "SELECT authority_epoch,coordination_state,restore_id,restored_at "
            "FROM service_state WHERE singleton=1"
        ).fetchone()
        _require(service is not None, "Restored coordination state is missing.")
        authority_epoch, coordination_state, restore_id, restored_at = service
        restore_source = connection.execute(
            "SELECT snapshot_id FROM restore_runs WHERE id=?", (restore_id,)
        ).fetchone()
        credentials = connection.execute("SELECT count(*) FROM credentials").fetchone()[0]
        browser_sessions = connection.execute("SELECT count(*) FROM browser_sessions").fetchone()[0]
        agent_sessions = connection.execute("SELECT count(*) FROM agent_sessions").fetchone()[0]
        human_accounts = connection.execute(
            "SELECT count(*) FROM principals WHERE kind='human'"
        ).fetchone()[0]
        credentials_revoked = connection.execute(
            "SELECT count(*)=0 FROM credentials WHERE revoked_at IS NULL"
        ).fetchone()[0]
        browser_sessions_revoked = connection.execute(
            "SELECT count(*)=0 FROM browser_sessions WHERE revoked_at IS NULL"
        ).fetchone()[0]
        agent_sessions_closed = connection.execute(
            "SELECT count(*)=0 FROM agent_sessions WHERE closed_at IS NULL"
        ).fetchone()[0]
        human_accounts_disabled = connection.execute(
            "SELECT count(*)=0 FROM principals WHERE kind='human' AND disabled_at IS NULL"
        ).fetchone()[0]
        no_active_attempts = connection.execute(
            "SELECT count(*)=0 FROM attempts WHERE state='active'"
        ).fetchone()[0]
        attempts = connection.execute("SELECT count(*) FROM attempts").fetchone()[0]
        reporters_expired = connection.execute(
            "SELECT count(*)=0 FROM reporters WHERE expires_at>? OR renew_until>?",
            (restored_at, restored_at),
        ).fetchone()[0]
        integration_authorizations_invalidated = connection.execute(
            "SELECT count(*)=0 FROM integration_authorizations WHERE invalidated_at IS NULL"
        ).fetchone()[0]
        old_receipts_invalidated = connection.execute(
            "SELECT count(*)=0 FROM mutation_receipts WHERE authority_epoch=?",
            (authority_epoch,),
        ).fetchone()[0]
    except (sqlite3.Error, OSError):
        raise RuntimeError("Restored database verification failed.") from None
    finally:
        if "connection" in locals():
            connection.close()

    checks = {
        "integrity_ok": integrity_ok,
        "foreign_keys_ok": foreign_keys_ok,
        "history_count_ok": history_count == expected_history,
        "coordination_paused": coordination_state == "restore_reconciliation"
        and restore_id == authority["restore_id"]
        and restore_source == (snapshot_id,)
        and type(restored_at) is int,
        "credentials_revoked": bool(credentials_revoked) and credentials > 0,
        "browser_sessions_revoked": bool(browser_sessions_revoked)
        and browser_sessions > 0,
        "agent_sessions_closed": bool(agent_sessions_closed) and agent_sessions > 0,
        "human_accounts_disabled": bool(human_accounts_disabled) and human_accounts > 0,
        "no_active_attempts": bool(no_active_attempts) and attempts > 0,
        "reporters_expired": bool(reporters_expired),
        "integration_authorizations_invalidated": bool(
            integration_authorizations_invalidated
        ),
        "old_receipts_invalidated": bool(old_receipts_invalidated),
    }
    _require(all(checks.values()), "Restored database did not pass authority checks.")
    return {
        **checks,
        "coordination_state": coordination_state,
        "database_bytes": database.stat().st_size,
        "artifact_count": artifact_count,
        "artifact_bytes": artifact_bytes,
        "historical_tasks": history_count,
        "credential_count": credentials,
        "browser_session_count": browser_sessions,
        "agent_session_count": agent_sessions,
        "human_account_count": human_accounts,
        "attempt_count": attempts,
    }


def restore_stage(
    server: Path,
    snapshot: Path,
    destination: Path,
    affinity: list[int],
    expected_history: int,
) -> dict:
    """Restore one snapshot offline and verify the paused, invalidated stage."""
    _require(sys.platform.startswith("linux"), "Restore-stage verification requires Linux.")
    _require(server.is_file() and not server.is_symlink(), "Restore-stage server is invalid.")
    _require(snapshot.is_dir() and not snapshot.is_symlink(), "Restore-stage snapshot is invalid.")
    _require(destination.is_absolute(), "Restore-stage destination must be absolute.")
    server = server.resolve()
    snapshot = snapshot.resolve()
    destination = destination.absolute()
    _require(
        destination.parent.is_dir() and not os.path.lexists(destination),
        "Restore-stage destination must be an absent child of an existing directory.",
    )
    _require(
        bool(affinity)
        and len(affinity) <= 64
        and len(set(affinity)) == len(affinity)
        and all(type(cpu) is int and cpu >= 0 for cpu in affinity)
        and set(affinity).issubset(os.sched_getaffinity(0)),
        "Restore-stage CPU affinity is invalid.",
    )
    _require(
        type(expected_history) is int and 0 <= expected_history <= 100_000,
        "Restore-stage historical count is invalid.",
    )

    started = time.monotonic()
    restored = _run_restore(server, snapshot, destination, affinity)
    database = destination / "coordinator.sqlite3"
    checks = _readonly_restore_checks(
        database, restored, destination, expected_history, started + RECOVERY_TARGET_SECONDS
    )
    seconds = time.monotonic() - started
    _require(seconds <= RECOVERY_TARGET_SECONDS, "Restore-stage verification exceeded one hour.")
    return {
        "measurement": "restore_stage",
        "passed": True,
        "full_service_recovery": False,
        "seconds": round(seconds, 3),
        "target_seconds": RECOVERY_TARGET_SECONDS,
        "within_target": True,
        **checks,
    }
