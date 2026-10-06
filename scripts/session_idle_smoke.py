"""Exercise idle-session closing by real maintenance and native CLI recovery.

The disposable database is backdated directly; no existing service state is used.
"""
import json
import sqlite3
import subprocess

DAY_MS = 86_400_000


def backdate_session(database, session_id, days):
    """Moves every recorded touch of one session `days` into the past."""
    with sqlite3.connect(database, timeout=10) as db:
        shift = days * DAY_MS
        db.execute("UPDATE agent_sessions SET created_at=created_at-? WHERE id=?", (shift, session_id))
        db.execute("UPDATE instruction_acknowledgments SET created_at=created_at-? WHERE session_id=?",
                   (shift, session_id))
        db.execute("UPDATE attempts SET created_at=created_at-?1,last_heartbeat_at=last_heartbeat_at-?1,"
                   "last_progress_at=last_progress_at-?1,ended_at=ended_at-?1 WHERE session_id=?2",
                   (shift, session_id))


def run_maintenance(server_options):
    """Runs the server's one-shot maintenance with the default idle period."""
    result = subprocess.run(server_options + ["maintenance"], text=True, capture_output=True, timeout=60)
    assert result.returncode == 0, f"maintenance failed: {result.stderr}"
    return json.loads(result.stdout)


def exercise_session_idle(database, server_options, cli, index, session_id):
    """An idle harness session is closed by maintenance; connect replaces it."""
    backdate_session(database, session_id, 30)
    report = run_maintenance(server_options)
    assert report["idle_sessions_closed"] == 1, f"maintenance closed {report['idle_sessions_closed']} sessions."
    assert report["limits"]["session_idle_days"] == 7
    with sqlite3.connect(database, timeout=10) as db:
        closed = db.execute("SELECT closed_at FROM agent_sessions WHERE id=?", (session_id,)).fetchone()[0]
        event = db.execute("SELECT data_json FROM events WHERE kind='agent_session_closed' AND record_id=?",
                           (session_id,)).fetchone()
    assert closed is not None and json.loads(event[0])["reason"] == "idle", "The idle session was not closed."
    reconnected = cli(index, "connect")["data"]
    assert reconnected["replaced_closed_session_id"] == session_id, "connect did not replace the closed session."
    assert reconnected["session"]["id"] != session_id and reconnected["session"]["closed_at"] is None
    assert reconnected["orientation"]["instructions_complete"]
    assert "items" in cli(index, "tasks", "list")["data"], "The replacement session cannot read tasks."
