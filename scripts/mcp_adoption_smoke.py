"""Real-service MCP-to-native adoption checks, called by the disposable smoke fixture.

Secrets are passed only through the subprocess environment and never printed.
"""
import json
import os
from pathlib import Path
import subprocess
import urllib.request
import uuid


def exercise_mcp_adoption(temporary, api, project, token, binary, owner, attempt):
    source = next((temporary / f"agent-{owner}" / "sessions").glob("*.json"))
    identity = json.loads(source.read_text())
    origin = identity["service_origin"]
    proof = identity["session"]["proof"]
    session = identity["session"]["id"]
    home = temporary / "mcp-adopted"
    env = {k: v for k, v in os.environ.items() if not k.startswith("AGENT_COORDINATOR_")}
    env.update(AGENT_COORDINATOR_HOME=str(home), AGENT_COORDINATOR_MCP_TOKEN=token,
               AGENT_COORDINATOR_MCP_SESSION_ID=session,
               AGENT_COORDINATOR_MCP_SESSION_PROOF=proof,
               AGENT_COORDINATOR_MCP_URL=origin + "/mcp",
               AGENT_COORDINATOR_MCP_PROJECT_ID=project)
    base = [str(binary), "--repo-config", str(temporary / ".agent-coordinator.toml"),
            "--session", "adopted", "--allow-insecure-loopback", "--json"]
    command = ["session", "adopt-mcp", "--mcp-writes-quiescent",
               "--workstation", identity["workstation_id"]]

    def native(args=command, overrides=None, success=True):
        selected = dict(env)
        selected.update(overrides or {})
        result = subprocess.run(base + args, capture_output=True, text=True,
                                env=selected, timeout=15)
        output = result.stdout + result.stderr
        assert token not in output and proof not in output, "Adoption disclosed a secret."
        for name in ("AGENT_COORDINATOR_MCP_TOKEN", "AGENT_COORDINATOR_MCP_SESSION_PROOF"):
            assert selected[name] not in output, "Rejected adoption disclosed a supplied secret."
        if (result.returncode == 0) != success:
            failure = json.loads(result.stdout).get("error", {}) if result.stdout else {}
            raise AssertionError(f"Unexpected adoption status: {result.returncode}, {failure.get('code')}: {failure.get('message')}")
        return json.loads(result.stdout) if result.stdout else None

    # Verify the MCP host holds the same authenticated identity before handoff.
    request = urllib.request.Request(origin + "/mcp", data=json.dumps({
        "jsonrpc": "2.0", "id": "adoption-probe", "method": "tools/call",
        "params": {"name": "coordinator_session_get", "arguments": {"session": session}}
    }).encode(), headers={"Content-Type": "application/json",
        "Authorization": "Bearer " + token, "X-Coordinator-Session": session,
        "X-Coordinator-Session-Proof": proof,
        "Accept": "application/json, text/event-stream"})
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    with urllib.request.build_opener(NoRedirect()).open(request, timeout=10) as response:
        assert json.load(response)["result"]["structuredContent"]["data"]["session_id"] == session

    before = api(f"/api/v1/projects/{project}/attempts/{attempt['id']}")["attempt"]
    native(command[0:2] + command[3:], success=False)  # No quiescence acknowledgement.
    other_token = api("/api/v1/admin/agents", {"name": "unrelated-adoption-credential"})["token"]
    for overrides in [
        {"AGENT_COORDINATOR_MCP_SESSION_PROOF": "invalid-proof"},
        {"AGENT_COORDINATOR_MCP_TOKEN": "invalid-token"},
        {"AGENT_COORDINATOR_MCP_TOKEN": other_token},
        {"AGENT_COORDINATOR_MCP_PROJECT_ID": str(uuid.uuid4())},
        {"AGENT_COORDINATOR_MCP_URL": "https://wrong.example/mcp"},
        {"AGENT_COORDINATOR_TOKEN": "different-token", "AGENT_COORDINATOR_ORIGIN": origin},
    ]:
        native(overrides=overrides, success=False)
        assert not list((home / "sessions").glob("*.json")), "Failed adoption saved state."
    native(command[:-1] + ["wrong-workstation"], success=False)
    adopted = native()["data"]
    assert adopted["adopted"] and not adopted["already_adopted"]
    assert adopted["session_id"] == session and not adopted["authority_renewed"]
    assert not adopted["remote_writes"]
    saved_path = next((home / "sessions").glob("*.json"))
    original = saved_path.read_bytes()
    saved = json.loads(original)
    assert saved["session"] == identity["session"]
    assert saved["workstation_id"] == identity["workstation_id"]
    if os.name != "nt":
        assert saved_path.stat().st_mode & 0o077 == 0
    assert native()["data"]["already_adopted"]
    assert saved_path.read_bytes() == original, "Identical adoption rewrote durable state."
    for changed in [dict(saved, pending={"key": "unresolved", "method": "POST",
            "path": "/api/v1/projects", "body": {}}),
            dict(saved, session={"id": str(uuid.uuid4()), "proof": "different-proof"})]:
        saved_path.write_text(json.dumps(changed))
        snapshot = saved_path.read_bytes()
        native(success=False)
        assert saved_path.read_bytes() == snapshot, "Conflict overwrote protected state."
    saved_path.write_bytes(original)
    # The adopted session immediately owns the same attempt; no reconnect/reclaim.
    checkpoint = temporary / "mcp-adoption-checkpoint.json"
    checkpoint.write_text(json.dumps({"summary": "MCP session adopted without changing ownership."}))
    native(["checkpoint", "--attempt", attempt["id"], "--generation", str(attempt["generation"]),
            "--input", str(checkpoint)])
    after = api(f"/api/v1/projects/{project}/attempts/{attempt['id']}")["attempt"]
    assert before["expires_at"] == after["expires_at"], "Adoption implicitly renewed the lease."
    assert before["session_id"] == after["session_id"] == session
    print("PASS: MCP session adoption, exact ownership, rejected invalid/conflicting imports, private state, no renewal.")
