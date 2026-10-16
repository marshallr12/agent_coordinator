#!/usr/bin/env python3
"""Tests for the pilot renewal helper: a fake client and a controlled clock, no live calls."""
import contextlib
import datetime
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("pilot_renew", Path(__file__).with_name("pilot_renew.py"))
pilot = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pilot)

ATTEMPT = "attempt-1"
TASK = "task-1"
SESSION = "session-1"
CLAIM = pilot.Claim(ATTEMPT, 3, TASK, SESSION, 900.0)
EPOCH = datetime.datetime(2026, 10, 16, 10, 0, 0, tzinfo=datetime.timezone.utc)
SECRET = "SECRET-DO-NOT-LOG"


def iso(seconds):
    return (EPOCH + datetime.timedelta(seconds=seconds)).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def task_answer(now, expires, **changes):
    attempt = {"id": ATTEMPT, "generation": 3, "session_id": SESSION, "state": "active", "expires_at": iso(expires)}
    data = {"current_attempt_id": ATTEMPT, "generation": 3, "attempts": [attempt]}
    for key, value in changes.items():
        (attempt if key.startswith("attempt_") else data)[key.removeprefix("attempt_")] = value
    return {"data": data, "server_time": iso(now)}


class World:
    """A controlled clock plus a scripted client; every call is recorded."""

    def __init__(self):
        self.now = 0.0
        self.lease = 3600.0
        self.calls = []
        self.reads = []  # scripted outcomes for `tasks show`; empty means a normal answer
        self.renews = []  # scripted outcomes for `renew`; empty means success extending the lease
        self.retries = []  # scripted outcomes for `retry`
        self.logs = []
        self.alerts = []

    def run(self, command, **kwargs):
        self.calls.append(command)
        words = command[5:-1]  # after the client, --repo-config X and --session Y; before --json
        assert command[-1] == "--json" and kwargs["timeout"] > 0
        name = words[0]
        if name == "tasks":
            script, extend = self.reads, False
        elif name == "renew":
            script, extend = self.renews, True
        elif name == "retry":
            script, extend = self.retries, True
        else:
            raise AssertionError(f"helper issued a forbidden client command: {name}")
        step = script.pop(0) if script else "ok"
        if step == "ok":
            if extend:
                self.lease = self.now + 3600.0
                return subprocess.CompletedProcess(command, 0, json.dumps({"data": {"receipt": SECRET}}), "")
            return subprocess.CompletedProcess(command, 0, json.dumps(task_answer(self.now, self.lease)), "")
        if step == "replay":
            return subprocess.CompletedProcess(command, 0, json.dumps({"data": {"receipt": SECRET}}), "")
        if step == "timeout":
            raise subprocess.TimeoutExpired(command, kwargs["timeout"])
        if step == "garbage":
            return subprocess.CompletedProcess(command, 0, SECRET + " <html>", "")
        if isinstance(step, dict):
            return subprocess.CompletedProcess(command, 0, json.dumps(step), "")
        # any other string is an error code from a failing client
        return subprocess.CompletedProcess(
            command, 1, json.dumps({"error": {"code": step, "message": SECRET}}), SECRET
        )

    def sleep(self, seconds):
        self.now += seconds

    def renewer(self, **options):
        options.setdefault("retry_delay", 2.0)
        return pilot.Renewer(
            pilot.Cli(["cli", "--repo-config", "repo", "--session", "s"], self.run),
            CLAIM,
            clock=lambda: self.now,
            sleep=self.sleep,
            log=self.logs.append,
            alert=self.alerts.append,
            **options,
        )

    def names(self):
        return [c[5] for c in self.calls]

    def text(self):
        return "\n".join(self.logs + self.alerts)


