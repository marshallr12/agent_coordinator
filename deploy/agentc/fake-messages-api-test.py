#!/usr/bin/env python3
"""Tests for fake-messages-api.py, the containment suite's stand-in Messages API."""
import importlib.util
import json
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request
from pathlib import Path

API = Path(__file__).resolve().parent / "fake-messages-api.py"
SPEC = importlib.util.spec_from_file_location("fake_messages_api", API)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

BASH = [{"name": "Bash", "input_schema": {}}]
FIRST = {"model": "m", "tools": BASH, "messages": [{"role": "user", "content": "probe"}]}
ANSWERED = {"model": "m", "tools": BASH, "messages": [
    {"role": "user", "content": "probe"},
    {"role": "assistant", "content": [{"type": "tool_use", "id": "t", "name": "Bash", "input": {}}]},
    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": "ok"}]},
]}


class Replies(unittest.TestCase):
    """The first turn asks for the probe command; a tool result ends the run."""

    def test_first_turn_requests_the_probe_command(self):
        blocks, stop = MODULE.reply_blocks(FIRST, "echo hi")
        self.assertEqual(stop, "tool_use")
        self.assertEqual(blocks[0]["name"], "Bash")
        self.assertEqual(blocks[0]["input"]["command"], "echo hi")

    def test_tool_result_or_no_bash_tool_ends_the_turn(self):
        self.assertEqual(MODULE.reply_blocks(ANSWERED, "x")[1], "end_turn")
        self.assertEqual(MODULE.reply_blocks({"messages": []}, "x")[1], "end_turn")

    def test_stream_carries_the_tool_input_and_stop_reason(self):
        blocks, stop = MODULE.reply_blocks(FIRST, "echo hi")
        events = MODULE.stream_events(FIRST, blocks, stop)
        names = [name for name, _ in events]
        self.assertEqual(names[0], "message_start")
        self.assertEqual(names[-1], "message_stop")
        delta = dict(events)["content_block_delta"]["delta"]
        self.assertEqual(json.loads(delta["partial_json"])["command"], "echo hi")
        self.assertEqual(dict(events)["message_delta"]["delta"]["stop_reason"], "tool_use")


class Server(unittest.TestCase):
    """The server binds loopback, writes its port, and answers over HTTP."""

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        root = Path(self.dir.name)
        (root / "command").write_text("echo served\n")
        self.port_file = root / "port"
        self.process = subprocess.Popen(
            [sys.executable, "-I", str(API), str(self.port_file), str(root / "command"), "30"])
        for _ in range(100):
            if self.port_file.exists() and self.port_file.read_text():
                break
            time.sleep(0.05)

    def tearDown(self):
        self.process.kill()
        self.process.wait()
        self.dir.cleanup()

    def post(self, path, body):
        """POSTs `body` as JSON and returns the decoded response bytes."""
        url = f"http://127.0.0.1:{self.port_file.read_text()}{path}"
        request = urllib.request.Request(url, data=json.dumps(body).encode(), method="POST")
        with urllib.request.urlopen(request, timeout=10) as response:
            return response.read().decode()

    def test_messages_endpoint_returns_the_tool_use(self):
        reply = json.loads(self.post("/v1/messages?beta=true", FIRST))
        self.assertEqual(reply["content"][0]["input"]["command"], "echo served")

    def test_streamed_reply_is_server_sent_events(self):
        text = self.post("/v1/messages", {**FIRST, "stream": True})
        self.assertIn("event: message_start", text)
        self.assertIn("echo served", text)

    def test_other_paths_get_an_empty_object(self):
        self.assertEqual(json.loads(self.post("/api/other", {})), {})


if __name__ == "__main__":
    unittest.main()
