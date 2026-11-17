#!/usr/bin/env python3
"""Tests for the deploy drain check, on disposable databases built from the real migrations."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import shutil
import sqlite3
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("drain_check", HERE / "drain_check.py")
drain = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(drain)
MIGRATIONS = sorted((HERE.parent / "crates" / "server" / "migrations").glob("*.sql"))

OWN = {"task": "own-task", "attempt": "own-attempt", "job": "own-job", "reservation": "own-res"}
SECRET = "SECRET-TASK-TEXT"


def new_database(path):
    connection = sqlite3.connect(path)
    for migration in MIGRATIONS:
        connection.executescript(migration.read_text())
    return connection


def insert(connection, table, **values):
    columns = ",".join(values)
    marks = ",".join("?" * len(values))
    connection.execute(f"INSERT INTO {table}({columns}) VALUES({marks})", list(values.values()))
    connection.commit()


def attempt(connection, id, task, state="active"):
    insert(connection, "attempts", id=id, project_id="p", task_id=task, owner_id="o", session_id="s",
           generation=1, state=state, mode="work", expires_at=1, last_heartbeat_at=1,
           last_progress_at=1, created_at=1)


def reservation(connection, id, attempt_id, state="held"):
    insert(connection, "reservations", id=id, project_id="p", attempt_id=attempt_id, generation=1,
           state=state, created_by="o", created_at=1)


def job(connection, id, state="registered", reconciled=None, pid=None, sequence=0, attempt_id="a-old"):
    insert(connection, "jobs", id=id, producer_id="prod-" + id, project_id="p", task_id="t-old",
           attempt_id=attempt_id, generation=1, runner_instance_id="r", workstation_id="w", label="l",
           source_revision="x", source_tree="y", reservation_id="r-old", state=state, pid=pid,
           last_sequence=sequence, reconciled_at=reconciled, created_at=1)


def activity(connection, id, subject, task, kind="integration", state="active"):
    insert(connection, "workflow_activities", id=id, project_id="p", subject_task_id=subject,
           submission_id="sub-" + id, activity_task_id=task, kind=kind, state=state, created_at=1)


TEMPLATE = None


def setUpModule():
    """Apply every migration once; each test copies the result."""
    global TEMPLATE, _template_dir
    _template_dir = tempfile.TemporaryDirectory()
    TEMPLATE = Path(_template_dir.name) / "template.sqlite3"
    new_database(TEMPLATE).close()


def tearDownModule():
    _template_dir.cleanup()


class DrainCheckTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "coordinator.sqlite3"
        shutil.copyfile(TEMPLATE, self.path)
        self.db = sqlite3.connect(self.path)
        self.addCleanup(self.db.close)
        # A finished other task: nothing here may block.
        attempt(self.db, "a-old", "t-old", "submitted")
        reservation(self.db, "r-old", "a-old", "released")

    def run_check(self, *extra):
        out, err = io.StringIO(), io.StringIO()
        argv = [str(self.path)]
        for name, value in OWN.items():
            argv += [f"--{name}", value]
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = drain.main(argv + list(extra))
        return code, out.getvalue(), err.getvalue()

    def assert_drained(self):
        code, out, _ = self.run_check()
        self.assertEqual(code, 0, out)
        self.assertIn("drained", out)
        return out

    def assert_blocked(self, category, *ids):
        code, out, _ = self.run_check()
        self.assertEqual(code, 1, out)
        self.assertIn(f"{category}: 1 ", out)
        line = next(l for l in out.splitlines() if l.startswith(f"blocking {category}:"))
        for value in ids:
            self.assertIn(value, line)
        self.assertEqual(sum(l.startswith("blocking ") for l in out.splitlines()), 1, out)
        return out

    def test_empty_database_is_drained_and_prints_every_count(self):
        out = self.assert_drained()
        for category in drain.CATEGORIES:
            self.assertIn(f"{category.name}: 0 ", out)

    def test_reconciled_registered_job_without_pid_does_not_block(self):
        job(self.db, "j-never", state="registered", reconciled=5, pid=None, sequence=0)
        self.assert_drained()

    def test_reconciled_job_keeps_non_terminal_state_without_blocking(self):
        job(self.db, "j-run", state="running", reconciled=5, pid=7, sequence=3)
        job(self.db, "j-unk", state="unknown", reconciled=5)
        self.assert_drained()

    def test_terminal_jobs_do_not_block(self):
        for state in ("succeeded", "failed", "not_started"):
            job(self.db, "j-" + state, state=state)
        self.assert_drained()

    def test_unreconciled_registered_running_and_unknown_jobs_block(self):
        for state, extra in (("registered", {}), ("running", {"pid": 9, "sequence": 2}), ("unknown", {})):
            with self.subTest(state=state):
                job(self.db, "j-" + state, state=state, **extra)
                out = self.assert_blocked("jobs", "j-" + state, f"state={state}")
                self.assertIn("job=j-" + state, out)
                self.db.execute("DELETE FROM jobs")
                self.db.commit()

    def test_own_job_does_not_block(self):
        job(self.db, OWN["job"], state="running", pid=1, sequence=1)
        self.assert_drained()

    def test_another_tasks_active_attempt_blocks_but_own_does_not(self):
        attempt(self.db, OWN["attempt"], OWN["task"])
        self.assert_drained()
        attempt(self.db, "a-other", "t-other")
        self.assert_blocked("attempts", "a-other", "t-other", "state=active")

    def test_ended_attempts_do_not_block(self):
        for index, state in enumerate(("released", "blocked", "expired", "canceled", "submitted")):
            attempt(self.db, f"a-{state}", f"t-{index}", state)
        self.assert_drained()

    def test_held_reservation_blocks_but_own_and_released_do_not(self):
        reservation(self.db, OWN["reservation"], "a-old")
        self.assert_drained()
        reservation(self.db, "r-other", "a-other-res")
        self.assert_blocked("reservations", "r-other", "state=held")

    def test_held_integration_blocks(self):
        activity(self.db, "w-held", "t-sub", "t-act", state="completed")
        insert(self.db, "integration_holds", id="h-1", activity_id="w-held", canonical_repository_key="k",
               target_branch="main", state="held", acquired_by="o", acquired_at=1)
        self.assert_blocked("held_integrations", "h-1", "w-held", "state=held")

    def test_released_integration_hold_does_not_block(self):
        activity(self.db, "w-done", "t-sub", "t-act", state="completed")
        insert(self.db, "integration_holds", id="h-1", activity_id="w-done", canonical_repository_key="k",
               target_branch="main", state="released", acquired_by="o", acquired_at=1)
        self.assert_drained()

    def test_active_integration_blocks_but_review_does_not(self):
        activity(self.db, "w-review", "t-sub", "t-rev", kind="agent_review")
        self.assert_drained()
        activity(self.db, "w-int", "t-sub2", "t-act2")
        self.assert_blocked("active_integrations", "w-int", "t-sub2", "state=active")

    def test_recovery_required_activity_blocks(self):
        activity(self.db, "w-rec", "t-sub", "t-act", state="recovery_required")
        self.assert_blocked("recovery_required", "w-rec", "state=recovery_required")

    def test_unreconciled_publication_intent_blocks(self):
        activity(self.db, "w-pub", "t-sub", "t-act", state="completed")
        self.add_intent("w-pub")
        self.assert_blocked("publication_intents", "w-pub", "sub-w-pub", "state=unreconciled")

    def add_intent(self, activity_id):
        insert(self.db, "publication_intents", activity_id=activity_id, submission_id="sub-" + activity_id,
               attempt_id="a-old", observed_target_revision="o", observed_target_tree="ot",
               result_revision="r", result_tree="rt", created_by="o", created_at=1)

    def test_reconciled_or_published_intent_does_not_block(self):
        activity(self.db, "w-rec", "t-sub", "t-act", state="completed")
        self.add_intent("w-rec")
        insert(self.db, "publication_reconciliations", activity_id="w-rec", submission_id="sub-w-rec",
               disposition="published", observed_target_revision="r", observed_target_tree="rt",
               evidence="e", actor_id="o", created_at=2)
        activity(self.db, "w-pub", "t-sub2", "t-act2", state="completed")
        self.add_intent("w-pub")
        insert(self.db, "integration_results", activity_id="w-pub", submission_id="sub-w-pub",
               attempt_id="a-old", publication_state="published", observed_target_revision="o",
               result_revision="r", result_tree="rt", check_job_ids_json="[]", summary="s",
               reported_by="o", created_at=2)
        self.assert_drained()

    def test_own_task_activities_do_not_block(self):
        activity(self.db, "w-int", OWN["task"], "t-act", state="active")
        activity(self.db, "w-rec", OWN["task"], "t-act2", state="recovery_required")
        activity(self.db, "w-held", "t-sub", OWN["task"], state="completed")
        insert(self.db, "integration_holds", id="h-1", activity_id="w-held", canonical_repository_key="k",
               target_branch="main", state="held", acquired_by="o", acquired_at=1)
        activity(self.db, "w-pub", OWN["task"], "t-act3", state="completed")
        self.add_intent("w-pub")
        self.assert_drained()

    def test_every_blocking_row_is_reported_with_every_count(self):
        attempt(self.db, "a-other", "t-other")
        job(self.db, "j-live")
        reservation(self.db, "r-other", "a-other")
        activity(self.db, "w-rec", "t-sub", "t-act", state="recovery_required")
        code, out, _ = self.run_check()
        self.assertEqual(code, 1)
        for text in ("attempts: 1 ", "jobs: 1 ", "reservations: 1 ", "recovery_required: 1 ",
                     "held_integrations: 0 ", "active_integrations: 0 ", "publication_intents: 0 ",
                     "not drained: 4 blocking row(s)"):
            self.assertIn(text, out)

    def test_output_has_no_task_text(self):
        insert(self.db, "tasks", id="t-other", project_id="p", title=SECRET, description=SECRET,
               acceptance_json="[]", kind="code", priority=1, lifecycle="open", created_at=1,
               ready_since=1)
        attempt(self.db, "a-other", "t-other")
        code, out, err = self.run_check()
        self.assertEqual(code, 1)
        self.assertNotIn(SECRET, out + err)

    def test_database_is_opened_read_only(self):
        connection = drain.open_read_only(self.path)
        self.addCleanup(connection.close)
        with self.assertRaises(sqlite3.OperationalError):
            connection.execute("DELETE FROM attempts")
        before = self.path.read_bytes()
        self.run_check()
        self.assertEqual(self.path.read_bytes(), before)

    def test_unreadable_database_fails_closed(self):
        os.remove(self.path)
        code, out, err = self.run_check()
        self.assertEqual((code, out), (2, ""))
        self.assertIn("could not read", err)

    def test_unknown_schema_fails_closed(self):
        self.db.execute("DROP TABLE publication_intents")
        self.db.commit()
        code, out, err = self.run_check()
        self.assertEqual((code, out), (2, ""))
        self.assertIn("could not read", err)


if __name__ == "__main__":
    unittest.main()
