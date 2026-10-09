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
        self.results = self.dir / "results.jsonl"
        self.ledger = self.dir / "costs.jsonl"
        self.clock = Clock()

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
        self.assertEqual((task["kind"], task["priority"]), ("code", 3))
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

    def test_options_default_from_the_environment_and_flags_win(self):
        env = {"E2E_HARNESS": "codex", "E2E_URL": self.fake.url, "E2E_PROJECT": "p",
               "E2E_NTFY_TOPIC": "alerts", "E2E_TIMEOUT_MINUTES": "5", "E2E_POLL_SECONDS": ""}
        with mock.patch.dict(os.environ, env):
            args = e2e.parse(["--timeout-minutes", "7"])
        self.assertEqual((args.harness, args.url, args.timeout_minutes), ("codex", self.fake.url, 7.0))
        self.assertEqual(args.poll_seconds, e2e.DEFAULT_POLL_SECONDS)

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
