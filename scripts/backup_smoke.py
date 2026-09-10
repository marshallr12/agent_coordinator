#!/usr/bin/env python3
"""Timed, disposable live-backup/fresh-directory-restore exercise.

Build the workspace first. The exercise stops its original service before
attesting that it is fenced. It never touches an existing installation.
"""
import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
BUILD = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "debug"
SERVER = BUILD / "agent-coordinator-server"
CLI = BUILD / "agent-coordinator"


class Browser:
    def __init__(self, origin):
        self.origin = origin
        self.csrf = ""
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def call(self, path, body=None, expected=200):
        request = urllib.request.Request(self.origin + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json", "Origin": self.origin,
                     "X-CSRF-Token": self.csrf, "Idempotency-Key": str(uuid.uuid4())})
        try:
            reply = self.opener.open(request, timeout=20)
        except urllib.error.HTTPError as error:
            reply = error
        with reply:
            status = reply.status
            value = json.load(reply)
        assert status == expected, f"Restore exercise API status {status}, expected {expected}: {value.get('error', {}).get('code')}"
        return value.get("data") if status < 400 else value["error"]

    def login(self, password):
        self.csrf = self.call("/api/v1/auth/login", {
            "username": "restore-fixture", "password": password})["csrf_token"]


def run(browser_fixture=False):
    assert SERVER.is_file() and CLI.is_file(), "Build the workspace first."
    with tempfile.TemporaryDirectory(prefix="coordinator-restore-") as directory:
        temporary = Path(directory)
        source = temporary / "live"
        source.mkdir()
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        origin = f"http://127.0.0.1:{port}"
        common = ["--listen", f"127.0.0.1:{port}", "--public-origin", origin,
                  "--allow-insecure-loopback"]
        database = source / "coordinator.sqlite3"
        server_options = [str(SERVER), "--database", str(database), *common]

        def host(*args, password=None, expected=0, options=None, cwd=temporary):
            result = subprocess.run([*(options or server_options), *args],
                input=None if password is None else password + "\n", text=True,
                capture_output=True, timeout=300, cwd=cwd)
            assert result.returncode == expected, f"Host {args[0]} failed (output withheld to protect fixture credentials)."
            return json.loads(result.stdout) if result.stdout.lstrip().startswith("{") else None

        def start(options):
            process = subprocess.Popen([*options, "serve"], stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL)
            for _ in range(200):
                try:
                    with urllib.request.urlopen(origin + "/healthz", timeout=1):
                        return process
                except (urllib.error.URLError, TimeoutError):
                    assert process.poll() is None, "Disposable service exited."
                    time.sleep(.05)
            process.terminate()
            process.wait(timeout=10)
            raise AssertionError("Disposable service did not start.")

        def stop(process):
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)

        password = secrets.token_hex(24)
        host("init-admin", "--username", "restore-fixture", "--password-stdin", password=password)
        process = start(server_options)
        try:
            browser = Browser(origin)
            browser.login(password)
            project = browser.call("/api/v1/projects", {
                "name": "Restore fixture", "repository_url": "https://example.invalid/restore.git",
                "target_branch": "main"})["id"]
            base = f"/api/v1/projects/{project}"
            credential = browser.call("/api/v1/admin/agents", {"name": "restore-worker"})
            binding = temporary / ".agent-coordinator.toml"
            binding.write_text(f'service_url = "{origin}"\nproject_id = "{project}"\n')

            def client(token, home, *args, body=None, expected=0):
                env = {k: v for k, v in os.environ.items() if not k.startswith("AGENT_COORDINATOR_")}
                env.update(AGENT_COORDINATOR_HOME=str(temporary / home),
                           AGENT_COORDINATOR_TOKEN=token, AGENT_COORDINATOR_ORIGIN=origin)
                command = [str(CLI), "--repo-config", str(binding), "--session", home,
                           "--allow-insecure-loopback", "--json", *args]
                if body is not None:
                    command += ["--input", "-"]
                result = subprocess.run(command, input=None if body is None else json.dumps(body),
                    text=True, capture_output=True, env=env, timeout=30)
                assert token not in result.stdout, "Credential leaked into native output."
                assert result.returncode == expected, f"Native {args[0]} status mismatch."
                payload = json.loads(result.stdout)
                return payload.get("data") if expected == 0 else payload["error"]

            token = credential["token"]
            client(token, "original-harness", "connect")
            task = client(token, "original-harness", "tasks", "create", body={
                "title": "Preserve a checkpoint across restore", "kind": "general",
                "acceptance_criteria": ["Restore does not revive old ownership"]})
            attempt = client(token, "original-harness", "claim", "--task", task["id"],
                "--revision", str(task["revision"]))["claim"]["attempt"]
            own = ["--attempt", attempt["id"], "--generation", str(attempt["generation"])]
            client(token, "original-harness", "checkpoint", *own, body={
                "summary": "Checkpoint captured before backup.", "next_step": "Inspect saved evidence."})
            resource = browser.call("/api/v1/resources", {"key": "restore/physical-hold", "capacity": 1,
                "description": "Fixture reservation; no physical producer was launched."})
            client(token, "original-harness", "resources", "reserve", *own,
                body={"items": [{"resource_id": resource["id"], "units": 1}]})
            evidence = temporary / "evidence.bin"
            content = b"Preserved backup artifact.\x00\xff\n" * 4096
            evidence.write_bytes(content)
            artifact = client(token, "original-harness", "artifacts", "reserve", body={
                "filename": "evidence.bin", "media_type": "application/octet-stream",
                "size_bytes": len(content), "sha256": hashlib.sha256(content).hexdigest(),
                "task_id": task["id"], "job_id": None, "pinned": True})["artifact"]
            client(token, "original-harness", "artifacts", "upload", "--id", artifact["id"], "--file", str(evidence))
            backup = host("backup", "--repository", str(temporary / "backups"))
            snapshot = Path(backup["snapshot_path"])
            assert backup["artifact_count"] == 1
            # An off-server transfer is modeled as another private directory;
            # real remote transport is an installation responsibility.
            offsite = temporary / "offsite-copy"
            shutil.copytree(snapshot, offsite)
            verified = host("backup-verify", "--snapshot", str(offsite), options=[str(SERVER)])
            assert verified["verified"] and not (temporary / "data").exists()
            host("restore", "--snapshot", str(offsite), "--destination", str(source),
                "--reason", "Must refuse an existing live data directory.", expected=1, options=[str(SERVER)])
            assert browser.call(base + f"/tasks/{task['id']}")["work_status"] == "in_progress"
            client(token, "original-harness", "checkpoint", *own, body={
                "summary": "Post-snapshot update, intentionally absent from restored history.",
                "next_step": "Account for this gap before resuming."})
            stop(process)
            process = None
            restored_at = time.monotonic()
            restored = host("restore", "--snapshot", str(offsite), "--destination", str(temporary / "restored"),
                "--reason", "Disposable restore exercise after stopping the original service.", options=[str(SERVER)])
            assert not (temporary / "data").exists(), "Restore opened the default live database."
            restored_options = [str(SERVER), "--database", restored["database_path"], *common]
            process = start(restored_options)
            browser.call("/api/v1/auth/account", expected=401)
            Browser(origin).call("/api/v1/auth/login", {"username": "restore-fixture", "password": password}, expected=401)
            client(token, "original-harness", "tasks", "list", expected=3)
            new_password = ("-".join(["disposable", "restore", "browser", "fixture"])
                            if browser_fixture else secrets.token_hex(24))
            host("recover-operator-password", "--username", "restore-fixture", "--reason",
                 "Recover the disposable administrator after restore.", "--password-stdin",
                 options=restored_options, password=new_password)
            current = Browser(origin)
            current.login(new_password)
            status = current.call("/api/v1/admin/restore")
            # Service-state field names and requirement shape follow the restore API.
            restore_id = status["service_state"]["restore_id"]
            detail = current.call(base + f"/tasks/{task['id']}")
            assert detail["work_status"] == "recovery_required"
            assert len(detail["checkpoints"]) == 1
            assert current.call("/api/v1/resources")["items"][0]["held_units"] == 1
            if browser_fixture:
                print(json.dumps({"origin": origin, "data_directory": restored["data_directory"],
                                  "project_id": project, "task_id": task["id"]}), flush=True)
                while True:
                    time.sleep(5)
            replacement = current.call(f"/api/v1/admin/agents/{credential['principal_id']}/credentials", {"name": "after-restore"})
            assert replacement["principal_id"] == credential["principal_id"]
            fresh_token = replacement["token"]
            client(fresh_token, "recovered-harness", "connect")
            paused = client(fresh_token, "recovered-harness", "claim", "--task", task["id"],
                "--revision", str(task["revision"]), "--mode", "recovery", expected=5)
            assert paused["code"] == "restore_reconciliation_required"
            current.call("/api/v1/admin/restore/finish", {"restore_id": restore_id,
                "reason": "Must reject missing reconciliation."}, expected=409)
            requirements = status["items"]
            while status.get("next_cursor"):
                status = current.call("/api/v1/admin/restore?cursor=" + urllib.parse.quote(status["next_cursor"], safe=""))
                requirements.extend(status["items"])
            assert requirements, "The restored physical hold must require inspection."
            for requirement in requirements:
                current.call("/api/v1/admin/restore/inspections", {"restore_id": restore_id,
                    "kind": requirement["kind"], "target_id": requirement["target_id"],
                    "disposition": "held", "evidence": "Fixture inspected: no producer launched; retain the hold for task recovery."})
            current.call("/api/v1/admin/restore/old-installation-fenced", {"restore_id": restore_id,
                "evidence": "The original disposable server process exited before the restored server was started."})
            current.call("/api/v1/admin/restore/post-snapshot-gap", {"restore_id": restore_id,
                "evidence": "The only post-snapshot change was one fixture checkpoint; no source publication or producer launch occurred."})
            current.call("/api/v1/admin/restore/finish", {"restore_id": restore_id,
                "reason": "All disposable holdings and the post-snapshot gap have been inspected."})
            assert current.call("/api/v1/resources")["items"][0]["held_units"] == 1
            output = temporary / "restored-evidence.bin"
            client(fresh_token, "recovered-harness", "artifacts", "download", "--id", artifact["id"], "--output", str(output))
            assert output.read_bytes() == content
            recovered = client(fresh_token, "recovered-harness", "claim", "--task", task["id"],
                "--revision", str(task["revision"]), "--mode", "recovery")["claim"]["attempt"]
            assert recovered["generation"] > attempt["generation"] and recovered["mode"] == "recovery"
            assert current.call("/api/v1/resources")["items"][0]["held_units"] == 1
            elapsed = time.monotonic() - restored_at
            assert elapsed < 3600, "Restore rehearsal exceeded the one-hour target."
            print(f"PASS: live consistent snapshot, verified copied bundle, fresh-directory restore, old access rejection,")
            print(f"      preserved checkpoint/artifact/physical hold, explicit reconciliation, same-principal native reconnect and inspected-recovery claim.")
            print(f"      Disposable recovery completed in {elapsed:.1f}s; this is not a production-size restore benchmark.")
        finally:
            if process is not None:
                stop(process)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--browser-fixture", action="store_true",
                        help="Keep a disposable paused restore running for manual browser verification.")
    run(parser.parse_args().browser_fixture)
