#!/usr/bin/env python3
"""Exercise the built server and native CLI together using disposable local data.

Run `cargo build --workspace --locked` first. Python's standard library is enough.
No existing credentials, repositories, or service state are used.
"""
import concurrent.futures
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import platform
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid

from job_smoke import exercise_jobs
from completion_smoke import exercise_completion
from shared_smoke import exercise_shared
from operator_smoke import exercise_operator
from mcp_launcher_smoke import exercise_mcp_launcher
from mcp_adoption_smoke import exercise_mcp_adoption
from mcp_transport_smoke import exercise_mcp_transport


ROOT = Path(__file__).resolve().parents[1]
BUILD = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "debug"
BINARY_SUFFIX = ".exe" if os.name == "nt" else ""
SERVER = BUILD / f"agent-coordinator-server{BINARY_SUFFIX}"
CLI = BUILD / f"agent-coordinator{BINARY_SUFFIX}"


def run():
    global CLI
    assert SERVER.is_file() and CLI.is_file(), "Build the workspace first."
    subprocess.run([sys.executable, str(ROOT / "scripts/upgrade_client_test.py")], check=True, timeout=30)
    with tempfile.TemporaryDirectory(prefix="coordinator-smoke-") as directory:
        temporary = Path(directory)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        origin = f"http://127.0.0.1:{port}"
        options = [str(SERVER), "--database", str(temporary / "test.sqlite3"),
                   "--listen", f"127.0.0.1:{port}", "--public-origin", origin,
                   "--allow-insecure-loopback"]
        password = secrets.token_hex(24)
        initialized = subprocess.run(options + ["init-admin", "--username", "smoke",
                                     "--password-stdin"], input=password + "\n",
                                     text=True, capture_output=True, timeout=30)
        assert initialized.returncode == 0, "Disposable operator initialization failed."
        process = subprocess.Popen(options + ["serve"], stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL)
        try:
            for _ in range(100):
                try:
                    with urllib.request.urlopen(origin + "/healthz", timeout=1):
                        break
                except (urllib.error.URLError, TimeoutError):
                    assert process.poll() is None, "Service exited during startup."
                    time.sleep(0.05)
            else:
                raise AssertionError("Service did not become ready.")

            opener = urllib.request.build_opener(
                urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
            csrf = ""

            def api(path, body=None, method=None):
                headers = {"Content-Type": "application/json", "Origin": origin,
                           "X-CSRF-Token": csrf, "Idempotency-Key": str(uuid.uuid4())}
                request = urllib.request.Request(origin + path, headers=headers,
                           data=None if body is None else json.dumps(body).encode(), method=method)
                try:
                    with opener.open(request, timeout=10) as response:
                        return json.load(response)["data"]
                except urllib.error.HTTPError as error:
                    failure = json.load(error).get("error", {})
                    raise AssertionError(f"API {path} failed with {error.code}: {failure.get('code')} {failure.get('message')}") from None

            csrf = api("/api/v1/auth/login", {"username": "smoke", "password": password})["csrf_token"]
            project = api("/api/v1/projects", {"name": "Smoke project",
                "repository_url": "https://example.com/demo.git", "target_branch": "main"})["id"]
            second = api("/api/v1/projects", {"name": "Second project",
                "repository_url": "https://example.com/second.git", "target_branch": "main"})["id"]
            binding = temporary / ".agent-coordinator.toml"
            binding.write_text(f'service_url = "{origin}"\nproject_id = "{project}"\n')
            credentials = [api("/api/v1/admin/agents", {"name": f"workstation-{i}"}) for i in range(2)]

            def cli(index, *args, body=None, expected=0, project_id=None):
                env = {k: v for k, v in os.environ.items() if not k.startswith("AGENT_COORDINATOR_")}
                runner_home = temporary / f"agent-{index}"
                session_directory = runner_home / "sessions"
                if os.name == "nt":
                    env["AGENT_COORDINATOR_STATE_DIR"] = str(session_directory)
                else:
                    env["AGENT_COORDINATOR_HOME"] = str(runner_home)
                env.update(AGENT_COORDINATOR_TOKEN=credentials[index]["token"],
                           AGENT_COORDINATOR_ORIGIN=origin)
                selected_binding = binding
                if project_id is not None:
                    selected_binding = temporary / f"binding-{project_id}.toml"
                    selected_binding.write_text(f'service_url = "{origin}"\nproject_id = "{project_id}"\n')
                command = [str(CLI), "--repo-config", str(selected_binding), "--session", f"harness-{index}",
                           "--allow-insecure-loopback", "--json", *args]
                if body is not None:
                    command += ["--input", "-"]
                result = subprocess.run(command, input=None if body is None else json.dumps(body),
                                        text=True, capture_output=True, env=env,
                                        timeout=45 if os.name == "nt" else 15)
                assert credentials[index]["token"] not in result.stdout + result.stderr, "Token leaked into CLI output."
                for saved_path in session_directory.glob("*.json"):
                    saved = json.loads(saved_path.read_text())
                    proof = saved.get("session", {}).get("proof")
                    assert not proof or proof not in result.stdout + result.stderr, "Session proof leaked into CLI output."
                payload = json.loads(result.stdout)
                failure = payload.get("error", {})
                assert result.returncode == expected, f"CLI {args[0]} returned {result.returncode}, expected {expected}: {failure.get('code')} {failure.get('message')}"
                return payload

            if os.name != "nt":
                # Upgrade a same-semver legacy build to the exact client named by live discovery.
                upgrade_dir = temporary / "upgrade"
                upgrade_dir.mkdir()
                installed_cli = upgrade_dir / "agent-coordinator"
                legacy_commit = "8bf489f72cf5b82dac1fa7ebd6ec6d53bd7f13c3"
                installed_cli.write_text(
                    "#!/usr/bin/env python3\nimport json\n"
                    "print(json.dumps({'version':'0.1.0','build':"
                    "{'source_commit':'" + legacy_commit + "','source_repository':'https://github.com/marshallr12/agent_coordinator',"
                    "'target_os':'linux','target_arch':'" + platform.machine().lower() + "','dirty':False},'capabilities':[]}))\n"
                )
                installed_cli.chmod(0o755)
                protected_state = upgrade_dir / "session-state.json"
                protected_state.write_text('{"origin":"local-smoke","pending_key":"preserved"}')
                original_state = protected_state.read_bytes()
                with urllib.request.urlopen(origin + "/api/v1/info", timeout=5) as response:
                    service_info = upgrade_dir / "service-info.json"
                    service_info.write_bytes(response.read())
                digest = hashlib.sha256(CLI.read_bytes()).hexdigest()
                upgraded = subprocess.run([
                    sys.executable, str(ROOT / "scripts/upgrade_client.py"),
                    "--binary", str(installed_cli), "--service-info", str(service_info),
                    "--candidate", str(CLI), "--sha256", digest,
                ], text=True, capture_output=True, timeout=30)
                assert upgraded.returncode == 0, f"Verified client upgrade failed: {upgraded.stderr}"
                assert Path(str(installed_cli) + ".rollback").is_file(), "Upgrade did not retain its rollback executable."
                assert protected_state.read_bytes() == original_state, "Upgrade changed protected session state."
                CLI = installed_cli

            for index in range(2):
                connected = cli(index, "connect")
                assert connected["data"]["orientation"]["instructions_complete"]
                assert len(cli(index, "projects", "list")["data"]["items"]) == 2
            compatibility = cli(0, "compatibility")
            assert compatibility["compatible"] is True
            task = cli(0, "tasks", "create", body={"title": "One shared task",
                "description": "Two native CLI processes compete for one task.",
                "acceptance_criteria": ["Exactly one owner"]})["data"]
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
                claims = list(executor.map(lambda index: cli(index, "claim", "--next"), range(2)))
            winners = [i for i, claim in enumerate(claims) if claim["data"]["claim"] is not None]
            assert len(winners) == 1, "Competing CLI processes did not produce exactly one owner."
            owner = winners[0]
            attempt = claims[owner]["data"]["claim"]["attempt"]
            own_args = ["--attempt", attempt["id"], "--generation", str(attempt["generation"])]
            cli(owner, "renew", *own_args)
            cli(owner, "checkpoint", *own_args, body={"summary": "Saved the shared handoff.", "next_step": "Review checkpoint."})
            detail = api(f"/api/v1/projects/{project}/tasks/{task['id']}")
            assert detail["work_status"] == "in_progress" and detail["checkpoints"][0]["summary"] == "Saved the shared handoff."
            assert api(f"/api/v1/projects/{second}/tasks")["items"] == []
            exercise_mcp_launcher(cli, api, project, owner, attempt, CLI)
            exercise_mcp_adoption(temporary, api, project, credentials[owner]["token"], CLI, owner, attempt)
            cli(owner, "release", *own_args, body={"summary": "Paused safely with a handoff."})
            assert api(f"/api/v1/projects/{project}/tasks/{task['id']}")["work_status"] == "ready"

            exercise_jobs(temporary, api, cli, project, owner)
            exercise_completion(temporary, api, cli, owner)
            exercise_shared(temporary, api, cli, project, owner)

            exercise_operator(api, cli, owner, options)

            adapter_project = api("/api/v1/projects", {"name": "Adapter smoke", "repository_url": "https://example.com/adapter.git", "target_branch": "main"})["id"]
            adapter = BUILD / f"agent-coordinator-mcp{BINARY_SUFFIX}"
            exercise_mcp_transport(temporary, api, adapter_project, origin, adapter)
            if os.environ.get("MCP_EVALUATION_DIR"):
                evaluation_project = api("/api/v1/projects", {"name": "Isolated adapter evaluation", "repository_url": "https://example.com/evaluation.git", "target_branch": "main"})["id"]
                evaluation_temp = temporary / "evaluation-fixture"
                evaluation_temp.mkdir()
                exercise_mcp_transport(evaluation_temp, api, evaluation_project, origin, adapter,
                                       os.environ["MCP_EVALUATION_DIR"])

            # Credential revocation must be visible to the actual CLI on its next call.
            api(f"/api/v1/admin/credentials/{credentials[owner]['credential_id']}/revoke", {})
            rejected = cli(owner, "tasks", "list", expected=3)
            assert "error" in rejected
            print("PASS: service startup, operator login, two-project visibility, two native harness sessions,")
            print("      atomic competing claims, renewal, checkpoint visibility, safe release, and revocation.")
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


if __name__ == "__main__":
    run()
