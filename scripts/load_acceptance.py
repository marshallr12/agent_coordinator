#!/usr/bin/env python3
"""Disposable Linux capacity exercise; no existing service or credentials are used.

The default acceptance run is 30 minutes at 50 requests/second. Short runs are
development checks and are labeled as such. Historical fixtures are seeded in
SQLite while the service is stopped; all measured operations use HTTP.
"""
import argparse
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
import hashlib
import http.client
import http.cookiejar
import json
import math
import os
from pathlib import Path
import platform
import resource
import secrets
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]


def identifier():
    return str(uuid.uuid4())


def percentile(values, percent):
    return round(sorted(values)[max(0, math.ceil(len(values) * percent / 100) - 1)], 3) if values else None


class Api:
    def __init__(self, origin, headers=None, browser=False):
        self.origin, self.headers = origin, headers or {}
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar())) if browser else None

    def call(self, path, body=None, allowed=(200,), key=None):
        headers = {**self.headers, "Content-Type": "application/json"}
        if body is not None:
            headers["Idempotency-Key"] = key or identifier()
        request = urllib.request.Request(self.origin + path, headers=headers,
            data=None if body is None else json.dumps(body).encode())
        try:
            reply = (self.opener.open if self.opener else urllib.request.urlopen)(request, timeout=15)
        except urllib.error.HTTPError as error:
            reply = error
        with reply:
            status, value = reply.status, json.load(reply)
        if status not in allowed:
            # Print only stable error codes, never a credential-bearing response.
            raise RuntimeError(f"HTTP {status}: {value.get('error', {}).get('code', 'invalid_response')}")
        return status, value.get("data") if status < 400 else value.get("error", {})


