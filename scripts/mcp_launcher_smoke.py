"""Exercise the native launcher, real HTTP MCP endpoint, and same-harness CLI.

The probe receives secrets only in its process environment and emits none.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.request
import uuid


def exercise_mcp_launcher(cli, api, project, owner, attempt, binary):
    before = api(f"/api/v1/projects/{project}/attempts/{attempt['id']}")
    result = cli(owner, "mcp-client", "--", sys.executable, str(Path(__file__).resolve()),
                 "--probe", str(binary), attempt["id"], str(attempt["generation"]))
    assert result["data"]["client_exited"] is True
    assert result["data"]["authority_renewed"] is False
    after = api(f"/api/v1/projects/{project}/attempts/{attempt['id']}")
    assert before["attempt"]["expires_at"] == after["attempt"]["expires_at"]
    task = api(f"/api/v1/projects/{project}/tasks/{attempt['task_id']}")
    assert any(c["summary"] == "MCP and native CLI share this exact harness."
               for c in task["checkpoints"])
    print("PASS: MCP client launcher, exact CLI session continuity, live MCP checkpoint, no implicit renewal.")


def probe(binary, attempt, generation):
    token = os.environ["AGENT_COORDINATOR_MCP_TOKEN"]
    proof = os.environ["AGENT_COORDINATOR_MCP_SESSION_PROOF"]
    session = os.environ["AGENT_COORDINATOR_MCP_SESSION_ID"]
    project = os.environ["AGENT_COORDINATOR_MCP_PROJECT_ID"]
    headers = {"Authorization": "Bearer " + token,
               "X-Coordinator-Session": session,
               "X-Coordinator-Session-Proof": proof,
               "Content-Type": "application/json",
               "Accept": "application/json, text/event-stream",
               "MCP-Protocol-Version": "2025-11-25"}
    # No origin-changing redirects, including in this disposable loopback fixture.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    opener = urllib.request.build_opener(NoRedirect())

    def rpc(method, params):
        request = urllib.request.Request(os.environ["AGENT_COORDINATOR_MCP_URL"],
            data=json.dumps({"jsonrpc": "2.0", "id": str(uuid.uuid4()),
                             "method": method, "params": params}).encode(), headers=headers)
        with opener.open(request, timeout=5) as response:
            assert response.headers.get("Mcp-Session-Id") is None
            payload = json.load(response)
        assert "error" not in payload, "MCP protocol call failed."
        return payload["result"]

    rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                       "clientInfo": {"name": "launcher-smoke", "version": "1"}})
    initialized = urllib.request.Request(os.environ["AGENT_COORDINATOR_MCP_URL"],
        data=json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}).encode(), headers=headers)
    with opener.open(initialized, timeout=5) as response:
        assert response.status == 202 and response.read() == b""
    state = rpc("tools/call", {"name": "coordinator_session_get", "arguments": {"session": session}})
    assert state.get("isError") is not True
    assert state["structuredContent"]["data"]["session_id"] == session
    # The ordinary session journal lock must be available to the client's CLI.
    native = subprocess.run([binary, "--json", "tasks", "list"],
                            capture_output=True, text=True, timeout=5)
    assert token not in native.stdout + native.stderr and proof not in native.stdout + native.stderr
    assert native.returncode == 0, "Native CLI cannot use the launched client's harness."
    tasks = json.loads(native.stdout)["data"]["items"]
    assert any(t["current_attempt_id"] == attempt for t in tasks)
    # A second launcher cannot run another MCP host under the same local name.
    nested = subprocess.run([binary, "--json", "mcp-client", "--", sys.executable,
                             str(Path(__file__).resolve()), "--nested-probe"],
                            capture_output=True, text=True, timeout=5)
    assert token not in nested.stdout + nested.stderr and proof not in nested.stdout + nested.stderr
    assert nested.returncode == 7, "Duplicate launcher was not excluded."
    checkpoint = rpc("tools/call", {"name": "coordinator_checkpoint", "arguments": {
        "project": project, "attempt": attempt, "idempotency_key": str(uuid.uuid4()),
        "body": {"generation": int(generation), "summary": "MCP and native CLI share this exact harness."}}})
    assert checkpoint.get("isError") is not True, "MCP does not own the CLI's claimed attempt."
    rpc("ping", {})


if __name__ == "__main__":
    if sys.argv[1:2] == ["--nested-probe"]:
        sys.exit(61)
    if sys.argv[1:2] != ["--probe"] or len(sys.argv) != 5:
        sys.exit(2)
    try:
        probe(*sys.argv[2:])
    except Exception:
        # Neither exception repr nor request objects may enter captured output.
        sys.exit(62)