class RenewalTests(unittest.TestCase):
    def test_healthy_cycles_verify_each_renewal(self):
        world = World()
        self.assertEqual(world.renewer().run(max_cycles=2), 0)
        self.assertEqual(world.names(), ["tasks", "renew", "tasks", "renew", "tasks"])
        self.assertEqual(world.now, 900.0)
        self.assertEqual(sum("verified active" in line for line in world.logs), 3)
        self.assertEqual(world.alerts, [])

    def test_transient_read_failure_after_renew_recovers_with_read_only_retries(self):
        world = World()
        world.reads = ["ok", "transport_error", "timeout", "garbage"]
        self.assertEqual(world.renewer().run(max_cycles=1), 0)
        self.assertEqual(world.names(), ["tasks", "renew", "tasks", "tasks", "tasks", "tasks"])
        self.assertEqual(world.now, 2.0 + 4.0 + 8.0)
        self.assertEqual(sum("fresh ownership read failed" in line for line in world.logs), 3)
        self.assertEqual(world.alerts, [])
        self.assertNotIn(SECRET, world.text())
        self.assertIn("code=transport_error", world.text())
        self.assertIn("client timed out", world.text())

    def test_persistent_read_failure_stops_before_the_verified_deadline_with_an_alert(self):
        world = World()
        world.reads = ["ok"] + ["unavailable"] * 500
        self.assertEqual(world.renewer().run(max_cycles=1), 1)
        self.assertLess(world.now, 3600.0)  # never past the lease verified by the startup read
        reads = [n for n in world.names() if n == "tasks"]
        self.assertGreater(len(reads), 10)
        self.assertEqual(world.names().count("renew"), 1)  # read-only retries: no further mutation
        self.assertEqual(len(world.alerts), 1)
        self.assertTrue(world.alerts[0].startswith("STOPPED: no fresh ownership read succeeded"))
        self.assertIn("no new claim attempted", world.alerts[0])
        self.assertNotIn(SECRET, world.text())

    def test_retry_delay_is_capped(self):
        world = World()
        world.reads = ["ok"] + ["unavailable"] * 8 + ["ok"]
        world.renewer(retry_max_delay=5.0).run(max_cycles=1)
        waits = [int(line.split(" in ")[1].split("s,")[0]) for line in world.logs if "read-only retry" in line]
        self.assertEqual(waits, [2, 4, 5, 5, 5, 5, 5, 5])

    def test_old_receipt_does_not_renew_authority(self):
        # The saved mutation was replayed: the receipt succeeds but the lease never moved.
        world = World()
        world.renews = ["replay"]
        world.now = 0.0
        renewer = world.renewer(min_remaining=3000.0)
        world.reads = [task_answer(0, 3500)]  # startup lease ends at 3500
        world.now = 700.0  # a replay leaves 2800s, under the 3000s margin
        self.assertEqual(renewer.run(max_cycles=1), 1)
        self.assertIn("renewal did not take effect", world.alerts[0])
        self.assertNotIn("verified active", world.logs[-1])

    def test_backwards_expiry_stops(self):
        world = World()
        world.reads = [task_answer(0, 3600), task_answer(0, 3000)]
        self.assertEqual(world.renewer().run(max_cycles=1), 1)
        self.assertIn("lease expiry moved backwards", world.alerts[0])

    def test_changed_ownership_stops_at_once_without_retrying_reads(self):
        changes = {
            "current_attempt_id": "attempt-2",
            "generation": 4,
            "attempt_generation": 4,
            "attempt_session_id": "someone-else",
            "attempt_state": "released",
        }
        for key, value in changes.items():
            with self.subTest(change=key):
                world = World()
                world.reads = ["ok", task_answer(0, 7200, **{key: value})]
                self.assertEqual(world.renewer().run(max_cycles=1), 1)
                self.assertEqual(world.names(), ["tasks", "renew", "tasks"])
                self.assertIn("ownership changed", world.alerts[0])
                self.assertNotIn("someone-else", world.text())

    def test_attempt_missing_from_the_task_stops(self):
        world = World()
        answer = task_answer(0, 7200)
        answer["data"]["attempts"] = []
        world.reads = ["ok", answer]
        self.assertEqual(world.renewer().run(max_cycles=1), 1)
        self.assertIn("no longer listed", world.alerts[0])

    def test_expiry_stops_all_work(self):
        # The startup read shows an already expired lease: nothing is renewed.
        world = World()
        world.reads = [task_answer(0, -1)]
        self.assertEqual(world.renewer().run(), 1)
        self.assertEqual(world.names(), ["tasks"])
        self.assertIn("lease expired", world.alerts[0])

        # The verified lease runs out while sleeping: the helper does not renew it.
        world = World()
        world.reads = [task_answer(0, 600), task_answer(0, 600 + 960 + 1)]
        claim = CLAIM._replace(renew_after=2000.0)
        renewer = world.renewer()
        renewer.claim = claim
        self.assertEqual(renewer.run(max_cycles=3), 1)
        self.assertEqual(world.names().count("renew"), 1)
        self.assertEqual(world.names().count("tasks"), 2)
        self.assertIn("verified lease expired before the next renewal", world.alerts[0])

    def test_lease_expiring_during_read_retries_stops_without_another_mutation(self):
        world = World()
        world.reads = [task_answer(0, 100)] + ["unavailable"] * 100
        self.assertEqual(world.renewer().run(max_cycles=1), 1)
        self.assertLessEqual(world.now, 100.0)
        self.assertEqual(world.names().count("renew"), 1)
        self.assertIn("STOPPED", world.alerts[0])

    def test_startup_read_is_bounded_by_its_own_budget(self):
        world = World()
        world.reads = ["unavailable"] * 100
        self.assertEqual(world.renewer(startup_seconds=30.0).run(), 1)
        self.assertLess(world.now, 30.0)
        self.assertNotIn("renew", world.names())

    def test_failed_renew_gets_one_exact_retry_then_a_verified_read(self):
        world = World()
        world.renews = ["unavailable"]
        self.assertEqual(world.renewer().run(max_cycles=1), 0)
        self.assertEqual(world.names(), ["tasks", "renew", "retry", "tasks"])
        self.assertTrue(any("retry verified active" in line for line in world.logs))
        self.assertNotIn(SECRET, world.text())

    def test_failed_exact_retry_stops_without_a_second_retry_or_a_new_mutation(self):
        world = World()
        world.renews = ["timeout"]
        world.retries = ["unavailable", "unavailable"]
        self.assertEqual(world.renewer().run(max_cycles=1), 1)
        self.assertEqual(world.names(), ["tasks", "renew", "retry"])
        self.assertIn("exact retry failed", world.alerts[0])

    def test_unexpected_error_still_raises_the_alert_without_details(self):
        world = World()

        def broken(command, **kwargs):
            raise ValueError(SECRET)

        world.run = broken
        renewer = world.renewer()
        renewer.cli.run = broken
        self.assertEqual(renewer.run(), 1)
        self.assertEqual(len(world.alerts), 1)
        self.assertIn("unexpected ValueError", world.alerts[0])
        self.assertNotIn(SECRET, world.text())

    def test_only_renew_retry_and_tasks_show_are_ever_issued(self):
        world = World()
        world.renews = ["unavailable"]
        world.reads = ["ok", "unavailable"]
        world.renewer().run(max_cycles=2)
        self.assertLessEqual(set(world.names()), {"tasks", "renew", "retry"})
        self.assertNotIn("claim", world.names())


