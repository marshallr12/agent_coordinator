#!/usr/bin/env python3
"""Tests for e2e-canary.py and canary-setup.py against local fake servers."""
import contextlib
import importlib.util
import io
import json
import os
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, HERE / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


e2e = load("e2e_canary", "e2e-canary.py")
setup = load("canary_setup", "canary-setup.py")

TOKEN = "t" * 40


class Fake:
    """A coordinator and an ntfy topic on loopback ports. A created task moves
    through the lifecycles in `script`, one per status poll, then stays on the
    last."""

    def __init__(self):
        self.script = ["open", "open", "done"]
        self.create_status = 201
        self.created = []
        self.keys = []
        self.polls = 0
        self.pages = []
        self.requests = []
        # Canary reports: bodies, their Idempotency-Keys, and an optional
        # `report_reply(body) -> status` standing in for the coordinator.
        self.reports = []
        self.report_keys = []
        self.report_reply = None
        fake = self

        class Coordinator(BaseHTTPRequestHandler):
            def reply(self, status, body):
                self.send_response(status)
                self.end_headers()
                self.wfile.write(json.dumps(body).encode())

            def body(self):
                return json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))

            def do_POST(self):
                fake.requests.append(("POST", self.path, self.headers.get("Authorization")))
                if self.path == "/api/v1/auth/login":
                    return self.reply(200, {"data": {"csrf_token": "c"}})
                if self.path == "/api/v1/projects":
                    fake.project_input = self.body()
                    return self.reply(200, {"data": {"id": "proj-1", "policy_revision": 1, "lease_seconds": 900}})
                if self.path.endswith("/telemetry/canary-results"):
                    body = self.body()
                    fake.reports.append(body)
                    fake.report_keys.append(self.headers.get("Idempotency-Key"))
                    status = fake.report_reply(body) if fake.report_reply else 200
                    if status != 200:
                        return self.reply(status, {"error": {"code": "x"}})
                    return self.reply(200, {"data": {"accepted": len(body["rows"]), "duplicates": 0}})
                fake.keys.append(self.headers.get("Idempotency-Key"))
                fake.created.append(self.body())
                if fake.create_status >= 300:
                    return self.reply(fake.create_status, {"error": {"code": "x"}})
                self.reply(201, {"data": {"id": "task-1"}})

            def do_PATCH(self):
                fake.policy = self.body()
                self.reply(200, {"data": {}})

            def do_PUT(self):
                fake.workflow_policy = self.body()
                self.reply(200, {"data": {"revision": 1}})

            def do_GET(self):
                fake.requests.append(("GET", self.path, self.headers.get("Authorization")))
                lifecycle = fake.script[min(fake.polls, len(fake.script) - 1)]
                fake.polls += 1
                self.reply(200, {"data": {"id": "task-1", "lifecycle": lifecycle, "work_status": "ready"}})

            def log_message(self, *args):
                pass

        class Ntfy(BaseHTTPRequestHandler):
            def do_POST(self):
                size = int(self.headers.get("Content-Length", 0))
                fake.pages.append((self.path, self.headers.get("Title"), self.rfile.read(size).decode()))
                self.send_response(200)
                self.end_headers()

            def log_message(self, *args):
                pass

        self.servers = [ThreadingHTTPServer(("127.0.0.1", 0), h) for h in (Coordinator, Ntfy)]
        for server in self.servers:
            threading.Thread(target=server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.servers[0].server_port}"
        self.ntfy = f"http://127.0.0.1:{self.servers[1].server_port}"

    def close(self):
        for server in self.servers:
            server.shutdown()
            server.server_close()


