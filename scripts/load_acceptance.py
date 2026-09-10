#!/usr/bin/env python3
"""Disposable Linux capacity exercise; no existing service or credentials are used.

The default acceptance run is 30 minutes at 50 requests/second. Short runs are
development checks and are labeled as such. Historical fixtures are seeded in
SQLite while the service is stopped; all measured operations use HTTP.
"""
import argparse
from collections import Counter, defaultdict, deque
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

from capacity_restore import restore_stage

ROOT = Path(__file__).resolve().parents[1]
BASELINE_MEMORY_BYTES = 4 * 1024 ** 3


def identifier():
    return str(uuid.uuid4())


def percentile(values, percent):
    return round(sorted(values)[max(0, math.ceil(len(values) * percent / 100) - 1)], 3) if values else None


def os_release():
    values = {}
    for line in Path("/etc/os-release").read_text().splitlines():
        if "=" in line:
            name, value = line.split("=", 1)
            values[name] = value.strip().strip('"')
    return values


def cgroup_v2():
    relative = None
    for line in Path("/proc/self/cgroup").read_text().splitlines():
        fields = line.split(":", 2)
        if len(fields) == 3 and fields[0] == "0" and fields[1] == "":
            relative = fields[2]
            break
    if relative is None:
        return {"version": None, "available": False}
    directory = Path("/sys/fs/cgroup") / relative.lstrip("/")

    def value(name):
        path = directory / name
        return path.read_text().strip() if path.is_file() else None

    memory_max = value("memory.max")
    swap_max = value("memory.swap.max")
    cpu_max = value("cpu.max")
    quota, period = (cpu_max or "").split() if cpu_max and len(cpu_max.split()) == 2 else (None, None)
    cpu_stat = {}
    for line in (value("cpu.stat") or "").splitlines():
        key, count = line.split()
        if key in {"usage_usec", "user_usec", "system_usec", "nr_periods", "nr_throttled", "throttled_usec"}:
            cpu_stat[key] = int(count)
    return {
        "version": 2,
        "cpu_stat": cpu_stat,
        "available": (directory / "cgroup.controllers").is_file(),
        "memory_max_bytes": None if memory_max in (None, "max") else int(memory_max),
        "memory_swap_max_bytes": None if swap_max in (None, "max") else int(swap_max),
        "memory_current_bytes": None if value("memory.current") is None else int(value("memory.current")),
        "memory_peak_bytes": None if value("memory.peak") is None else int(value("memory.peak")),
        "cpu_quota_micros": None if quota in (None, "max") else int(quota),
        "cpu_period_micros": None if period is None else int(period),
    }


