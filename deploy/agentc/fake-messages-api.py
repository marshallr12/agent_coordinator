#!/usr/bin/env python3
"""A loopback stand-in for the Anthropic Messages API, for containment checks.

The containment suite uses it to drive the real pinned Claude Code through one
Bash tool call without launching an LLM: the first `/v1/messages` request of a
conversation is answered with a `tool_use` that runs COMMAND in Bash, and any
request that already carries a `tool_result` is answered with a short final
text, which ends the `claude -p` run. Every other path gets an empty JSON
object, so incidental client traffic neither fails nor hangs.

Usage: fake-messages-api.py PORT_FILE COMMAND_FILE [LIFETIME_SECONDS]

It binds 127.0.0.1 on an ephemeral port, writes that port to PORT_FILE, and
exits after LIFETIME_SECONDS (default 300) so a failed run never leaves it up.
"""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOOL_ID = "toolu_containment_suite"


def has_tool_result(body):
    """True when any message in the request carries a tool_result block."""
    for message in body.get("messages", []):
        content = message.get("content")
        if isinstance(content, list) and any(
            isinstance(block, dict) and block.get("type") == "tool_result" for block in content
        ):
            return True
    return False


def reply_blocks(body, command):
    """The content blocks and stop reason that answer one request."""
    if has_tool_result(body) or not any(t.get("name") == "Bash" for t in body.get("tools", [])):
        return [{"type": "text", "text": "done"}], "end_turn"
    tool = {"type": "tool_use", "id": TOOL_ID, "name": "Bash",
            "input": {"command": command, "description": "containment probe"}}
    return [tool], "tool_use"


def message(body, blocks, stop):
    """A complete non-streaming Messages API response."""
    return {"id": "msg_containment_suite", "type": "message", "role": "assistant",
            "model": body.get("model", "fake"), "content": blocks, "stop_reason": stop,
            "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 1}}


def block_events(index, block):
    """The streaming events that deliver one content block."""
    if block["type"] == "text":
        start, delta = {"type": "text", "text": ""}, {"type": "text_delta", "text": block["text"]}
    else:
        start = {**block, "input": {}}
        delta = {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}
    return [("content_block_start", {"index": index, "content_block": start}),
            ("content_block_delta", {"index": index, "delta": delta}),
            ("content_block_stop", {"index": index})]


def stream_events(body, blocks, stop):
    """The full server-sent event sequence for a streamed response."""
    start = {**message(body, [], None), "usage": {"input_tokens": 1, "output_tokens": 0}}
    events = [("message_start", {"message": start})]
    for index, block in enumerate(blocks):
        events += block_events(index, block)
    events += [("message_delta", {"delta": {"stop_reason": stop, "stop_sequence": None},
                                  "usage": {"output_tokens": 1}}),
               ("message_stop", {})]
    return events


class Handler(BaseHTTPRequestHandler):
    """Answers Messages requests as described above; everything else with {}."""

    probe_command = "true"

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length)
        if not self.path.split("?")[0].endswith("/v1/messages"):
            return self.send_json({})
        body = json.loads(raw or b"{}")
        blocks, stop = reply_blocks(body, self.probe_command)
        if body.get("stream"):
            return self.send_stream(stream_events(body, blocks, stop))
        self.send_json(message(body, blocks, stop))

    def do_GET(self):
        self.send_json({})

    def do_HEAD(self):
        self.send_response(200)
        self.end_headers()

    def send_json(self, value):
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def send_stream(self, events):
        data = "".join(f"event: {name}\ndata: {json.dumps({'type': name, **payload})}\n\n"
                       for name, payload in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_args):
        pass


def main(argv):
    """Serve until the lifetime elapses."""
    port_file, command_file = argv[1], argv[2]
    lifetime = float(argv[3]) if len(argv) > 3 else 300.0
    with open(command_file, encoding="utf-8") as handle:
        Handler.probe_command = handle.read().strip()
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    with open(port_file, "w", encoding="utf-8") as handle:
        handle.write(str(server.server_address[1]))
    threading.Timer(lifetime, server.shutdown).start()
    server.serve_forever()


if __name__ == "__main__":
    main(sys.argv)
