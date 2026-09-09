"""Local job acceptance scenario used by smoke.py; all files are disposable."""
import subprocess
import sys
import time


def exercise_jobs(temporary, api, cli, project, owner):
    source = temporary / "source repository with spaces"
    checkout = temporary / "task worktree with spaces"
    source.mkdir()

    def git(*args, cwd=source):
        result = subprocess.run(["git", "-C", str(cwd), *args], capture_output=True,
                                text=True, timeout=15)
        assert result.returncode == 0, f"Disposable Git setup failed: {args[0]}"
        return result.stdout.strip()

    git("init", "--initial-branch=main")
    git("config", "user.name", "Smoke fixture")
    git("config", "user.email", "smoke@example.invalid")
    git("remote", "add", "origin", "https://example.com/demo.git")
    (source / "README.md").write_text("Disposable local job fixture.\n")
    git("add", "README.md")
    git("commit", "-m", "Initial fixture")

    task = cli(owner, "tasks", "create", body={"title": "Run a guarded local job",
        "description": "Observe one producer without starting a duplicate.",
        "acceptance_criteria": ["One producer and a persistent physical hold"]})["data"]
    attempt = cli(owner, "claim", "--task", task["id"], "--revision",
                  str(task["revision"]))["data"]["claim"]["attempt"]
    ownership = ["--attempt", attempt["id"], "--generation", str(attempt["generation"])]
    prepare = ["worktree", "prepare", *ownership, "--source", str(source),
               "--path", str(checkout), "--branch", "task/guarded", "--base", "main"]
    cli(owner, *prepare)
    cli(owner, *prepare)  # Reconcile the existing intent and checkout.
    assert len([line for line in git("worktree", "list", "--porcelain").splitlines()
                if line.startswith("worktree ")]) == 2
    resource = api("/api/v1/resources", {"key": "smoke/shared-device", "capacity": 1,
                                        "description": "Disposable exclusive test device"})
    reservation_data = cli(owner, "resources", "reserve", *ownership,
                body={"items": [{"resource_id": resource["id"], "units": 1}]})["data"]
    reservation = reservation_data.get("reservation", reservation_data)["id"]
    launches = temporary / "producer-starts.txt"
    finish = temporary / "allow-producer-exit"
    producer = temporary / "producer.py"
    producer.write_text("""import pathlib, sys, time
starts, finish = map(pathlib.Path, sys.argv[1:])
with starts.open('a') as output:
    output.write('started\\n')
    output.flush()
deadline = time.monotonic() + 40
while not finish.exists() and time.monotonic() < deadline:
    time.sleep(.05)
if not finish.exists():
    sys.exit(19)
print('producer completed')
""")
    cli(owner, "jobs", "run", *ownership, "--reservation", reservation,
        "--checkout", str(checkout), body={"label": "Smoke producer",
        "program": sys.executable, "argv": [str(producer), str(launches), str(finish)],
        "environment": {}, "log_limit_bytes": 1048576})
    jobs = api(f"/api/v1/projects/{project}/jobs")["items"]
    assert len(jobs) == 1
    job_id = jobs[0]["id"]

    def wait_for(predicate, message):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.1)
        raise AssertionError(message)

    try:
        wait_for(lambda: launches.exists(), "Registered guardian did not launch the producer.")
        cli(owner, "jobs", "reconnect", "--job", job_id)
        cli(owner, "jobs", "reconnect", "--job", job_id)
        local = cli(owner, "jobs", "inspect", "--job", job_id)
        assert "error" not in local
        assert launches.read_text().splitlines() == ["started"], "Observer reconnect relaunched work."
        # A live job must prevent an ordinary handoff from requeueing the task.
        rejected = cli(owner, "release", *ownership,
                       body={"summary": "Must not requeue a live producer."}, expected=5)
        assert "error" in rejected
        detail = api(f"/api/v1/projects/{project}/tasks/{task['id']}")
        assert detail["job_evidence"]["reservations"], "Physical hold missing from task evidence."
        assert api("/api/v1/resources")["items"][0]["held_units"] == 1
    finally:
        finish.touch()
    wait_for(lambda: api(f"/api/v1/projects/{project}/jobs/{job_id}")["state"] == "succeeded",
             "Terminal producer result did not reach the service.")
    assert launches.read_text().splitlines() == ["started"]
    # A terminal report alone does not release capacity.
    assert api("/api/v1/resources")["items"][0]["held_units"] == 1
    cli(owner, "reservations", "release", "--reservation", reservation,
        "--generation", str(attempt["generation"]), "--reason", "Verified terminal result.")
    cli(owner, "release", *ownership, body={"summary": "Producer finished and capacity released."})
    assert api("/api/v1/resources")["items"][0]["held_units"] == 0
    print("PASS: worktree reconciliation with spaces, local guardian, duplicate-free reconnect,")
    print("      task job evidence, live-work release rejection, terminal report, explicit capacity release.")