def baseline_status(source_clean, affinity):
    release = os_release()
    cgroup = cgroup_v2()
    cpu_quota = cgroup.get("cpu_quota_micros")
    cpu_period = cgroup.get("cpu_period_micros")
    quota_cores = None if cpu_quota is None or not cpu_period else cpu_quota / cpu_period
    checks = {
        "ubuntu_24_04": release.get("ID") == "ubuntu" and release.get("VERSION_ID") == "24.04",
        "x86_64": platform.machine() == "x86_64",
        "source_clean": source_clean,
        "two_cpus_available": len(affinity) >= 2,
        "cgroup_v2": cgroup.get("available") is True,
        "aggregate_memory_max_at_most_4_gib": cgroup.get("memory_max_bytes") is not None
            and 0 < cgroup["memory_max_bytes"] <= BASELINE_MEMORY_BYTES,
        "aggregate_swap_disabled": cgroup.get("memory_swap_max_bytes") == 0,
        "aggregate_cpu_quota_at_most_two_cores": quota_cores is not None and 0 < quota_cores <= 2,
        "aggregate_memory_peak_available": cgroup.get("memory_peak_bytes") is not None,
    }
    cgroup["cpu_quota_cores"] = None if quota_cores is None else round(quota_cores, 3)
    return {
        "verified": all(checks.values()),
        "checks": checks,
        "os_id": release.get("ID"),
        "os_version_id": release.get("VERSION_ID"),
        "architecture": platform.machine(),
        "cgroup": cgroup,
    }


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
    if not __debug__:
        raise SystemExit("Run the acceptance exercise without Python optimization so assertions remain active.")
    if platform.system() != "Linux":
        raise SystemExit("This acceptance exercise requires native Linux.")
    server = args.server.resolve()
    if not server.is_file():
        raise SystemExit("Build the service first or select --server.")
    available_affinity = sorted(os.sched_getaffinity(0))
    affinity = available_affinity[:2]
    if len(affinity) != 2:
        raise SystemExit("Two available CPU cores are required.")
    memory_limit = BASELINE_MEMORY_BYTES

    def constrain_service():
        os.sched_setaffinity(0, affinity)
        resource.setrlimit(resource.RLIMIT_AS, (memory_limit, memory_limit))

    report = {"schema_version": 2, "passed": False, "full_acceptance": False,
        "baseline_required": args.require_baseline,
        "duration_requested_seconds": args.duration, "requests_per_second": args.rate,
        "projects": 20, "agent_sessions": 50, "historical_tasks": args.history,
        "historical_fixture": "Synthetic canceled tasks, definition revisions, and audit events; seeded with service stopped.",
        "host": {"os": platform.platform(), "architecture": platform.machine(),
            "service_cpu_cores": 2, "service_address_space_limit_bytes": memory_limit,
            "memory_limit_kind": "RLIMIT_AS per service/backup/restore process; cgroup v2 is required for aggregate baseline acceptance"},
        "transport": "Authenticated loopback HTTP; HTTPS installation is exercised separately."}
    samples, errors = defaultdict(list), Counter()
    diagnostics = deque(maxlen=120)
    background_progress = {"upload_started": False, "upload_finished": False,
        "backup_started": False, "backup_finished": False}
    try:
        report["source_commit"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        report["source_dirty"] = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True))
        report["executable_sha256"] = hashlib.sha256(server.read_bytes()).hexdigest()
        github = {name.lower(): os.environ[name] for name in (
            "GITHUB_REPOSITORY", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_SHA",
            "GITHUB_REF", "GITHUB_WORKFLOW", "GITHUB_JOB") if os.environ.get(name)}
        if github:
            report["github"] = github
        baseline = baseline_status(not report["source_dirty"], available_affinity)
        if github:
            baseline["checks"]["github_head_matches_source"] = (
                github.get("github_sha") == report["source_commit"])
            baseline["verified"] = all(baseline["checks"].values())
        report["baseline"] = baseline
        full_workload = args.duration >= 1800 and args.rate == 50 and args.history == 100000
        report["full_workload"] = full_workload
        report["full_acceptance"] = full_workload and baseline["verified"]
        if args.require_baseline and not baseline["verified"]:
            failed = sorted(name for name, passed in baseline["checks"].items() if not passed)
            raise AssertionError("Required Linux baseline checks failed: " + ", ".join(failed))
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
                    active.append({"base": base, "project_id": project, "task_id": task["id"],
                        "attempt": claim["claim"]["attempt"], "session_id": session_id})
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
                _, upload = sessions[0].call(active[0]["base"] + "/artifacts/uploads", {"filename": "capacity.bin",
                    "media_type": "application/octet-stream", "size_bytes": len(blob),
                    "sha256": hashlib.sha256(blob).hexdigest(), "pinned": True})
                samples, errors, pending = defaultdict(list), Counter(), set()
                maxima = {"rss_bytes": 0, "inflight": 0, "schedule_lag_ms": 0.0}
                background = {}
                completed_at = []
                renewals_by_session = Counter()
                scheduled_operations = Counter()
                scheduled_renewals_by_session = Counter()
                upload_streaming = threading.Event()
                total = int(args.duration * args.rate)

                def operation(index):
                    api_index, slot = (index // 5) % 50, index % 5
                    if slot == 4:
                        return api_index, "renew"
                    kind = ["task_list", "task_detail", "orientation", "context", "history"][(index // 5 + slot) % 5]
                    return api_index, kind

                def request(index, scheduled):
                    api_index, kind = operation(index)
                    api = sessions[api_index]
                    item = active[api_index]
                    if kind == "renew":
                        attempt = item["attempt"]
                        path, body = item["base"] + f"/attempts/{attempt['id']}/renew", {"generation": attempt["generation"]}
                    else:
                        path = {"task_list": item["base"] + "/tasks?limit=50",
                            "task_detail": item["base"] + f"/tasks/{item['task_id']}",
                            "orientation": item["base"] + "/orientation",
                            "context": item["base"] + "/context?q=capacity&limit=20&budget=8192",
                            "history": item["base"] + f"/tasks/{item['task_id']}/history?kind=events&limit=50"}[kind]
                        body = None
                    try:
                        api.call(path, body)
                        return kind, api_index, (time.monotonic() - scheduled) * 1000, None, time.monotonic()
                    except Exception as error:
                        # Exception type and stable HTTP codes only, no request data or headers.
                        code = str(error) if isinstance(error, RuntimeError) else type(error).__name__
                        return kind, api_index, (time.monotonic() - scheduled) * 1000, code, time.monotonic()

                def collect(future):
                    kind, api_index, latency, error, finished = future.result()
                    samples[kind].append(latency)
                    completed_at.append(finished)
                    if error:
                        errors[error] += 1
                    elif kind == "renew":
                        renewals_by_session[api_index] += 1

                def upload_blob():
                    started = time.monotonic()
                    background_progress["upload_started"] = True
                    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
                    try:
                        connection.putrequest("PUT", upload["upload_path"])
                        for name, value in {**sessions[0].headers, "Idempotency-Key": identifier(),
                                "Content-Type": "application/octet-stream", "Content-Length": str(len(blob))}.items():
                            connection.putheader(name, value)
                        connection.endheaders()
                        for offset in range(0, len(blob), 65536):
                            connection.send(blob[offset:offset + 65536])
                            if offset == 0:
                                upload_streaming.set()
                            time.sleep(.01)
                        response = connection.getresponse()
                        value = json.loads(response.read())
                        assert response.status == 200 and value["data"]["artifact"]["sha256"] == hashlib.sha256(blob).hexdigest()
                        finished = time.monotonic()
                        background_progress["upload_finished"] = True
                        return {"started": started, "finished": finished, "report": {
                            "passed": True, "bytes": len(blob), "seconds": round(finished - started, 3)}}
                    finally:
                        connection.close()

                def backup():
                    assert upload_streaming.wait(timeout=30), "Artifact upload did not begin before backup."
                    started = time.monotonic()
                    background_progress["backup_started"] = True
                    # preexec_fn is deliberately avoided after worker threads start.
                    result = subprocess.run(["taskset", "-c", ",".join(map(str, affinity)), "prlimit",
                        f"--as={memory_limit}", "--", *command, "backup", "--repository", str(temporary / "backups")],
                        capture_output=True, text=True, timeout=2700)
                    assert result.returncode == 0, "Concurrent backup failed; output withheld."
                    value = json.loads(result.stdout)
                    finished = time.monotonic()
                    background_progress["backup_finished"] = True
                    snapshot = Path(value["snapshot_path"])
                    assert snapshot.is_dir(), "Concurrent backup did not publish its completed snapshot."
                    return {"started": started, "finished": finished, "snapshot": snapshot, "report": {
                        "passed": True, "seconds": round(finished - started, 3),
                        "database_bytes": value["database_bytes"],
                        "snapshot_bytes": value["snapshot_bytes"]}}

                began, last_progress = time.monotonic(), 0
                label = "full acceptance" if report["full_acceptance"] else (
                    "full workload without verified baseline" if full_workload else "short development check")
                print(f"Starting {label}: {args.history} historical tasks, 20 projects, 50 sessions, {args.rate} requests/s.", flush=True)
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
                            collect(future)
                        pending -= done
                        if len(pending) >= 100:
                            raise AssertionError("Metadata workload exceeded its 100-request inflight bound.")
                        api_index, kind = operation(index)
                        scheduled_operations[kind] += 1
                        if kind == "renew":
                            scheduled_renewals_by_session[api_index] += 1
                        pending.add(workers.submit(request, index, scheduled))
                        maxima["inflight"] = max(maxima["inflight"], len(pending))
                        if index % args.rate == 0:
                            status = Path(f"/proc/{process.pid}/status").read_text()
                            rss = next(int(line.split()[1]) * 1024 for line in status.splitlines() if line.startswith("VmRSS:"))
                            maxima["rss_bytes"] = max(maxima["rss_bytes"], rss)
                            fields = Path(f"/proc/{process.pid}/stat").read_text().rsplit(")", 1)[1].split()
                            diagnostics.append({"elapsed_seconds": round(elapsed, 3),
                                "completed_requests": sum(map(len, samples.values())),
                                "pending_requests": len(pending), "service_rss_bytes": rss,
                                "service_cpu_seconds": round((int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"), 3),
                                "aggregate_cpu_stat": cgroup_v2().get("cpu_stat", {}),
                                **background_progress})
                        if elapsed - last_progress >= 60:
                            last_progress = elapsed
                            print(f"Capacity exercise: {int(elapsed)}s, {sum(map(len, samples.values()))} completed requests, {sum(errors.values())} unexpected errors.", flush=True)
                    for future in pending:
                        collect(future)
                    metadata_finished = time.monotonic()
                    elapsed = max(args.duration, time.monotonic() - began)
                    background_results = {kind: future.result() for kind, future in background.items()}
                upload_result, backup_result = background_results["upload"], background_results["backup"]
                overlap_start = max(upload_result["started"], backup_result["started"])
                overlap_end = min(upload_result["finished"], backup_result["finished"])
                assert overlap_start < overlap_end, "Artifact upload and backup did not overlap."
                assert upload_result["started"] < metadata_finished and backup_result["started"] < metadata_finished
                if full_workload:
                    assert upload_result["finished"] <= metadata_finished
                    assert backup_result["finished"] <= metadata_finished
                    assert any(overlap_start <= completed <= overlap_end for completed in completed_at), \
                        "No measured metadata request completed while upload and backup overlapped."
                report["concurrent_operations"] = {
                    "upload": {**upload_result["report"],
                        "started_offset_seconds": round(upload_result["started"] - began, 3),
                        "finished_offset_seconds": round(upload_result["finished"] - began, 3)},
                    "backup": {**backup_result["report"],
                        "started_offset_seconds": round(backup_result["started"] - began, 3),
                        "finished_offset_seconds": round(backup_result["finished"] - began, 3)},
                    "overlap_seconds": round(overlap_end - overlap_start, 3)}
                report["total_including_background_seconds"] = round(time.monotonic() - began, 3)
                expected_ownership = {(item["project_id"], item["task_id"], item["attempt"]["id"],
                    item["attempt"]["generation"], credential["principal_id"], item["session_id"],
                    credential["credential_id"]) for item in active}
                with sqlite3.connect(database) as connection:
                    assert connection.execute("SELECT count(*) FROM tasks WHERE lifecycle='canceled'").fetchone()[0] == args.history
                    rows = connection.execute("SELECT a.project_id,a.task_id,a.id,a.generation,a.owner_id,a.session_id,a.credential_id,"
                        "t.current_attempt_id,t.generation,a.expires_at FROM attempts a JOIN tasks t ON t.project_id=a.project_id AND t.id=a.task_id "
                        "WHERE a.state='active'").fetchall()
                    actual_ownership = {tuple(row[:7]) for row in rows}
                    assert actual_ownership == expected_ownership
                    assert len(rows) == 50 and all(row[7] == row[2] and row[8] == row[3]
                        and row[9] > int(time.time() * 1000) for row in rows)
                    artifact = connection.execute("SELECT project_id,state,created_by,size_bytes,sha256 FROM artifacts WHERE id=?",
                        (upload["artifact"]["id"],)).fetchone()
                    assert artifact == (active[0]["project_id"], "finalized", credential["principal_id"],
                        len(blob), hashlib.sha256(blob).hexdigest())
                    assert connection.execute("PRAGMA foreign_key_check").fetchone() is None
                combined = [value for values in samples.values() for value in values]
                operation_counts = Counter({kind: len(values) for kind, values in samples.items()})
                read_count = sum(count for kind, count in operation_counts.items() if kind != "renew")
                write_count = operation_counts["renew"]
                sessions_with_renewals = len(renewals_by_session)
                report.update({"elapsed_seconds": round(elapsed, 3), "completed_requests": len(combined),
                    "achieved_requests_per_second": round(len(combined) / elapsed, 3),
                    "load_model": "open_loop_fixed_schedule", "latency_includes_client_schedule_delay": True,
                    "metadata_p95_ms": percentile(combined, 95), "metadata_p99_ms": percentile(combined, 99),
                    "operations": {kind: {"count": len(values), "p95_ms": percentile(values, 95), "p99_ms": percentile(values, 99)} for kind, values in samples.items()},
                    "workload_mix": {"reads": read_count, "writes": write_count,
                        "scheduled_reads_per_second": round(read_count / args.duration, 3),
                        "scheduled_writes_per_second": round(write_count / args.duration, 3),
                        "sessions_with_successful_renewals": sessions_with_renewals,
                        "successful_renewals_per_participating_session_min": min(renewals_by_session.values(), default=0),
                        "successful_renewals_per_participating_session_max": max(renewals_by_session.values(), default=0)},
                    "unexpected_errors": dict(errors), "maxima": maxima,
                    "ownership_invariants_passed": True, "artifact_ownership_invariant_passed": True})
                assert not errors, "Unexpected workload errors; inspect the sanitized report."
                assert operation_counts == scheduled_operations
                assert renewals_by_session == scheduled_renewals_by_session
                if full_workload:
                    assert read_count == args.duration * 40 and write_count == args.duration * 10
                    assert sessions_with_renewals == 50
                assert len(combined) == total and report["metadata_p95_ms"] < 500
                assert all(percentile(values, 95) < 500 for values in samples.values())
                assert len(combined) / elapsed >= args.rate * .98
                assert maxima["rss_bytes"] < memory_limit
                assert set(report["concurrent_operations"]) == {"upload", "backup", "overlap_seconds"}
                stop(process)
                process = None
                report["restore_stage"] = restore_stage(
                    server, backup_result["snapshot"], temporary / "restored", affinity, args.history)
                report["passed"] = True
            finally:
                if process is not None:
                    stop(process)
    finally:
        aggregate = cgroup_v2()
        report["host"]["aggregate_cgroup"] = aggregate
        report["recent_diagnostics"] = list(diagnostics)
        report["background_progress"] = background_progress
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
    parser.add_argument("--require-baseline", action="store_true",
        help="fail unless the clean Ubuntu 24.04 x86_64 run inherits the aggregate two-core, 4 GiB, no-swap cgroup v2 baseline")
    options = parser.parse_args()
    if not 10 <= options.duration <= 7200 or not 5 <= options.rate <= 100 or not 1000 <= options.history <= 100000:
        parser.error("Use duration 10..7200 seconds, rate 5..100, and history 1000..100000.")
    run(options)