class Clock:
    """A fake monotonic clock whose sleep advances it, so deadlines need no waiting."""

    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class E2ETests(unittest.TestCase):
    def setUp(self):
        self.fake = Fake()
        self.addCleanup(self.fake.close)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.token = self.dir / "token"
        self.token.write_text(TOKEN + "\n")
        self.token.chmod(0o600)
        self.results = self.dir / "results.jsonl"
        self.ledger = self.dir / "costs.jsonl"
        self.clock = Clock()
        # Every run ends a second later, so no two result lines are identical.
        seconds = iter(range(10_000))

        def end_time(fmt, when=None):
            n = next(seconds)
            return f"2027-01-15T10:{n // 60:02d}:{n % 60:02d}Z"

        patch = mock.patch.object(e2e.time, "strftime", side_effect=end_time)
        patch.start()
        self.addCleanup(patch.stop)

    def canary(self, *extra, harness="claude"):
        argv = ["--harness", harness, "--url", self.fake.url, "--project", "p", "--token-file", str(self.token),
                "--ntfy-topic", "alerts", "--ntfy-url", self.fake.ntfy, "--results", str(self.results),
                "--ledger", str(self.ledger), "--host", "h1", "--poll-seconds", "30", *extra]
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = e2e.run(e2e.parse(argv), self.clock, self.clock.sleep)
        return code, err.getvalue()

    def rows(self):
        return [json.loads(line) for line in self.results.read_text().splitlines()]

    def ledger_row(self, harness, role="impl", task="task-1"):
        with self.ledger.open("a") as handle:
            handle.write(json.dumps({"role": role, "task": task, "harness": harness}) + "\n")

    def test_a_green_run_creates_a_harmless_task_and_records_duration_and_outcome(self):
        self.ledger_row("claude")
        self.ledger_row("codex", role="rev")  # a reviewer on the other vendor is fine
        code, err = self.canary()
        self.assertEqual(code, 0)
        self.assertEqual(self.fake.pages, [])
        [row] = self.rows()
        self.assertEqual((row["host"], row["harness"], row["outcome"], row["task"]),
                         ("h1", "claude", "ok", "task-1"))
        self.assertEqual(row["duration_seconds"], 60.0)  # two polls of 30 s before done
        self.assertEqual(row["served_by"], ["claude"])
        [task] = self.fake.created
        self.assertEqual((task["kind"], task["priority"]), ("code", 0))  # urgent: ahead of all other work
        self.assertIn("CANARY.md", task["description"])
        self.assertEqual(len(task["acceptance_criteria"]), 1)
        self.assertEqual(task["admission_class"], "canary")  # outside the weekly agent budget
        self.assertEqual({a for _, _, a in self.fake.requests}, {f"Bearer {TOKEN}"})
        self.assertNotIn(TOKEN, err + json.dumps(row))

    def test_a_timeout_pages_once_and_records_the_last_status(self):
        self.fake.script = ["open"]
        code, _ = self.canary("--timeout-minutes", "2")
        self.assertEqual(code, 1)
        [row] = self.rows()
        self.assertEqual(row["outcome"], "timeout")
        self.assertGreaterEqual(row["duration_seconds"], 120)
        self.assertIn("last seen open/ready", row["detail"])
        [(path, title, body)] = self.fake.pages
        self.assertEqual((path, title), ("/alerts", "agentc e2e canary failed"))
        self.assertIn("claude on h1: timeout", body)
        self.assertNotIn(TOKEN, body)

    def test_a_canceled_task_fails_and_pages(self):
        self.fake.script = ["open", "canceled"]
        self.assertEqual(self.canary()[0], 1)
        self.assertEqual(self.rows()[0]["outcome"], "failed")
        self.assertIn("canceled", self.rows()[0]["detail"])
        self.assertEqual(len(self.fake.pages), 1)

    def test_a_task_that_cannot_be_created_fails_and_pages(self):
        self.fake.create_status = 403
        self.assertEqual(self.canary()[0], 1)
        row = self.rows()[0]
        self.assertEqual((row["outcome"], row["task"]), ("failed", None))
        self.assertIn("403", row["detail"])
        self.assertEqual(len(self.fake.pages), 1)

    def test_a_server_error_is_retried_with_one_idempotency_key(self):
        self.fake.create_status = 503
        self.assertEqual(self.canary()[0], 1)
        self.assertEqual(len(self.fake.keys), e2e.CREATE_ATTEMPTS)
        self.assertEqual(len(set(self.fake.keys)), 1)

    def test_the_wrong_harness_serving_the_task_fails_the_run(self):
        self.ledger_row("claude")
        code, _ = self.canary(harness="codex")
        self.assertEqual(code, 1)
        self.assertIn("served by claude, not codex", self.rows()[0]["detail"])
        self.assertEqual(len(self.fake.pages), 1)

    def test_a_missing_ledger_leaves_the_harness_unverified_but_green(self):
        self.assertEqual(self.canary()[0], 0)
        self.assertIsNone(self.rows()[0]["served_by"])

    def test_an_undelivered_page_exits_2_and_the_result_is_still_recorded(self):
        self.fake.script = ["canceled"]
        code, err = self.canary("--ntfy-url", "http://127.0.0.1:9")
        self.assertEqual(code, 2)
        self.assertIn("ntfy page not delivered", err)
        self.assertEqual(self.rows()[0]["outcome"], "failed")

    def test_a_results_file_that_cannot_be_written_does_not_hide_the_page(self):
        self.fake.script = ["canceled"]
        code, err = self.canary("--results", str(self.dir / "missing" / "r.jsonl"))
        self.assertEqual(code, 1)
        self.assertIn("results not recorded", err)
        self.assertEqual(len(self.fake.pages), 1)

    def test_results_append_one_line_per_run(self):
        self.canary()
        self.fake.polls = 0
        self.canary()
        self.assertEqual(len(self.rows()), 2)

    def reported_ids(self):
        return Path(str(self.results) + ".reported").read_text().split()

    def line(self, **fields):
        row = {"at": "2027-01-15T10:00:00Z", "detail": "done", "duration_seconds": 61.5, "harness": "claude",
               "host": "h1", "outcome": "ok", "project": "p", "served_by": ["claude"], "task": "t-old"}
        row.update(fields)
        with self.results.open("a") as handle:
            handle.write(json.dumps(row, sort_keys=True) + "\n")

    def test_a_run_reports_its_result_once_with_a_stable_identity(self):
        self.ledger_row("claude")
        self.assertEqual(self.canary()[0], 0)
        [body] = self.fake.reports
        [row] = body["rows"]
        self.assertEqual(body["host"], "h1")
        line = self.results.read_text().splitlines()[0]
        self.assertEqual(row["row_id"], e2e.row_id(line))
        self.assertEqual((row["harness"], row["outcome"], row["task_id"], row["served_by"]),
                         ("claude", "ok", "task-1", ["claude"]))
        self.assertEqual(row["duration_seconds"], 60.0)
        self.assertEqual(row["occurred_at_ms"] % 1000, 0)
        self.assertEqual(self.reported_ids(), [row["row_id"]])
        self.fake.polls = 0
        self.canary()  # the next run sends only its own result
        self.assertEqual([len(b["rows"]) for b in self.fake.reports], [1, 1])
        self.assertEqual(len(set(self.reported_ids())), 2)
        self.assertEqual({a for _, _, a in self.fake.requests}, {f"Bearer {TOKEN}"})

    def test_a_coordinator_that_is_down_never_changes_the_exit_status_and_loses_nothing(self):
        self.fake.report_reply = lambda body: 503
        code, err = self.canary()
        self.assertEqual(code, 0)
        self.assertIn("not reported (HTTP 503)", err)
        self.assertNotIn(TOKEN, err)
        self.assertFalse(Path(str(self.results) + ".reported").exists())
        self.fake.polls = 0
        self.canary()  # still down: one request, rows still unsent
        self.assertEqual(len(self.fake.reports), 2)
        self.fake.report_reply = None
        self.fake.polls = 0
        self.canary()
        sent = self.fake.reports[-1]["rows"]
        self.assertEqual(len(sent), 3)  # both earlier results and this one
        self.assertEqual(len(self.rows()), 3)  # the results file was only ever appended to
        self.assertEqual(set(self.reported_ids()), {r["row_id"] for r in sent})

    def test_a_transport_failure_is_logged_and_the_rows_stay_unsent(self):
        self.line()
        args = e2e.parse(["--backfill", "--url", "http://127.0.0.1:9", "--project", "p", "--results", str(self.results)])
        with contextlib.redirect_stderr(io.StringIO()) as err:
            self.assertEqual(e2e.report_results(args, TOKEN), 0)
        self.assertIn("results not reported (URLError)", err.getvalue())
        self.assertNotIn(TOKEN, err.getvalue())
        self.assertFalse(Path(str(self.results) + ".reported").exists())

    def test_no_report_sends_nothing(self):
        self.assertEqual(self.canary("--no-report")[0], 0)
        self.assertEqual(self.fake.reports, [])
        self.assertEqual(len(self.rows()), 1)

    def test_a_retry_of_the_same_rows_reuses_its_idempotency_key(self):
        self.line()
        self.line(task="t-older", at="2027-01-14T10:00:00Z")
        self.fake.report_reply = lambda body: 502
        argv = ["--backfill", "--url", self.fake.url, "--project", "p", "--token-file", str(self.token),
                "--results", str(self.results), "--host", "h1"]
        for _ in range(2):
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(e2e.run(e2e.parse(argv)), 0)
        self.assertEqual(len(self.fake.report_keys), 2)
        self.assertEqual(len(set(self.fake.report_keys)), 1)
        self.assertLessEqual(len(self.fake.report_keys[0]), 128)

    def test_backfill_imports_an_existing_results_file_once(self):
        self.line()
        self.line(task="t-2", outcome="failed", detail="not done", at="2027-01-14T10:00:00Z")
        self.line(task="t-3", project=None, host=None, at="2027-01-13T10:00:00Z")  # a legacy row
        with self.results.open("a") as handle:
            handle.write("not json\n" + json.dumps({"outcome": "ok"}) + "\n\n")
        argv = ["--backfill", "--url", self.fake.url, "--project", "p-default", "--token-file", str(self.token),
                "--results", str(self.results), "--host", "host-default"]
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(e2e.run(e2e.parse(argv)), 0)
        self.assertIn("reported 3 results", err.getvalue())
        self.assertEqual(self.fake.created, [])  # a backfill runs no canary
        self.assertEqual(self.fake.pages, [])
        by_host = {b["host"]: b["rows"] for b in self.fake.reports}
        self.assertEqual({h: len(r) for h, r in by_host.items()}, {"h1": 2, "host-default": 1})
        paths = sorted(path for _, path, _ in self.fake.requests)
        self.assertEqual(paths, ["/api/v1/projects/p-default/telemetry/canary-results",
                                 "/api/v1/projects/p/telemetry/canary-results"])
        failed = [r for r in by_host["h1"] if r["outcome"] == "failed"][0]
        self.assertEqual((failed["task_id"], failed["detail"], failed["occurred_at_ms"]),
                         ("t-2", "not done", 1_799_920_800_000))
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(e2e.run(e2e.parse(argv)), 0)  # a second import has nothing to send
        self.assertEqual(len(self.fake.reports), 2)
        os.remove(str(self.results) + ".reported")  # even a lost marker only resends, never duplicates
        with contextlib.redirect_stderr(io.StringIO()):
            e2e.run(e2e.parse(argv))
        resent = {r["row_id"] for b in self.fake.reports[2:] for r in b["rows"]}
        first = {r["row_id"] for b in self.fake.reports[:2] for r in b["rows"]}
        self.assertEqual(resent, first)

    def test_a_row_the_coordinator_refuses_is_skipped_and_the_rest_still_go(self):
        self.line(task="good-1")
        self.line(task="bad", detail="refuse me")
        self.line(task="good-2")
        self.fake.report_reply = lambda body: 422 if any(r["detail"] == "refuse me" for r in body["rows"]) else 200
        argv = ["--backfill", "--url", self.fake.url, "--project", "p", "--token-file", str(self.token),
                "--results", str(self.results), "--host", "h1"]
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(e2e.run(e2e.parse(argv)), 0)
        self.assertIn("refused (422); not resent", err.getvalue())
        self.assertEqual(len(self.reported_ids()), 3)
        before = len(self.fake.reports)
        with contextlib.redirect_stderr(io.StringIO()):
            e2e.run(e2e.parse(argv))
        self.assertEqual(len(self.fake.reports), before)

    def test_backfill_needs_a_project_but_no_harness_or_topic(self):
        with mock.patch.dict(os.environ, clear=True), contextlib.redirect_stderr(io.StringIO()):
            self.assertTrue(e2e.parse(["--backfill", "--project", "p"]).backfill)
            with self.assertRaises(SystemExit):
                e2e.parse(["--backfill"])

    def test_options_default_from_the_environment_and_flags_win(self):
        env = {"E2E_HARNESS": "codex", "E2E_URL": self.fake.url, "E2E_PROJECT": "p",
               "E2E_NTFY_TOPIC": "alerts", "E2E_TIMEOUT_MINUTES": "5", "E2E_POLL_SECONDS": ""}
        with mock.patch.dict(os.environ, env):
            args = e2e.parse(["--timeout-minutes", "7"])
        self.assertEqual((args.harness, args.url, args.timeout_minutes), ("codex", self.fake.url, 7.0))
        self.assertEqual(args.poll_seconds, e2e.DEFAULT_POLL_SECONDS)

    def test_the_priority_defaults_to_the_highest_the_service_accepts_and_is_configurable(self):
        self.assertEqual(e2e.DEFAULT_PRIORITY, 0)
        self.assertEqual(e2e.PRIORITIES, (0, 1, 2, 3))  # the service takes 0 (urgent) through 3 (low)
        self.canary("--priority", "2")
        self.assertEqual(self.fake.created[-1]["priority"], 2)
        self.fake.polls = 0
        with mock.patch.dict(os.environ, {"E2E_PRIORITY": "1"}):
            self.canary()
            self.assertEqual(self.fake.created[-1]["priority"], 1)
            self.fake.polls = 0
            self.canary("--priority", "0")  # a flag wins over the environment
            self.assertEqual(self.fake.created[-1]["priority"], 0)
        with mock.patch.dict(os.environ, {"E2E_PRIORITY": ""}):  # empty counts as unset
            self.fake.polls = 0
            self.canary()
            self.assertEqual(self.fake.created[-1]["priority"], 0)

    def test_a_priority_the_service_would_refuse_is_refused_up_front(self):
        argv = ["--harness", "claude", "--project", "p", "--ntfy-topic", "t"]
        with mock.patch.dict(os.environ, clear=True), contextlib.redirect_stderr(io.StringIO()):
            for bad in ("-1", "4", "high"):
                with self.assertRaises(SystemExit):
                    e2e.parse([*argv, "--priority", bad])

    def test_code_defaults_name_the_production_service_and_host_paths(self):
        with mock.patch.dict(os.environ, clear=True):
            args = e2e.parse(["--harness", "claude", "--project", "p", "--ntfy-topic", "t"])
        self.assertEqual(args.url, "https://agents.sithbit.com")
        self.assertEqual(args.token_file, "/etc/agentc/e2e-canary-token")
        self.assertEqual(args.results, "/var/lib/agentc/e2e-canary.jsonl")
        self.assertEqual(args.ledger, "/var/lib/agentc/costs.jsonl")
        self.assertEqual(args.timeout_minutes, 90.0)

    def test_harness_project_and_topic_are_required(self):
        with mock.patch.dict(os.environ, clear=True), contextlib.redirect_stderr(io.StringIO()):
            for argv in (["--project", "p", "--ntfy-topic", "t"], ["--harness", "claude", "--ntfy-topic", "t"],
                         ["--harness", "claude", "--project", "p"], ["--harness", "gemini", "--project", "p",
                                                                     "--ntfy-topic", "t"]):
                with self.assertRaises(SystemExit):
                    e2e.parse(argv)


