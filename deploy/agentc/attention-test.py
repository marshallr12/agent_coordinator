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
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("attention", HERE / "attention.py")
attention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(attention)

TOKEN = "t" * 40
ACK_URL = "https://agents.example/api/v1/projects/p/digest/ack?token=123.abc"
DAY = 86400.0
START = 1_790_000_000.0
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
        self.last_read_at = None
        self.digest_queries = []
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
                    fake.digest_queries.append(self.path)
                    body = {"data": {**DIGEST, "last_read_at": fake.last_read_at,
                                     "ack_link": {"url": ACK_URL, "expires_at": "later"}
                                     if "ack_link=true" in self.path else None}}
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

    def test_the_emailed_digest_carries_the_ack_link_and_the_printed_one_does_not(self):
        sent = []

        class Smtp:
            def __init__(self, host, port, timeout):
                pass

            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

            def send_message(self, message):
                sent.append(message)

        out = io.StringIO()
        with mock.patch.object(attention.smtplib, "SMTP", Smtp), contextlib.redirect_stdout(out):
            attention.main(["digest", "--url", self.fake.url, "--project", "p",
                            "--token-file", str(self.token), "--mail-to", "me@example.org",
                            "--smtp-host", "localhost"])
        self.assertEqual(len(sent), 1)
        self.assertIn(ACK_URL, sent[0].get_content())
        self.assertNotIn(ACK_URL, out.getvalue())
        self.assertIn("ack_link=true", self.fake.digest_queries[0])
        # Without mail the digest asks for no link.
        with contextlib.redirect_stdout(io.StringIO()):
            attention.main(["digest", "--url", self.fake.url, "--project", "p",
                            "--token-file", str(self.token)])
        self.assertNotIn("ack_link", self.fake.digest_queries[1])

    def neglect_canary(self, clock, day, *extra):
        """One canary run on `day` of a fake clock that starts at START."""
        clock[0] = START + day * DAY
        self.heartbeat.write_text(json.dumps({"at_ms": int(clock[0] * 1000)}))
        return self.canary(*extra)[0]

    def read_at(self, day):
        stamp = datetime.fromtimestamp(START + day * DAY, timezone.utc)
        self.fake.last_read_at = stamp.strftime("%Y-%m-%dT%H:%M:%S.000Z")

    def test_a_digest_unread_for_more_than_n_days_pages_once_and_a_read_rearms_it(self):
        clock = [START]
        with mock.patch.object(attention, "now", lambda: clock[0]):
            self.read_at(0)
            self.assertEqual(self.neglect_canary(clock, 0), 0)
            self.assertEqual(self.neglect_canary(clock, 2), 0)
            self.assertEqual(self.fake.pages, [], "no page on day 2")
            self.assertEqual(self.neglect_canary(clock, 4), 1)
            self.assertEqual(len(self.fake.pages), 1, "one page on day 4")
            self.assertIn("digest: digest unread for 4 days (limit 3)", self.fake.pages[0][1])
            self.assertEqual(self.neglect_canary(clock, 5), 1)
            self.assertEqual(len(self.fake.pages), 1, "no repeat page on day 5")
            # A read re-arms the page, but only another lapse of N days pages again.
            self.read_at(5)
            self.assertEqual(self.neglect_canary(clock, 5), 0)
            self.assertEqual(self.neglect_canary(clock, 7), 0)
            self.assertEqual(len(self.fake.pages), 1)
            self.assertEqual(self.neglect_canary(clock, 9), 1)
            self.assertEqual(len(self.fake.pages), 2)
            self.assertEqual(self.neglect_canary(clock, 10), 1)
            self.assertEqual(len(self.fake.pages), 2)

    def test_a_digest_never_read_counts_from_the_first_canary_run(self):
        clock = [START]
        with mock.patch.object(attention, "now", lambda: clock[0]):
            self.assertEqual(self.neglect_canary(clock, 0), 0)
            self.assertEqual(self.neglect_canary(clock, 2), 0)
            self.assertEqual(self.neglect_canary(clock, 4), 1)
            self.assertEqual(self.neglect_canary(clock, 5), 1)
            self.assertEqual(len(self.fake.pages), 1)

    def test_the_neglect_limit_is_a_setting_and_zero_turns_it_off(self):
        clock = [START]
        with mock.patch.object(attention, "now", lambda: clock[0]):
            self.read_at(0)
            self.assertEqual(self.neglect_canary(clock, 2, "--neglect-days", "1"), 1)
            self.assertEqual(self.neglect_canary(clock, 30, "--neglect-days", "0"), 0)
        self.assertEqual(len(self.fake.pages), 1)
        with mock.patch.dict(os.environ, clear=True):
            self.assertEqual(attention.parse(["canary", "--ntfy-topic", "t"]).neglect_days, 3)
        with mock.patch.dict(os.environ, {"ATTENTION_NEGLECT_DAYS": "5"}):
            self.assertEqual(attention.parse(["canary", "--ntfy-topic", "t"]).neglect_days, 5)

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
