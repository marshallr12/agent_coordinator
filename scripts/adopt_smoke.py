"""Exercise `agent-coordinator adopt` with the built CLI against the disposable service.

A throwaway Git repository is adopted end to end: a dry run, an unattended run
that reports what it needs, a real run that waits while the "human" creates the
project through the operator API session, a `connect` from the adopted
directory, and a second run that finds nothing left to do. No existing
repository, credential or service state is used.
"""
import json
import os
import subprocess
import threading

REMOTE = "https://example.com/acme/adopted.git"
LINK = "#/new-project?name=adopted&repository_url=https%3A%2F%2Fexample.com%2Facme%2Fadopted.git&target_branch=main"


def git(repo, *args):
    """Runs Git in the repository and returns its stdout."""
    result = subprocess.run(["git", "-C", str(repo), *args], text=True, capture_output=True, timeout=30)
    assert result.returncode == 0, f"git {args[0]} failed: {result.stderr}"
    return result.stdout


def make_repository(path):
    """Creates a repository with one commit, a local identity and an `origin` remote."""
    path.mkdir()
    git(path, "init", "--quiet", "--initial-branch=main")
    for name, value in (("user.name", "Adopt Smoke"), ("user.email", "adopt@example.invalid"),
                        ("commit.gpgsign", "false")):
        git(path, "config", name, value)
    (path / "README.md").write_text("# Adopted\n")
    git(path, "add", "README.md")
    git(path, "commit", "--quiet", "-m", "Start")
    git(path, "remote", "add", "origin", REMOTE)


def environment(temporary, origin, token):
    """The environment of a workstation that already holds a credential for the service."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("AGENT_COORDINATOR_")}
    home = temporary / "adopt-home"
    if os.name == "nt":
        env["AGENT_COORDINATOR_STATE_DIR"] = str(home / "sessions")
    else:
        env["AGENT_COORDINATOR_HOME"] = str(home)
    env.update(AGENT_COORDINATOR_TOKEN=token, AGENT_COORDINATOR_ORIGIN=origin)
    return env


def adopt_command(cli, repo, origin, *flags):
    """The adopt command line: every run names the repository and the disposable service."""
    return [str(cli), "--allow-insecure-loopback", "--json", "adopt", str(repo),
            "--service-url", origin, "--harness", "both", *flags]


def run_adopt(cli, repo, env, origin, token, *flags, expected=0):
    """Runs adopt to completion and returns its JSON event lines; the token must never be printed."""
    result = subprocess.run(adopt_command(cli, repo, origin, *flags), text=True, capture_output=True,
                            env=env, timeout=60)
    assert token not in result.stdout + result.stderr, "Token leaked into adopt output."
    assert result.returncode == expected, f"adopt returned {result.returncode}, expected {expected}: {result.stdout[-400:]}"
    return [json.loads(line) for line in result.stdout.splitlines() if line.strip()]


def snapshot(repo):
    """Every file outside .git with its bytes, plus HEAD, to prove a run changed nothing."""
    files = {str(path.relative_to(repo)): path.read_bytes()
             for path in sorted(repo.rglob("*")) if path.is_file() and ".git" not in path.relative_to(repo).parts}
    return files, git(repo, "rev-parse", "HEAD")


def create_while_waiting(cli, repo, env, origin, token, api):
    """Starts adopt, creates the project when it shows the link, and returns (result, project id)."""
    command = adopt_command(cli, repo, origin, "--no-browser", "--yes", "--approval-timeout", "60")
    process = subprocess.Popen(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    watchdog = threading.Timer(90, process.kill)
    watchdog.start()
    try:
        events = (json.loads(line) for line in iter(process.stdout.readline, ""))
        waiting = next(event for event in events if event.get("status") == "waiting")
        assert waiting["link"] == origin + "/" + LINK, f"Unexpected dashboard link {waiting['link']}"
        created = api("/api/v1/projects", waiting["manual"])["id"]
        rest, errors = process.communicate(timeout=60)
    finally:
        watchdog.cancel()
    assert process.returncode == 0, f"adopt failed after the project was created: {rest[-400:]}"
    assert token not in rest + errors, "Token leaked into adopt output."
    return json.loads(rest.strip().splitlines()[-1])["result"], created


def check_adopted(repo, origin, token, result, created):
    """The repository is bound to the created project, instructed, committed and clean."""
    assert result["status"] == "connected" and result["project_id"] == created
    binding = (repo / ".agent-coordinator.toml").read_text()
    assert binding == f'service_url = "{origin}"\nproject_id = "{created}"\n', "Unexpected binding file."
    agents = (repo / "AGENTS.md").read_text()
    assert agents.startswith("<!-- agent-coordinator:begin v1 -->") and "<!-- agent-coordinator:end -->" in agents
    assert (repo / "CLAUDE.md").read_text() == "@AGENTS.md\n"
    assert git(repo, "status", "--porcelain") == "", "adopt left uncommitted files."
    assert git(repo, "log", "-1", "--format=%s").strip() == "Connect repository to Agent Coordinator"
    assert result["commit"] == git(repo, "rev-parse", "HEAD").strip()
    files, _ = snapshot(repo)
    assert all(token.encode() not in content for content in files.values()), "Token written into the repository."


def connect_from(cli, repo, env, created):
    """The very next session in the adopted repository connects to the created project."""
    command = [str(cli), "--session", "adopt-smoke", "--allow-insecure-loopback", "--json", "connect"]
    result = subprocess.run(command, text=True, capture_output=True, env=env, cwd=repo, timeout=30)
    assert result.returncode == 0, f"connect in the adopted repository failed: {result.stdout[-400:]}"
    orientation = json.loads(result.stdout)["data"]["orientation"]
    assert orientation["project"]["id"] == created and orientation["instructions_complete"]


def exercise_adopt(temporary, api, origin, token, cli):
    """Adopts a throwaway repository and proves the next session can connect."""
    repo = temporary / "adopted"
    make_repository(repo)
    env = environment(temporary, origin, token)
    before = snapshot(repo)
    dry = run_adopt(cli, repo, env, origin, token, "--dry-run")
    assert dry[-1]["result"]["status"] == "dry_run" and snapshot(repo) == before, "A dry run changed the repository."
    needs = run_adopt(cli, repo, env, origin, token, expected=2)[-1]["error"]
    assert needs["code"] == "needs_input" and "--yes" in needs["message"] and snapshot(repo) == before
    result, created = create_while_waiting(cli, repo, env, origin, token, api)
    check_adopted(repo, origin, token, result, created)
    connect_from(cli, repo, env, created)
    after = snapshot(repo)
    again = run_adopt(cli, repo, env, origin, token)[-1]["result"]
    assert again["status"] == "already_connected" and again["files_written"] == [] and snapshot(repo) == after