def run(args):
    if platform.system() != "Linux":
        raise SystemExit("This acceptance exercise requires native Linux.")
    server = args.server.resolve()
    if not server.is_file():
        raise SystemExit("Build the service first or select --server.")
    affinity = sorted(os.sched_getaffinity(0))[:2]
    if len(affinity) != 2:
        raise SystemExit("Two available CPU cores are required.")
    memory_limit = 4 * 1024 ** 3

    def constrain_service():
        os.sched_setaffinity(0, affinity)
        resource.setrlimit(resource.RLIMIT_AS, (memory_limit, memory_limit))

    report = {"schema_version": 1, "passed": False,
        "full_acceptance": args.duration >= 1800 and args.rate == 50 and args.history == 100000,
        "duration_requested_seconds": args.duration, "requests_per_second": args.rate,
        "projects": 20, "agent_sessions": 50, "historical_tasks": args.history,
        "historical_fixture": "Synthetic canceled tasks, definition revisions, and audit events; seeded with service stopped.",
        "host": {"os": platform.platform(), "architecture": platform.machine(),
            "service_cpu_cores": 2, "service_address_space_limit_bytes": memory_limit,
            "memory_limit_kind": "RLIMIT_AS per service/backup process; RSS sampled independently"},
        "transport": "Authenticated loopback HTTP; HTTPS installation is exercised separately."}
    samples, errors = defaultdict(list), Counter()
    try:
        report["source_commit"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        report["source_dirty"] = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True))
        report["executable_sha256"] = hashlib.sha256(server.read_bytes()).hexdigest()
        with tempfile.TemporaryDirectory(prefix="coordinator-load-") as folder:
            temporary = Path(folder)
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                port = listener.getsockname()[1]
            origin = f"http://127.0.0.1:{port}"
            database = temporary / "live" / "service.sqlite3"
            command = [str(server), "--database", str(database), "--listen", f"127.0.0.1:{port}",
                "--public-origin", origin, "--allow-insecure-loopback"]
            password = secrets.token_hex(24)
            initialized = subprocess.run(command + ["init-admin", "--username", "load-fixture", "--password-stdin"],
                input=password + "\n", text=True, capture_output=True, timeout=30)
            assert initialized.returncode == 0, "Disposable initialization failed."

            def start():
                process = subprocess.Popen(command + ["serve"], stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL, preexec_fn=constrain_service)
                for _ in range(200):
                    try:
                        Api(origin).call("/healthz")
                        return process
                    except (OSError, RuntimeError):
                        assert process.poll() is None, "Disposable service exited."
                        time.sleep(.05)
                stop(process)
                raise AssertionError("Disposable service startup timed out.")

            def stop(process):
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)

            def login():
                operator = Api(origin, {"Origin": origin}, browser=True)
                _, signed = operator.call("/api/v1/auth/login", {"username": "load-fixture", "password": password})
                operator.headers["X-CSRF-Token"] = signed["csrf_token"]
                return operator

            process = start()
            try:
                operator = login()
                projects = [operator.call("/api/v1/projects", {"name": f"Load project {index:02}",
                    "repository_url": f"https://example.invalid/load-{index}.git", "target_branch": "main"})[1]["id"]
                    for index in range(20)]
                _, credential = operator.call("/api/v1/admin/agents", {"name": "capacity-fixture"})
                stop(process)
                process = None
                # Seed historical volume without timing import or hiding setup in request latency.
                with sqlite3.connect(database) as connection:
                    connection.execute("PRAGMA foreign_keys=ON")
                    report["database_schema_version"] = connection.execute("SELECT max(version) FROM _sqlx_migrations").fetchone()[0]
                    actor = connection.execute("SELECT id FROM principals WHERE kind='human'").fetchone()[0]
                    now = int(time.time() * 1000) - 86400000
                    for start_index in range(0, args.history, 1000):
                        rows, revisions, events = [], [], []
                        for index in range(start_index, min(start_index + 1000, args.history)):
                            task, project = identifier(), projects[index % 20]
                            title = f"Historical capacity fixture {index:06}"
                            detail = {"title": title, "description": "Synthetic historical load record.",
                                "acceptance_criteria": ["Historical fixture; no claimed implementation outcome."],
                                "kind": "general", "priority": 2, "depends_on": [], "planned": False}
                            rows.append((task, project, title, detail["description"], json.dumps(detail["acceptance_criteria"]), now, now))
                            revisions.append((project, task, json.dumps(detail), actor, now))
                            events.append((project, actor, task, now))
                        connection.executemany("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?,?,?,?,?,'general',2,'canceled',?,?)", rows)
                        connection.executemany("INSERT INTO task_revisions(project_id,task_id,revision,data_json,actor_id,created_at) VALUES(?,?,1,?,?,?)", revisions)
                        connection.executemany("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(?,?,'load_fixture.historical',?,'{}',?)", events)
                        connection.commit()
                    assert connection.execute("PRAGMA foreign_key_check").fetchone() is None
                process = start()
                operator = login()
                sessions, active = [], []
                orientations = {}
                for index in range(50):
                    project, session_id = projects[index % 20], identifier()
                    api = Api(origin, {"Authorization": "Bearer " + credential["token"],
                        "X-Coordinator-Session-Proof": secrets.token_urlsafe(32)})
                    api.call("/api/v1/sessions", {"session_id": session_id, "workstation_id": identifier(),
                        "harness": f"capacity-harness-{index}", "capabilities": []})
                    api.headers["X-Coordinator-Session"] = session_id
                    for ack_project in set([project, projects[0]]):
                        if ack_project not in orientations:
                            orientations[ack_project] = api.call(f"/api/v1/projects/{ack_project}/orientation")[1]
                        orientation = orientations[ack_project]
                        api.call(f"/api/v1/sessions/{session_id}/instruction-acknowledgments", {
                            "project_id": ack_project, "policy_revision": orientation["policy_revision"],
                            "instruction_version": orientation["instruction_version"], "sections": orientation["required_sections"]})
                    base = f"/api/v1/projects/{project}"
                    _, task = api.call(base + "/tasks", {"title": f"Capacity active task {index}", "kind": "general",
                        "acceptance_criteria": ["Exactly one current owner throughout capacity exercise."]})
                    _, claim = api.call(base + "/claims", {"task_id": task["id"], "expected_task_revision": task["revision"],
                        "policy_revision": orientations[project]["policy_revision"], "instruction_version": orientations[project]["instruction_version"]})
                    sessions.append(api)
                    active.append((base, task["id"], claim["claim"]["attempt"]))
                _, race_task = sessions[0].call(f"/api/v1/projects/{projects[0]}/tasks", {"title": "100 simultaneous claim requests",
                    "kind": "general", "acceptance_criteria": ["Exactly one winner."]})
                barrier = threading.Barrier(100)

                def race(index):
                    barrier.wait(timeout=20)
                    return sessions[index % 50].call(f"/api/v1/projects/{projects[0]}/claims", {
                        "task_id": race_task["id"], "expected_task_revision": 1, "policy_revision": 1,
                        "instruction_version": orientations[projects[0]]["instruction_version"]}, allowed=(200, 409))

                with ThreadPoolExecutor(max_workers=100) as workers:
                    raced = list(workers.map(race, range(100)))
                assert sum(status == 200 for status, _ in raced) == 1
                assert all(status == 200 or value.get("code") == "claim_conflict" for status, value in raced)
                winner_index = next(index for index, (status, _) in enumerate(raced) if status == 200)
                race_attempt = raced[winner_index][1]["claim"]["attempt"]
                sessions[winner_index % 50].call(f"/api/v1/projects/{projects[0]}/attempts/{race_attempt['id']}/release", {
                    "generation": race_attempt["generation"], "summary": "Claim race verified; fixture can be torn down.", "blocked": False})
                report["claim_burst"] = {"requests": 100, "sessions": 50, "winners": 1, "expected_conflicts": 99}

                blob = (b"bounded-capacity-artifact\n" * (16 * 1024 * 1024 // 26 + 1))[:16 * 1024 * 1024]
                _, upload = sessions[0].call(active[0][0] + "/artifacts/uploads", {"filename": "capacity.bin",
                    "media_type": "application/octet-stream", "size_bytes": len(blob),
                    "sha256": hashlib.sha256(blob).hexdigest(), "pinned": True})
                samples, errors, pending = defaultdict(list), Counter(), set()
                maxima = {"rss_bytes": 0, "inflight": 0, "schedule_lag_ms": 0.0}
                background = {}

                def request(index, scheduled):
                    api_index, slot = (index // 5) % 50, index % 5
                    api = sessions[api_index]
                    base, task, attempt = active[api_index]
                    if slot == 4:
                        kind = "renew"
                        path, body = base + f"/attempts/{attempt['id']}/renew", {"generation": attempt["generation"]}
                    else:
                        kind = ["task_list", "task_detail", "orientation", "context", "history"][(index // 5 + slot) % 5]
                        path = {"task_list": base + "/tasks?limit=50", "task_detail": base + f"/tasks/{task}",
                            "orientation": base + "/orientation", "context": base + "/context?q=capacity&limit=20&budget=8192",
                            "history": base + f"/tasks/{task}/history?kind=events&limit=50"}[kind]
                        body = None
                    try:
                        api.call(path, body)
                        return kind, (time.monotonic() - scheduled) * 1000, None
                    except Exception as error:
                        # Exception type and stable HTTP codes only, no request data or headers.
                        code = str(error) if isinstance(error, RuntimeError) else type(error).__name__
                        return kind, (time.monotonic() - scheduled) * 1000, code

                def upload_blob():
                    began = time.monotonic()
                    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
                    try:
                        connection.putrequest("PUT", upload["upload_path"])
                        for name, value in {**sessions[0].headers, "Idempotency-Key": identifier(),
                                "Content-Type": "application/octet-stream", "Content-Length": str(len(blob))}.items():
                            connection.putheader(name, value)
                        connection.endheaders()
                        for offset in range(0, len(blob), 65536):
                            connection.send(blob[offset:offset + 65536])
                            time.sleep(.01)
                        response = connection.getresponse()
                        value = json.loads(response.read())
                        assert response.status == 200 and value["data"]["artifact"]["sha256"] == hashlib.sha256(blob).hexdigest()
                        return {"passed": True, "bytes": len(blob), "seconds": round(time.monotonic() - began, 3)}
                    finally:
                        connection.close()

                def backup():
                    began = time.monotonic()
                    # preexec_fn is deliberately avoided after worker threads start.
                    result = subprocess.run(["taskset", "-c", ",".join(map(str, affinity)), "prlimit",
                        f"--as={memory_limit}", "--", *command, "backup", "--repository", str(temporary / "backups")],
                        capture_output=True, text=True, timeout=2700)
                    assert result.returncode == 0, "Concurrent backup failed; output withheld."
                    value = json.loads(result.stdout)
                    return {"passed": True, "seconds": round(time.monotonic() - began, 3),
                        "database_bytes": value["database_bytes"], "snapshot_bytes": value["snapshot_bytes"]}

                began, last_progress = time.monotonic(), 0
                total = int(args.duration * args.rate)
                print(f"Starting {'full acceptance' if report['full_acceptance'] else 'short development check'}: {args.history} historical tasks, 20 projects, 50 sessions, {args.rate} requests/s.", flush=True)
                with ThreadPoolExecutor(max_workers=100) as workers:
                    for index in range(total):
                        scheduled = began + index / args.rate
                        delay = scheduled - time.monotonic()
                        if delay > 0:
                            time.sleep(delay)
                        elapsed = time.monotonic() - began
                        maxima["schedule_lag_ms"] = max(maxima["schedule_lag_ms"], (time.monotonic() - scheduled) * 1000)
                        if elapsed >= min(30, args.duration / 3) and not background:
                            background = {"upload": workers.submit(upload_blob), "backup": workers.submit(backup)}
                        done = {future for future in pending if future.done()}
                        for future in done:
                            kind, latency, error = future.result()
                            samples[kind].append(latency)
                            if error:
                                errors[error] += 1
                        pending -= done
                        if len(pending) >= 100:
                            raise AssertionError("Metadata workload exceeded its 100-request inflight bound.")
                        pending.add(workers.submit(request, index, scheduled))
                        maxima["inflight"] = max(maxima["inflight"], len(pending))
                        if index % args.rate == 0:
                            status = Path(f"/proc/{process.pid}/status").read_text()
                            rss = next(int(line.split()[1]) * 1024 for line in status.splitlines() if line.startswith("VmRSS:"))
                            maxima["rss_bytes"] = max(maxima["rss_bytes"], rss)
                        if elapsed - last_progress >= 60:
                            last_progress = elapsed
                            print(f"Capacity exercise: {int(elapsed)}s, {sum(map(len, samples.values()))} completed requests, {sum(errors.values())} unexpected errors.", flush=True)
                    for future in pending:
                        kind, latency, error = future.result()
                        samples[kind].append(latency)
                        if error:
                            errors[error] += 1
                    elapsed = max(args.duration, time.monotonic() - began)
                    report["concurrent_operations"] = {kind: future.result() for kind, future in background.items()}
                report["total_including_background_seconds"] = round(time.monotonic() - began, 3)
                with sqlite3.connect(database) as connection:
                    assert connection.execute("SELECT count(*) FROM tasks WHERE lifecycle='canceled'").fetchone()[0] == args.history
                    assert connection.execute("SELECT task_id FROM attempts WHERE state='active' GROUP BY task_id HAVING count(*)>1").fetchone() is None
                    assert connection.execute("SELECT count(*) FROM attempts a JOIN tasks t ON t.current_attempt_id=a.id WHERE a.state='active' AND a.expires_at>? AND a.generation=t.generation", (int(time.time() * 1000),)).fetchone()[0] == 50
                    assert connection.execute("PRAGMA foreign_key_check").fetchone() is None
                combined = [value for values in samples.values() for value in values]
                report.update({"elapsed_seconds": round(elapsed, 3), "completed_requests": len(combined),
                    "achieved_requests_per_second": round(len(combined) / elapsed, 3),
                    "latency_includes_client_schedule_delay": True,
                    "metadata_p95_ms": percentile(combined, 95), "metadata_p99_ms": percentile(combined, 99),
                    "operations": {kind: {"count": len(values), "p95_ms": percentile(values, 95), "p99_ms": percentile(values, 99)} for kind, values in samples.items()},
                    "unexpected_errors": dict(errors), "maxima": maxima, "ownership_invariants_passed": True})
                assert not errors, "Unexpected workload errors; inspect the sanitized report."
                assert len(combined) == total and report["metadata_p95_ms"] < 500
                assert all(percentile(values, 95) < 500 for values in samples.values())
                assert len(combined) / elapsed >= args.rate * .98
                assert maxima["rss_bytes"] < memory_limit
                assert set(report["concurrent_operations"]) == {"upload", "backup"}
                report["passed"] = True
            finally:
                if process is not None:
                    stop(process)
    finally:
        if not report["passed"]:
            report["partial_operations"] = {kind: {"count": len(values), "p95_ms": percentile(values, 95),
                "p99_ms": percentile(values, 99)} for kind, values in samples.items()}
            report["unexpected_errors"] = dict(errors)
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(f"PASS: {report['completed_requests']} requests; p95 {report['metadata_p95_ms']}ms; peak service RSS {report['maxima']['rss_bytes'] // 1024 ** 2}MiB. Sanitized report: {args.report}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, default=ROOT / "target/release/agent-coordinator-server")
    parser.add_argument("--duration", type=int, default=1800)
    parser.add_argument("--rate", type=int, default=50)
    parser.add_argument("--history", type=int, default=100000)
    parser.add_argument("--report", type=Path, required=True)
    options = parser.parse_args()
    if not 10 <= options.duration <= 7200 or not 5 <= options.rate <= 100 or not 1000 <= options.history <= 100000:
        parser.error("Use duration 10..7200 seconds, rate 5..100, and history 1000..100000.")
    run(options)
