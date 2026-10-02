#!/usr/bin/env python3
"""Deadline tests for ship; all subprocesses and clocks are mocked."""

from collections import deque
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("ship", Path(__file__).with_name("ship.py"))
SHIP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SHIP)
SHA = "1234567890abcdef" * 2 + "12345678"
REPO = "Owner/Repo"


def actions(status="in_progress", conclusion=None):
    return [
        {
            "databaseId": index,
            "workflowName": name,
            "status": status,
            "conclusion": conclusion,
            "createdAt": "2026-10-02T00:00:00Z",
        }
        for index, name in enumerate(SHIP.REQUIRED_WORKFLOWS)
    ]


class FakeClock:
    def __init__(self):
        self.started = 1000.0
        self.now = self.started
        self.sleeps = []

    def monotonic(self):
        return self.now

    def sleep(self, duration):
        if not 0 < duration <= SHIP.POLL_SECONDS:
            raise AssertionError(f"invalid poll sleep: {duration}")
        self.sleeps.append((self.now, duration))
        self.now += duration


class ShipDeadlineTests(unittest.TestCase):
    def setUp(self):
        self.clock = FakeClock()
        self.calls = []
        self.responses = deque()
        self.default_runs = actions()
        self.patch(SHIP.time, "monotonic", side_effect=self.clock.monotonic)
        self.patch(SHIP.time, "sleep", side_effect=self.clock.sleep)
        self.patch(SHIP.time, "time", side_effect=AssertionError("use monotonic time"))
        self.process = self.patch(SHIP.subprocess, "run", side_effect=self.query)

    def patch(self, obj, attribute, **kwargs):
        patcher = mock.patch.object(obj, attribute, **kwargs)
        self.addCleanup(patcher.stop)
        return patcher.start()

    def query(self, args, **kwargs):
        # Even an unexpected test path cannot launch Git, gh, or a real process.
        self.assertEqual(args[:3], ("gh", "run", "list"))
        self.assertEqual(args[args.index("--commit") + 1], SHA)
        self.assertEqual(args[args.index("--repo") + 1], f"github.com/{REPO}")
        self.assertTrue(kwargs["text"])
        self.assertTrue(kwargs["capture_output"])
        self.assertGreater(kwargs["timeout"], 0)
        self.assertLessEqual(kwargs["timeout"], 300)
        self.assertLess(len(self.calls), 1000, "deadline regression caused unbounded polling")
        self.calls.append((self.clock.now, kwargs["timeout"]))
        response = self.responses.popleft() if self.responses else self.default_runs
        if callable(response):
            return response(args, kwargs["timeout"])
        return subprocess.CompletedProcess(args, 0, json.dumps(response), "")

    def delayed(self, duration, runs):
        def respond(args, timeout):
            self.clock.now += duration
            return subprocess.CompletedProcess(args, 0, json.dumps(runs), "")
        return respond

    def hung(self, args, timeout):
        self.clock.now += timeout
        raise subprocess.TimeoutExpired(args, timeout)

    def assert_exit(self, message, timeout=SHIP.CHECKS_TIMEOUT_SECONDS):
        with self.assertRaises(SystemExit) as error:
            SHIP.wait_for_checks(SHA, REPO, checks_timeout=timeout)
        self.assertIn(message, str(error.exception))
        self.assertIn(SHA, str(error.exception))

    def assert_sleep_bounds(self, deadline):
        for started, duration in self.clock.sleeps:
            self.assertGreater(duration, 0)
            self.assertLessEqual(duration, SHIP.POLL_SECONDS)
            self.assertLessEqual(started + duration, deadline)

    def test_pending_forever_uses_the_default_overall_deadline(self):
        self.assertEqual(SHIP.CHECKS_TIMEOUT_SECONDS, 1800)
        self.assert_exit("timed out")
        deadline = self.clock.started + 1800
        self.assertEqual(self.clock.now, deadline)
        self.assertEqual(len(self.calls), 120)
        self.assert_sleep_bounds(deadline)
        for started, timeout in self.calls:
            self.assertLessEqual(started + timeout, deadline)

    def test_missing_workflows_keep_the_300_second_appearance_deadline(self):
        self.default_runs = []
        self.assertEqual(SHIP.APPEAR_TIMEOUT_SECONDS, 300)
        self.assert_exit("never started")
        deadline = self.clock.started + 300
        self.assertEqual(self.clock.now, deadline)
        self.assertEqual(len(self.calls), 20)
        self.assert_sleep_bounds(deadline)
        for started, timeout in self.calls:
            self.assertLessEqual(started + timeout, deadline)

    def test_one_missing_workflow_still_requires_appearance(self):
        self.default_runs = actions()[:1]
        self.assert_exit("never started")
        self.assertEqual(self.clock.now, self.clock.started + 300)

    def test_hung_gh_is_bounded_by_the_remaining_overall_budget(self):
        self.responses.extend([actions(), self.hung])
        self.assert_exit("timed out", timeout=40)
        self.assertEqual(self.calls, [(1000.0, 40.0), (1015.0, 25.0)])
        self.assertEqual(self.clock.now, self.clock.started + 40)

    def test_hung_initial_query_is_also_bounded_by_the_appearance_window(self):
        self.responses.append(self.hung)
        self.assert_exit("never started")
        self.assertEqual(self.calls, [(1000.0, 300.0)])
        self.assertEqual(self.clock.now, self.clock.started + 300)

    def test_huge_finite_budgets_cap_queries_without_shortening_the_overall_wait(self):
        for budget in [2592000, 1e308]:
            with self.subTest(budget=budget):
                self.clock.now = self.clock.started
                self.clock.sleeps.clear()
                self.calls.clear()
                self.responses.clear()
                self.responses.extend([actions()] * 21 + [actions("completed", "success")])
                SHIP.wait_for_checks(SHA, REPO, checks_timeout=budget)
                self.assertEqual(self.clock.now, self.clock.started + 315)
                self.assertEqual(len(self.calls), 22)
                self.assertTrue(all(timeout == 300 for _, timeout in self.calls))
                self.assert_sleep_bounds(self.clock.started + budget)

    def test_hung_query_after_appearance_with_huge_budget_reports_the_sha(self):
        for budget in [2592000, 1e308]:
            with self.subTest(budget=budget):
                self.clock.now = self.clock.started
                self.clock.sleeps.clear()
                self.calls.clear()
                self.responses.clear()
                self.responses.extend([actions(), self.hung])
                self.assert_exit("GitHub Actions query timed out", timeout=budget)
                self.assertEqual(self.calls, [(1000.0, 300.0), (1015.0, 300.0)])
                self.assertEqual(self.clock.now, self.clock.started + 315)

    def test_completed_success_at_or_after_the_overall_deadline_is_refused(self):
        for elapsed in [10, 11]:
            with self.subTest(elapsed=elapsed):
                self.clock.now = self.clock.started
                self.responses.append(self.delayed(elapsed, actions("completed", "success")))
                self.assert_exit("timed out", timeout=10)
                self.assertEqual(self.clock.sleeps, [])

    def test_first_completed_response_after_the_appearance_deadline_is_refused(self):
        self.responses.append(self.delayed(301, actions("completed", "success")))
        self.assert_exit("never started")
        self.assertEqual(self.clock.sleeps, [])

    def test_late_success_after_workflows_have_appeared_is_refused(self):
        self.responses.extend([actions(), self.delayed(6, actions("completed", "success"))])
        self.assert_exit("timed out", timeout=20)
        self.assertEqual(self.calls, [(1000.0, 20.0), (1015.0, 5.0)])

    def test_gh_failure_still_names_the_sha(self):
        self.responses.append(
            lambda args, timeout: subprocess.CompletedProcess(args, 1, "", "synthetic gh error")
        )
        self.assert_exit("failed")
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(self.clock.sleeps, [])

    def test_completed_failure_names_the_workflow_and_sha(self):
        self.default_runs = actions("completed", "success")
        self.default_runs[0]["conclusion"] = "failure"
        self.assert_exit("Coordination checks did not succeed")
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(self.clock.sleeps, [])

    def test_completed_success_returns_without_polling(self):
        self.default_runs = actions("completed", "success")
        SHIP.wait_for_checks(SHA, REPO)
        self.assertEqual(self.calls, [(1000.0, 300.0)])
        self.assertEqual(self.clock.sleeps, [])

    def test_success_after_pending_queries_consumes_query_time_from_the_budget(self):
        self.responses.extend([
            self.delayed(2, actions()),
            self.delayed(1, actions("completed", "success")),
        ])
        SHIP.wait_for_checks(SHA, REPO, checks_timeout=20)
        self.assertEqual(self.calls, [(1000.0, 20.0), (1017.0, 3.0)])
        self.assertEqual(self.clock.now, self.clock.started + 18)
        self.assertEqual(self.clock.sleeps, [(1002.0, 15)])

    def test_poll_sleep_and_query_timeout_shrink_with_the_overall_budget(self):
        self.responses.append(self.delayed(2, actions()))
        self.assert_exit("timed out", timeout=20)
        self.assertEqual(self.calls, [(1000.0, 20.0), (1017.0, 3.0)])
        self.assertEqual(self.clock.sleeps, [(1002.0, 15), (1017.0, 3)])
        self.assert_sleep_bounds(self.clock.started + 20)

    def test_poll_sleep_and_query_timeout_shrink_with_the_appearance_window(self):
        self.default_runs = []
        self.responses.append(self.delayed(7, []))
        self.assert_exit("never started")
        self.assertEqual(self.calls[-1], (1292.0, 8.0))
        self.assertEqual(self.clock.sleeps[-1], (1292.0, 8.0))
        self.assert_sleep_bounds(self.clock.started + 300)

    def test_cli_rejects_zero_negative_and_nonfinite_values_before_publication(self):
        for value in ["0", "-1", "nan", "inf", "-inf", "1e999", "not-a-number"]:
            with self.subTest(value=value):
                with mock.patch.object(SHIP, "run", return_value="topic"), \
                     mock.patch.object(SHIP, "clean_head") as clean, \
                     mock.patch.object(SHIP.sys, "argv", ["ship.py", f"--checks-timeout={value}"]), \
                     mock.patch.object(SHIP.sys, "stderr", new_callable=io.StringIO) as stderr:
                    with self.assertRaises(SystemExit) as error:
                        SHIP.main()
                    self.assertEqual(error.exception.code, 2)
                    self.assertIn("positive, finite", stderr.getvalue())
                    clean.assert_not_called()
        self.process.assert_not_called()

    def test_cli_passes_the_default_or_positive_finite_override_to_the_wait(self):
        for value, expected in [(None, 1800), ("2.5", 2.5), ("2592000", 2592000), ("1e308", 1e308)]:
            with self.subTest(value=value):
                argv = ["ship.py", "--name", "topic"]
                if value is not None:
                    argv.extend(["--checks-timeout", value])
                with mock.patch.object(SHIP, "run", return_value="topic"), \
                     mock.patch.object(SHIP, "clean_head", return_value=SHA), \
                     mock.patch.object(SHIP, "validated_remote", return_value=("f", "p", REPO)), \
                     mock.patch.object(SHIP, "require_fast_forward"), \
                     mock.patch.object(SHIP, "remote_tip", return_value=SHA), \
                     mock.patch.object(SHIP, "wait_for_checks") as wait, \
                     mock.patch.object(SHIP.sys, "argv", argv), \
                     mock.patch.object(SHIP.sys, "stdout", new_callable=io.StringIO):
                    SHIP.main()
                    wait.assert_called_once_with(SHA, REPO, checks_timeout=expected)
        self.process.assert_not_called()


if __name__ == "__main__":
    unittest.main()