class ClaimConfigTests(unittest.TestCase):
    def write(self, value):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "claim.json"
        path.write_text(value if isinstance(value, str) else json.dumps(value))
        return path

    def test_existing_claim_format_is_accepted(self):
        path = self.write({"attempt": "a", "generation": 2, "task": "t", "session_id": "s", "renew_after_seconds": 300, "extra": SECRET})
        self.assertEqual(pilot.load_claim(path), pilot.Claim("a", 2, "t", "s", 300.0))
        path = self.write({"attempt": "a", "generation": 2, "task": "t", "session_id": "s"})
        self.assertEqual(pilot.load_claim(path).renew_after, 900.0)

    def test_invalid_claims_are_rejected_without_echoing_values(self):
        good = {"attempt": "a", "generation": 2, "task": "t", "session_id": "s"}
        for bad in (
            "not json " + SECRET,
            [SECRET],
            {**good, "attempt": ""},
            {**good, "task": 5},
            {**good, "generation": "2"},
            {**good, "generation": True},
            {**good, "renew_after_seconds": 0},
            {**good, "renew_after_seconds": SECRET},
        ):
            with self.subTest(bad=str(bad)[:30]):
                with self.assertRaises(ValueError) as caught:
                    pilot.load_claim(self.write(bad))
                self.assertNotIn(SECRET, str(caught.exception))


class CommandLineTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.dir = Path(directory.name)
        self.claim = self.dir / "claim.json"
        self.claim.write_text(json.dumps({"attempt": ATTEMPT, "generation": 3, "task": TASK, "session_id": SESSION, "renew_after_seconds": 1}))
        self.cli = self.dir / "fake-cli"
        self.log = self.dir / "calls.log"
        # A real executable fake client: expired lease, so the helper must stop at once.
        self.cli.write_text(
            "#!/bin/sh\n"
            f'echo "$@" >> "{self.log}"\n'
            'echo \'{"data":{"current_attempt_id":"attempt-1","generation":3,"attempts":[{"id":"attempt-1",'
            '"generation":3,"session_id":"session-1","state":"active","expires_at":"2020-01-01T00:00:00Z"}]},'
            '"server_time":"2026-01-01T00:00:00Z"}\'\n'
        )
        self.cli.chmod(self.cli.stat().st_mode | stat.S_IXUSR)

    def args(self, *extra):
        return ["--cli", str(self.cli), "--repo-config", "repo.toml", "--session", "pilot", "--claim-config", str(self.claim), *extra]

    def invoke(self, argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                code = pilot.main(argv)
            except SystemExit as exit:
                code = exit.code
        return code, out.getvalue(), err.getvalue()

    def test_parameterized_client_and_visible_stop(self):
        code, out, err = self.invoke(self.args())
        self.assertEqual(code, 1)
        self.assertIn("STOPPED: lease expired", out)
        self.assertIn("STOPPED: lease expired", err)
        self.assertEqual(self.log.read_text().strip(), f"--repo-config repo.toml --session pilot tasks show --id {TASK} --json")

    def test_second_producer_is_refused_while_the_lock_is_held(self):
        import fcntl

        with open(f"{self.claim}.lock", "a") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            code, out, err = self.invoke(self.args())
        self.assertEqual(code, 2)
        self.assertIn("already holds this producer lock", err)
        self.assertFalse(self.log.exists())  # the client was never called

    def test_bad_inputs_exit_two_without_calling_the_client(self):
        self.claim.write_text("{}")
        code, _, err = self.invoke(self.args())
        self.assertEqual(code, 2)
        self.assertIn("attempt must be", err)
        self.assertFalse(self.log.exists())
        code, _, err = self.invoke(self.args("--claim-config", str(self.dir / "missing.json")))
        self.assertEqual(code, 2)
        self.claim.write_text(json.dumps({"attempt": ATTEMPT, "generation": 3, "task": TASK, "session_id": SESSION}))
        code, _, err = self.invoke(["--cli", str(self.dir / "nope"), *self.args()[2:]])
        self.assertEqual(code, 2)
        self.assertIn("not an executable", err)
        code, _, _ = self.invoke(self.args("--read-retry-seconds", "0"))
        self.assertEqual(code, 2)

    def test_source_has_no_machine_specific_paths(self):
        source = Path(pilot.__file__).read_text()
        self.assertNotIn("/home/", source)
        self.assertNotIn("p3b-pilot", source)


if __name__ == "__main__":
    unittest.main()
