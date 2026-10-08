#!/usr/bin/env python3
"""Tests for attention.py against local fake coordinator and ntfy servers."""
import contextlib
import importlib.util
import io
import json
import os
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("attention", HERE / "attention.py")
attention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(attention)

TOKEN = "t" * 40
DIGEST = {"project_id": "p", "window_hours": 24, "since": "a", "until": "2026-10-08T00:00:00.000Z",
          "proceeded_decisions": [{"decision_id": "d1", "question": "Rename the flag?",
                                   "proceeded_with": "Rename", "proceeded_at": "x",
                                   "affected_task_ids": ["t1"]}],
          "pending_reversible_decisions": [],
          "hri": {"count": 1, "stalled_tasks": 1,
                  "items": [{"code": "stalled_task", "title": "Stubborn task"}]}}


class Fake:
    """A coordinator and an ntfy topic on loopback ports."""

    def __init__(self):
        self.next_status = 200
        self.pages = []
        self.coordinator_tokens = []
        fake = self

        class Coordinator(BaseHTTPRequestHandler):
            def do_GET(self):
                fake.coordinator_tokens.append(self.headers.get("Authorization"))
                status, body = 200, {"data": {"status": "ok"}}
                if self.path.startswith("/api/v1/projects/p/next"):
                    status, body = fake.next_status, {"data": {"action": None}}
                elif self.path.startswith("/api/v1/projects/p/digest"):
                    body = {"data": DIGEST}
                self.send_response(status)
                self.end_headers()
                self.wfile.write(json.dumps(body).encode())

            def log_message(self, *args):
                pass

        class Ntfy(BaseHTTPRequestHandler):
            def do_POST(self):
                size = int(self.headers.get("Content-Length", 0))
                fake.pages.append((self.path, self.rfile.read(size).decode()))
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


class AttentionTests(unittest.TestCase):
    def setUp(self):
        self.fake = Fake()
        self.addCleanup(self.fake.close)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.token = self.dir / "token"
        self.token.write_text(TOKEN + "\n")
        self.heartbeat = self.dir / "heartbeat.json"
        self.beat()

    def beat(self, age_seconds=1):
        self.heartbeat.write_text(json.dumps({"at_ms": int((time.time() - age_seconds) * 1000)}))

    def canary(self, *extra):
        argv = ["canary", "--url", self.fake.url, "--project", "p", "--token-file", str(self.token),
                "--heartbeat", str(self.heartbeat), "--ntfy-topic", "alerts",
                "--ntfy-url", self.fake.ntfy, *extra]
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = attention.main(argv)
        return code, err.getvalue()

    def test_a_healthy_loop_pages_nobody(self):
        self.assertEqual(self.canary()[0], 0)
        self.assertEqual(self.fake.pages, [])

    def test_a_canary_failure_produces_one_ntfy_page(self):
        self.fake.next_status = 500
        code, err = self.canary()
        self.assertEqual(code, 1)
        self.assertEqual(len(self.fake.pages), 1)
        path, body = self.fake.pages[0]
        self.assertEqual(path, "/alerts")
        self.assertIn("next: next answered 500", body)
        # The same failure on later runs does not page again.
        self.assertEqual(self.canary()[0], 1)
        self.assertEqual(self.canary()[0], 1)
        self.assertEqual(len(self.fake.pages), 1)
        self.assertNotIn(TOKEN, err + body)

    def test_recovery_rearms_the_page_and_a_new_failure_pages_again(self):
        self.fake.next_status = 500
        self.canary()
        self.fake.next_status = 200
        self.assertEqual(self.canary()[0], 0)
        self.fake.next_status = 500
        self.canary()
        self.assertEqual(len(self.fake.pages), 2)
        self.beat(age_seconds=3600)
        self.canary()
        self.assertEqual(len(self.fake.pages), 3)
        self.assertIn("supervisor heartbeat is", self.fake.pages[2][1])

    def test_an_undelivered_page_is_retried_by_the_next_run(self):
        self.fake.next_status = 500
        code, _ = self.canary("--ntfy-url", "http://127.0.0.1:9")
        self.assertEqual(code, 2)
        self.assertEqual(self.canary()[0], 1)
        self.assertEqual(len(self.fake.pages), 1)

    def test_hri_over_the_limit_fails_the_canary(self):
        self.assertEqual(self.canary("--max-hri", "1")[0], 0)
        self.assertEqual(self.canary("--max-hri", "0")[0], 1)
        self.assertIn("1 human-required interventions", self.fake.pages[0][1])

    def test_the_digest_lists_proceeded_decisions_and_hri(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = attention.main(["digest", "--url", self.fake.url, "--project", "p",
                                   "--token-file", str(self.token)])
        self.assertEqual(code, 0)
        text = out.getvalue()
        self.assertIn("Rename the flag? -> Rename", text)
        self.assertIn("Human-required interventions: 1 (1 stalled tasks)", text)
        self.assertIn("Stubborn task", text)
        self.assertNotIn(TOKEN, text)
        self.assertEqual(set(self.fake.coordinator_tokens), {f"Bearer {TOKEN}"})

    def test_options_default_from_the_environment_and_flags_win(self):
        env = {"ATTENTION_URL": self.fake.url, "ATTENTION_PROJECT": "p",
               "ATTENTION_TOKEN_FILE": str(self.token), "ATTENTION_HEARTBEAT": str(self.heartbeat),
               "ATTENTION_NTFY_TOPIC": "alerts", "ATTENTION_NTFY_URL": self.fake.ntfy,
               "ATTENTION_MAX_HRI": "", "ATTENTION_SMTP_PORT": "2525"}
        self.fake.next_status = 500
        with mock.patch.dict(os.environ, env):
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(attention.main(["canary"]), 1)
            args = attention.parse(["digest", "--smtp-port", "26"])
            self.assertEqual((args.url, args.smtp_port), (self.fake.url, 26))
            self.assertIsNone(attention.parse(["canary"]).max_hri)
            self.assertEqual(attention.parse(["digest"]).smtp_port, 2525)
        self.assertEqual(self.fake.pages[0][0], "/alerts")

    def test_the_canary_needs_a_topic_from_a_flag_or_the_environment(self):
        with mock.patch.dict(os.environ, {"ATTENTION_NTFY_TOPIC": ""}):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                attention.parse(["canary"])

    def test_code_defaults_name_the_production_service(self):
        with mock.patch.dict(os.environ, clear=True):
            args = attention.parse(["canary", "--ntfy-topic", "t"])
        self.assertEqual(args.url, "https://agents.sithbit.com")
        self.assertEqual(args.token_file, "/etc/agentc/attention-token")
        self.assertEqual(args.heartbeat, "/var/lib/agentc/heartbeat.json")
        self.assertEqual(args.ntfy_url, "https://ntfy.sh")


if __name__ == "__main__":
    unittest.main()