class SetupTests(unittest.TestCase):
    def setUp(self):
        self.fake = Fake()
        self.addCleanup(self.fake.close)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.password = self.dir / "password"
        self.password.write_text("secret-password\n")

    def run_setup(self, *extra):
        out = io.StringIO()
        argv = ["--repository-url", "git@example.invalid:o/canary.git", "--seed-dir", str(self.dir / "seed"),
                "--url", self.fake.url, "--host", "h1", "--password-file", str(self.password), *extra]
        with contextlib.redirect_stdout(out):
            code = setup.main(argv)
        return code, out.getvalue()

    def test_it_creates_the_project_policy_roster_and_a_committed_seed(self):
        code, out = self.run_setup()
        self.assertEqual(code, 0)
        self.assertEqual(self.fake.project_input["name"], "canary-h1")
        self.assertEqual(self.fake.project_input["repository_url"], "git@example.invalid:o/canary.git")
        self.assertEqual(self.fake.policy["automatic_integration"], True)
        self.assertEqual(self.fake.policy["review_mode"], "either")
        self.assertEqual(self.fake.workflow_policy["required_checks"],
                         [{"identity": "canary-diff-check", "version": "v1", "environment": "any"}])
        self.assertIn("proj-1", out)
        self.assertNotIn("secret-password", out)
        seed = self.dir / "seed"
        self.assertIn('project_id = "proj-1"', (seed / ".agent-coordinator.toml").read_text())
        self.assertTrue((seed / "CANARY.md").is_file())
        self.assertIn("canary-diff-check", (seed / ".agent-coordinator/roster.toml").read_text())
        log = subprocess.run(["git", "-C", str(seed), "log", "--format=%s", "main"], capture_output=True,
                             text=True, check=True).stdout
        self.assertEqual(log.strip(), "Canary project seed")

    def test_it_refuses_a_seed_directory_that_already_has_files(self):
        (self.dir / "seed").mkdir()
        (self.dir / "seed" / "x").write_text("x")
        with self.assertRaises(SystemExit):
            self.run_setup()
        self.assertFalse(hasattr(self.fake, "project_input"))

    def test_required_checks_can_be_replaced(self):
        self.run_setup("--required-check", "a:v2:ci")
        self.assertEqual(self.fake.workflow_policy["required_checks"],
                         [{"identity": "a", "version": "v2", "environment": "ci"}])


if __name__ == "__main__":
    unittest.main()
