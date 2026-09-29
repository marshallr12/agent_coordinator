#!/usr/bin/env python3
"""Integrator staging soak (autonomy plan P4 step S5).

    deploy/agentc/soak.py run                         # every scenario, then the report
    deploy/agentc/soak.py run --scenario conflicts    # one scenario
    deploy/agentc/soak.py run --minutes 30            # the scenarios, then a random mix
    deploy/agentc/soak.py list                        # scenario names

Build first (one build, so the CLI passes the service's exact-client check):

    COORDINATOR_BUILD_COMMIT=$(git rev-parse HEAD) cargo build --locked \\
        -p coordinator-server -p coordinator-cli -p agentc-integrator

Each run creates a disposable instance in its own directory (default
$XDG_STATE_HOME/agentc-soak, emptied at the start of every run): a server on
127.0.0.1:18091, a bare remote seeded from this repository's HEAD plus a
roster commit, a project owned by the deterministic integrator, and the
credentials in ROLES (impl-a, impl-b, rev-1, rev-2: supervised/write;
integrator).
No LLM is involved: scripted agents drive the real `agent-coordinator` CLI
(one HOME per role, as staging.py does) and `agentc-integrator run` is a
child process with file-backed fake checks the soak edits live.

After every scenario the soak checks the invariants (fast-forward-only target
history, published results contained in the target, one landing per subject,
no report that requires a human, an empty human queue for both roles, no
parked or stuck subject, no integrator panic) and writes soak-report.json and
soak-report.md into the directory. HRI counts human-required interventions
caused by the system; a revert the soak issues as a human is a scenario
input. Latency is the time from the approving review decision to the
integrator's `published` observation. `--keep` keeps the database, remote
and clones after the run; the reports and logs are always kept.

Every child process gets an environment with all AGENT_COORDINATOR_* and
COORDINATOR_* variables removed (staging.clean_env); tokens stay in 0600
files under the directory and are never printed. The server and integrator
are stopped on exit, including Ctrl-C. Python's standard library is enough.
"""
import argparse
import collections
from datetime import datetime
import json
import os
from pathlib import Path
import random
import shutil
import signal
import sqlite3
import subprocess
import sys
import time
import tomllib
import traceback
import types
import urllib.request

sys.dont_write_bytecode = True  # no __pycache__ beside the deploy scripts
sys.path.insert(0, str(Path(__file__).resolve().parent))
import staging  # noqa: E402  (sibling module)

ROOT = staging.ROOT
ROLES = {"impl-a": ("supervised", "write"), "impl-b": ("supervised", "write"),
         "rev-1": ("supervised", "write"), "rev-2": ("supervised", "write"),
         "integrator": ("integrator", "write")}
CHECK = "Linux tests"
ROSTER = ('[[required_checks]]\nidentity = "tests"\n'
          f'check_name = "{CHECK}"\nworkflow_path = ".github/workflows/ci.yml"\n')
REQUIRED_CHECKS = [{"identity": "tests", "version": "v1", "environment": "fake"}]
RULES = ["non_fast_forward", "required_status_checks"]
CRITERION = "The scripted change is committed"
AGENT_TRAILER = "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
MARKER = ".agentc-soak"
LANDED = ("published", "already_contained", "published_after_reopen")
# Park thresholds, mirroring autonomy.rs REVISE_LIMIT and SERIALIZED_REVISE_CAP.
REVISE_LIMIT, SERIALIZED_REVISE_CAP = 3, 6
LATENCY_GOAL_MS = 10 * 60 * 1000


class SoakFailure(Exception):
    """A scenario step or assertion that did not hold."""


def now_ms():
    """Wall-clock milliseconds, the unit of the server's timestamps."""
    return int(time.time() * 1000)


def git(cwd, *args, check=True):
    """Runs git in `cwd` with a clean environment; returns stripped stdout."""
    result = subprocess.run(["git", *args], cwd=cwd, text=True, capture_output=True,
                            env=staging.clean_env(), timeout=120)
    if check and result.returncode != 0:
        output = (result.stderr.strip() or result.stdout.strip())[:300]
        raise SoakFailure(f"git {' '.join(args[:3])}: {output}")
    return result.stdout.strip()


def is_ancestor(repo, older, newer):
    """True when `older` is an ancestor of (or equal to) `newer` in `repo`."""
    result = subprocess.run(["git", "merge-base", "--is-ancestor", older, newer], cwd=repo,
                            capture_output=True, env=staging.clean_env(), timeout=60)
    return result.returncode == 0


class Deadline:
    """The time a scenario may still take."""

    def __init__(self, seconds):
        """A deadline `seconds` from now."""
        self.end = time.monotonic() + seconds

    def wait(self, check, what, interval=0.5):
        """Polls `check` until it returns a truthy value, which is returned;
        raises SoakFailure naming `what` once the deadline passes."""
        while True:
            value = check()
            if value:
                return value
            if time.monotonic() > self.end:
                raise SoakFailure(f"timed out waiting for {what}")
            time.sleep(interval)


class FakeChecks:
    """The integrator's fake checks file. The integrator rewrites it when it
    records a rerun, so reads retry a torn file and every edit is replaced
    atomically and verified until it sticks."""

    def __init__(self, path):
        """The fake checks file at `path`."""
        self.path = path

    def load(self):
        """The file's current contents (retrying a partially written file)."""
        for _ in range(50):
            try:
                return json.loads(self.path.read_text())
            except (json.JSONDecodeError, FileNotFoundError):
                time.sleep(0.05)
        raise SoakFailure(f"unreadable {self.path}")

    def save(self, data):
        """Replaces the file atomically."""
        temporary = self.path.with_suffix(".tmp")
        temporary.write_text(json.dumps(data, indent=1))
        os.replace(temporary, self.path)

    def edit(self, change):
        """Applies the idempotent `change` (a dict mutator) until a re-read
        shows it survived a concurrent integrator write."""
        for _ in range(10):
            data = self.load()
            change(data)
            self.save(data)
            time.sleep(0.2)
            again = self.load()
            expected = json.loads(json.dumps(again))
            change(expected)
            if expected == again:
                return
        raise SoakFailure("fake checks edit did not stick")

    def default(self, conclusion):
        """Sets the conclusion of every unscripted check (None = pending)."""
        self.edit(lambda d: d.__setitem__("default_conclusion", conclusion))

    def script(self, sha, outcomes):
        """Scripts the per-attempt conclusions of the roster check on `sha`."""
        self.edit(lambda d: d.setdefault("scripts", {}).__setitem__(sha, {CHECK: outcomes}))

    def reruns(self):
        """Reruns recorded over all synthetic runs."""
        return sum(self.load().get("reruns", {}).values())


class Integrator:
    """The `agentc-integrator run` child process and its logs."""

    def __init__(self, soak):
        """An integrator of `soak`, not started yet."""
        self.soak = soak
        self.process = None
        self.restarts = 0

    def start(self):
        """Starts the integrator in its own process group."""
        log = open(self.soak.path("integrator.log"), "ab")
        err = open(self.soak.path("integrator.err"), "ab")
        command = [str(self.soak.args.integrator), "--config",
                   str(self.soak.path("integrator.toml")), "run"]
        self.process = subprocess.Popen(command, stdout=log, stderr=err,
                                        stdin=subprocess.DEVNULL, env=staging.clean_env(),
                                        cwd=self.soak.dir, start_new_session=True)

    def kill(self, group=False):
        """SIGKILLs the integrator (with `group`, also its git children)."""
        if self.process is None or self.process.poll() is not None:
            return
        if group:
            os.killpg(self.process.pid, signal.SIGKILL)
        else:
            self.process.kill()
        self.process.wait(timeout=30)

    def restart(self, group=False):
        """Kills and restarts the integrator; counts the restart."""
        self.kill(group)
        self.restarts += 1
        self.start()

    def stop(self):
        """Stops the integrator and anything left in its process group."""
        if self.process is not None:
            try:
                os.killpg(self.process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            self.process.wait(timeout=30)

    def steps(self, since_ms, until_ms):
        """Counts of the cycle steps logged in [since_ms, until_ms], detail trimmed."""
        counts = collections.Counter()
        for line in self.soak.path("integrator.log").read_text().splitlines():
            entry = json.loads(line)
            if since_ms <= iso_ms(entry["time"]) <= until_ms:
                counts[trim_step(entry["step"])] += 1
        return counts

    def panics(self):
        """Panic lines in the integrator's stderr and stdout."""
        text = "".join(self.soak.path(name).read_text(errors="replace")
                       for name in ("integrator.err", "integrator.log"))
        return [line for line in text.splitlines() if "panicked" in line]


def iso_ms(stamp):
    """Milliseconds of an RFC 3339 timestamp such as the integrator logs."""
    return int(datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp() * 1000)


def trim_step(step):
    """A step name with its detail cut to the first word (`Blocked(target_failing)`);
    an id-valued detail (`RevertCandidate(<task id>)`) is dropped."""
    if "(" not in step:
        return step
    name, detail = step.split("(", 1)
    word = detail.strip('")').split(":")[0].split(" ")[0].strip('"')
    return name if len(word) >= 32 else f"{name}({word})"


class Agent:
    """One scripted agent: a CLI role with its own HOME, session and clone."""

    def __init__(self, soak, role):
        """The scripted agent for credential `role` of `soak`."""
        self.soak = soak
        self.role = role
        self.clone = soak.path("clones", role)
        self.count = 0

    def env(self):
        """The role's CLI environment (never carries a token)."""
        env = staging.cli_env(self.soak.st, self.role)
        env["AGENT_COORDINATOR_SESSION"] = f"soak-{self.role}"
        return env

    def cli(self, *args, body=None):
        """Runs the CLI with --json; returns the parsed reply or raises."""
        command = [str(self.soak.st.cli_bin), "--json", *args]
        if body is not None:
            command += ["--input", "-"]
        result = subprocess.run(command, input=None if body is None else json.dumps(body),
                                text=True, capture_output=True, env=self.env(), timeout=120)
        reply = json.loads(result.stdout) if result.stdout.strip() else {}
        if result.returncode != 0:
            error = reply.get("error", {})
            raise SoakFailure(f"{self.role} {' '.join(args[:2])}: {error.get('code')} "
                              f"{error.get('message')} {result.stderr.strip()[:300]}")
        return reply

    def setup(self):
        """Connects the session and clones the remote for worktrees/reviews."""
        self.soak.path("state", self.role).mkdir(mode=0o700, parents=True, exist_ok=True)
        self.cli("connect")
        git(self.soak.dir, "clone", "--quiet", str(self.soak.remote), str(self.clone))
        git(self.clone, "config", "user.name", f"soak-{self.role}")
        git(self.clone, "config", "user.email", f"{self.role}@soak.invalid")

    def task(self, task_id):
        """A fresh read of one task."""
        path = f"/api/v1/projects/{self.soak.project}/tasks/{task_id}"
        return self.cli("request", "--method", "get", "--path", path)["data"]

    def post(self, path, body):
        """A POST through the CLI's durable mutation state."""
        return self.cli("request", "--method", "post", "--path", path, body=body)["data"]

    def worktree(self, attempt, base):
        """Prepares and registers a fresh worktree for `attempt` at `base`."""
        git(self.clone, "fetch", "--quiet", "origin")
        self.count += 1
        path = self.soak.path("wt", f"{self.role}-{self.count}")
        self.cli("worktree", "prepare", "--attempt", attempt["id"],
                 "--generation", str(attempt["generation"]), "--source", str(self.clone),
                 "--path", str(path), "--branch", f"task/{self.role}-{self.count}",
                 "--base", base)
        return path

    def submit(self, attempt, task_revision, checkout):
        """Submits the worktree's head as a code candidate; returns its id."""
        evidence = {"summary": "Scripted soak change.", "handoff": "Review the diff.",
                    "acceptance_evidence": [{"criterion": CRITERION,
                                             "evidence": "committed by the soak"}]}
        reply = self.cli("submissions", "code", "--attempt", attempt["id"],
                         "--generation", str(attempt["generation"]),
                         "--task-revision", str(task_revision),
                         "--project-policy-revision", str(self.soak.policy_revision),
                         "--workflow-policy-revision", str(self.soak.workflow_revision),
                         "--checkout", str(checkout), body=evidence)
        return reply["data"]["submission"]["id"]


def commit_files(checkout, files, message):
    """Writes `files` ({path: text}) in `checkout` and commits them."""
    for name, text in files.items():
        target = Path(checkout, name)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
    git(checkout, "add", "--", *files)
    git(checkout, "commit", "--quiet", "-m", message)
    return git(checkout, "rev-parse", "HEAD")


class Soak:
    """One disposable soak instance: server, remote, project, agents,
    integrator, fake checks, and the scenario records."""

    def __init__(self, args):
        """The instance described by the parsed command line `args`."""
        self.args = args
        self.dir = Path(args.dir).resolve()
        self.st = staging.Staging(types.SimpleNamespace(
            dir=self.dir, port=args.port, server=args.server, cli=args.cli))
        self.remote = self.path("remote.git")
        self.owner = self.path("clones", "owner")
        self.fake = FakeChecks(self.path("fake-checks.json"))
        self.integrator = Integrator(self)
        self.agents = {}
        self.tokens = {}
        self.records = []
        self.record = None

    def path(self, *parts):
        """A path inside the soak directory."""
        return self.dir.joinpath(*parts)

    def query(self, sql, *params):
        """Rows (as dicts) from a read-only connection to the soak database."""
        uri = f"file:{self.path('db', 'coordinator.sqlite3')}?mode=ro"
        with sqlite3.connect(uri, uri=True, timeout=10) as connection:
            connection.row_factory = sqlite3.Row
            return [dict(row) for row in connection.execute(sql, params)]

    def tip(self):
        """The remote's current `main`."""
        return git(self.remote, "rev-parse", "refs/heads/main")

    def owner_commit(self, name, text, message):
        """The owner commits `name` directly to the remote's main; returns it."""
        git(self.owner, "pull", "--quiet", "--ff-only", "origin", "main")
        sha = commit_files(self.owner, {name: text}, message)
        git(self.owner, "push", "--quiet", "origin", "main")
        return sha

    def next_view(self, role, as_role):
        """`next` for `as_role`, read with `role`'s credential (bearer only)."""
        url = f"{self.st.origin}/api/v1/projects/{self.project}/next?role={as_role}"
        request = urllib.request.Request(url, headers={
            "Authorization": f"Bearer {self.tokens[role]}"})
        with urllib.request.urlopen(request, timeout=15) as response:
            return json.load(response)["data"]

    def track(self, task_id, label):
        """Adds a task to the current scenario's subjects."""
        self.record["tasks"].append({"id": task_id, "label": label})
        return task_id


def refuse_foreign_dir(directory):
    """Exits unless `directory` is absent or carries the soak marker. Runs
    before anything that `teardown` could undo, so a refused directory (for
    example a staging instance) keeps its database and running server."""
    if directory.exists() and not (directory / MARKER).is_file():
        sys.exit(f"{directory} exists and is not a soak directory; pick another --dir")


def prepare_dir(soak):
    """Empties the soak directory (only one this script created) and makes it."""
    refuse_foreign_dir(soak.dir)
    if soak.dir.exists():
        shutil.rmtree(soak.dir)
    soak.dir.mkdir(mode=0o700, parents=True)
    (soak.dir / MARKER).write_text("agentc soak instance\n")
    for name in ("db", "clones", "wt", "integrator"):
        soak.path(name).mkdir(mode=0o700)


def seed_remote(soak):
    """Creates the bare remote from the seed revision plus a roster commit,
    keeps a reflog of main and installs the push-hold hook."""
    git(soak.dir, "init", "--quiet", "--bare", "--initial-branch=main", str(soak.remote))
    git(soak.remote, "config", "core.logAllRefUpdates", "always")
    git(soak.args.seed, "push", "--quiet", str(soak.remote),
        f"{soak.args.revision}:refs/heads/main")
    git(soak.dir, "clone", "--quiet", str(soak.remote), str(soak.owner))
    git(soak.owner, "config", "user.name", "Soak Owner")
    git(soak.owner, "config", "user.email", "owner@soak.invalid")
    soak.owner_commit(".agent-coordinator/roster.toml", ROSTER, "Map the soak roster")
    install_hold_hook(soak)


def install_hold_hook(soak):
    """A pre-receive hook that parks an update of main while `soak-hold`
    exists in the remote, touching `soak-held` once it waits."""
    hold, held = soak.remote / "soak-hold", soak.remote / "soak-held"
    hook = soak.remote / "hooks" / "pre-receive"
    hook.write_text(
        "#!/bin/sh\n# Soak push hold: an update of main waits while the hold file exists.\n"
        "while read old new ref; do\n"
        f'  if [ "$ref" = refs/heads/main ] && [ -f "{hold}" ]; then\n'
        f'    : > "{held}"\n'
        f'    while [ -f "{hold}" ]; do sleep 0.1; done\n'
        "  fi\ndone\nexit 0\n")
    hook.chmod(0o755)


def create_project(soak, api):
    """Creates the project owned by the integrator and its required checks."""
    policy = dict(staging.POLICY, integration_owner="integrator")
    soak.project = staging.create_project(soak.st, api, soak.remote, name="Soak", policy=policy)
    project = next(p for p in api.call("/api/v1/projects")["items"] if p["id"] == soak.project)
    soak.policy_revision = project["policy_revision"]
    workflow = api.call(f"/api/v1/projects/{soak.project}/workflow-policy",
                        {"expected_revision": 0, "required_checks": REQUIRED_CHECKS},
                        method="PUT")
    soak.workflow_revision = workflow["revision"]


def issue_credentials(soak, api):
    """Issues the soak's credentials and keeps their tokens in memory only."""
    staging.issue_credentials(soak.st, api, ROLES, prefix="soak")
    for role in ROLES:
        with open(soak.path("home", role, "credentials.toml"), "rb") as handle:
            soak.tokens[role] = tomllib.load(handle)["credentials"][0]["token"]
    soak.path("binding.toml").write_text(
        f'service_url = "{soak.st.origin}"\nproject_id = "{soak.project}"\n')


def write_integrator_config(soak):
    """The integrator's TOML config and a fake checks file where every check
    passes and the target carries the required rules."""
    soak.path("integrator.toml").write_text(
        f'credential_file = "{soak.path("home", "integrator", "credentials.toml")}"\n'
        f'origin = "{soak.st.origin}"\nprojects = ["{soak.project}"]\n'
        f'state_dir = "{soak.path("integrator")}"\npoll_seconds = 5\nchecks = "fake"\n'
        f'fake_checks_file = "{soak.fake.path}"\n')
    soak.fake.save({"rules": {"main": RULES}, "default_conclusion": "success"})


def setup(soak):
    """Builds the whole instance and starts the integrator."""
    prepare_dir(soak)
    staging.init_admin(soak.st)
    staging.start_server(soak.st)
    admin = json.loads(soak.path("secrets", "admin.json").read_text())
    soak.admin = staging.Api(soak.st.origin, admin["username"], admin["password"])
    seed_remote(soak)
    create_project(soak, soak.admin)
    issue_credentials(soak, soak.admin)
    write_integrator_config(soak)
    for role in ("impl-a", "impl-b", "rev-1", "rev-2"):
        soak.agents[role] = Agent(soak, role)
        soak.agents[role].setup()
    soak.integrator.start()


def stop_server(soak):
    """Stops the server (SIGTERM, then SIGKILL after 10 s)."""
    pid = soak.st.pid()
    if pid is None:
        return
    os.kill(pid, signal.SIGTERM)
    for _ in range(100):
        if soak.st.pid() is None:
            return
        time.sleep(0.1)
    os.kill(pid, signal.SIGKILL)


def teardown(soak):
    """Stops every child; unless --keep, deletes the instance state (database,
    remote, clones, credentials), keeping the reports, logs and configs.
    Does nothing to a directory without the soak marker."""
    if not (soak.dir / MARKER).is_file():
        return
    soak.integrator.stop()
    stop_server(soak)
    soak.st.path("server.pid").unlink(missing_ok=True)
    if not soak.args.keep:
        for name in ("clones", "wt", "integrator", "remote.git", "db", "home", "state",
                     "secrets"):
            shutil.rmtree(soak.path(name), ignore_errors=True)


# ---------------------------------------------------------------- observers


def current_submission(soak, task_id):
    """The task's current submission id and workflow phase, or (None, None)."""
    rows = soak.query("SELECT current_submission_id, phase FROM workflow_subjects "
                      "WHERE task_id=?", task_id)
    return (rows[0]["current_submission_id"], rows[0]["phase"]) if rows else (None, None)


def lifecycle(soak, task_id):
    """The task's lifecycle (`open`, `done`, ...)."""
    return soak.query("SELECT lifecycle FROM tasks WHERE id=?", task_id)[0]["lifecycle"]


def results_of(soak, task_id):
    """Every integrator result pinned for the task's submissions, oldest first."""
    return soak.query("SELECT r.* FROM integrator_results r JOIN submissions s "
                      "ON s.id=r.submission_id WHERE s.task_id=? ORDER BY r.rowid", task_id)


def landings_of(soak, task_id):
    """The task's landed observations (published or equivalent) with their R."""
    marks = ",".join("?" * len(LANDED))
    return soak.query(
        "SELECT o.*, r.r, r.t0, r.submission_id FROM integrator_observations o "
        "JOIN integrator_results r ON r.id=o.result_id JOIN submissions s "
        f"ON s.id=r.submission_id WHERE s.task_id=? AND o.disposition IN ({marks}) "
        "ORDER BY o.observed_at", task_id, *LANDED)


def revises_of(soak, task_id):
    """The integrator's revises of the task, oldest first."""
    return soak.query("SELECT * FROM integrator_revises WHERE task_id=? "
                      "ORDER BY revised_at", task_id)


def reports_since(soak, since_ms):
    """Integrator reports created since `since_ms`."""
    return soak.query("SELECT * FROM integrator_reports WHERE created_at>=? "
                      "ORDER BY rowid", since_ms)


def approved_at(soak, submission_id):
    """The time of the submission's latest approving review, or None."""
    rows = soak.query("SELECT max(created_at) AS at FROM review_decisions "
                      "WHERE submission_id=? AND decision='approved'", submission_id)
    return rows[0]["at"]


def parked(soak, task_id):
    """True when agent revises parked the task (the service's own counts)."""
    window = now_ms() - 86_400_000
    base = ("SELECT count(*) AS n FROM events e JOIN principals pr ON pr.id=e.actor_id "
            "JOIN submissions s ON s.id=e.record_id WHERE e.kind='submission.reopened' "
            "AND pr.kind='agent' AND s.task_id=? AND e.created_at>?")
    total = soak.query(base, task_id, window)[0]["n"]
    counted = soak.query(base + " AND NOT EXISTS(SELECT 1 FROM integrator_revises ir "
                         "WHERE ir.submission_id=e.record_id AND ir.serialized_after "
                         "IS NOT NULL)", task_id, window)[0]["n"]
    return counted >= REVISE_LIMIT or total >= SERIALIZED_REVISE_CAP


# ---------------------------------------------------------------- scripted agents


def create_task(soak, agent, label):
    """Creates a code task as `agent` and tracks it under `label`."""
    body = {"title": f"Soak {soak.record['name']}: {label}", "kind": "code",
            "description": "A scripted change for the integrator soak.",
            "acceptance_criteria": [CRITERION]}
    task = agent.cli("tasks", "create", body=body)["data"]
    return soak.track(task["id"], label)


def implement(soak, agent, task_id, files, base=None):
    """Claims the task as `agent`, commits `files` on `base` (default: the
    remote's main) in a fresh worktree and submits it; returns the submission."""
    task = agent.task(task_id)
    claim = agent.cli("claim", "--task", task_id, "--revision", str(task["revision"]))
    claim = claim["data"]["claim"]
    if claim is None:
        raise SoakFailure(f"{agent.role} could not claim {task_id}")
    checkout = agent.worktree(claim["attempt"], base or soak.tip())
    commit_files(checkout, files, f"Soak change for task {task_id[:8]}")
    return agent.submit(claim["attempt"], claim["task"]["revision"], checkout)


def queued_review(soak, reviewer, task_id):
    """The subject's queued review activity for its current submission, or
    None (also while the subject has no submission yet, as a revert does
    until the integrator records its candidate)."""
    try:
        listing = reviewer.cli("reviews", "list", "--task", task_id)["data"]
    except SoakFailure:
        return None
    current = (listing.get("submission") or {}).get("id")
    for activity in listing.get("activities", []):
        if (activity["kind"] != "integration" and activity["status"] == "queued"
                and activity["submission_id"] == current):
            return activity
    return None


def approve(soak, reviewer, task_id, deadline):
    """Claims the subject's queued review as `reviewer` and approves it."""
    activity = deadline.wait(lambda: queued_review(soak, reviewer, task_id),
                             f"a queued review of {task_id}")
    claimed = reviewer.cli(
        "reviews", "claim", "--activity", activity["id"],
        "--submission", activity["submission_id"],
        "--project-policy-revision", str(soak.policy_revision),
        "--workflow-policy-revision", str(soak.workflow_revision),
        "--candidate-checkout", str(reviewer.clone))["data"]["attempt"]
    decision = {"decision": "approved", "summary": "Scripted soak approval.", "findings": []}
    reviewer.cli("reviews", "decide", "--activity", activity["id"],
                 "--attempt", claimed["id"], "--generation", str(claimed["generation"]),
                 "--submission", activity["submission_id"], body=decision)


def wait_done(soak, task_id, deadline):
    """Waits until the task is done."""
    deadline.wait(lambda: lifecycle(soak, task_id) == "done", f"task {task_id} done")


def wait_revised(soak, task_id, reason, count, deadline):
    """Waits until the integrator's `count`th revise of the task (with
    `reason`) left it in revision_needed; returns that revise."""
    def revised():
        """That revise once it exists and the subject is back in revision."""
        rows = [r for r in revises_of(soak, task_id) if r["reason_code"] == reason]
        phase = current_submission(soak, task_id)[1]
        return rows[count - 1] if len(rows) >= count and phase == "revision_needed" else None
    return deadline.wait(revised, f"revise #{count} ({reason}) of {task_id}")


def wait_result(soak, task_id, deadline, t0=None):
    """Waits for a result of the task's current submission (at `t0`, when
    given) and returns it."""
    def pinned():
        """The newest matching result of the current submission, or None."""
        submission = current_submission(soak, task_id)[0]
        rows = [r for r in results_of(soak, task_id) if r["submission_id"] == submission]
        rows = [r for r in rows if t0 is None or r["t0"] == t0]
        return rows[-1] if rows else None
    return deadline.wait(pinned, f"a pinned result of {task_id}")


def land(soak, impl, rev, label, files, deadline):
    """One subject from task creation to done; returns the task id."""
    task_id = create_task(soak, soak.agents[impl], label)
    implement(soak, soak.agents[impl], task_id, files)
    approve(soak, soak.agents[rev], task_id, deadline)
    wait_done(soak, task_id, deadline)
    return task_id


def expect(condition, message):
    """Raises SoakFailure with `message` unless `condition` holds."""
    if not condition:
        raise SoakFailure(message)


def note(soak, text):
    """Adds a line to the current scenario's notes."""
    soak.record["notes"].append(text)


# ---------------------------------------------------------------- scenarios


def scenario_clean(soak, deadline):
    """Four disjoint subjects submitted together; each lands and the target
    equals its R at the moment it is observed."""
    subjects = []
    for index in range(4):
        impl = ("impl-a", "impl-b")[index % 2]
        task_id = create_task(soak, soak.agents[impl], f"clean {index}")
        implement(soak, soak.agents[impl], task_id, {f"soak/clean/{index}.txt": f"{index}\n"})
        subjects.append((task_id, ("rev-1", "rev-2")[index % 2]))
    for task_id, rev in subjects:
        approve(soak, soak.agents[rev], task_id, deadline)
    for task_id, _ in subjects:
        wait_done(soak, task_id, deadline)
        landing = landings_of(soak, task_id)[0]
        expect(landing["tip"] == landing["r"], f"{task_id}: observed tip is not its R")
    note(soak, "4 disjoint subjects approved together; each published with tip == R")


def resubmit(soak, impl, task_id, files, deadline):
    """The implementer takes a revised subject back and resubmits `files`
    on the current tip."""
    deadline.wait(lambda: current_submission(soak, task_id)[1] == "revision_needed",
                  f"{task_id} back with its implementer")
    implement(soak, soak.agents[impl], task_id, files)


def conflict_pair(soak, deadline):
    """A first and a second subject create one file differently: the second
    is revised `conflict`, rebased by its implementer, re-reviewed and lands."""
    path = f"soak/conflict/pair-{now_ms()}.txt"
    base = soak.tip()
    first = create_task(soak, soak.agents["impl-a"], "pair first")
    second = create_task(soak, soak.agents["impl-b"], "pair second")
    implement(soak, soak.agents["impl-a"], first, {path: "first\n"}, base)
    implement(soak, soak.agents["impl-b"], second, {path: "second\n"}, base)
    approve(soak, soak.agents["rev-1"], first, deadline)
    wait_done(soak, first, deadline)
    approve(soak, soak.agents["rev-2"], second, deadline)
    revise = wait_revised(soak, second, "conflict", 1, deadline)
    expect(revise["landing_task_id"] == first, "the conflict revise did not cite the landing")
    resubmit(soak, "impl-b", second, {path: "first\nsecond\n"}, deadline)
    approve(soak, soak.agents["rev-2"], second, deadline)
    wait_done(soak, second, deadline)


def edge_round(soak, subject, lander, path, deadline):
    """One round of the three-way case: `lander` lands on `path` first, then
    the subject's approved candidate conflicts and is revised."""
    count = len(revises_of(soak, subject)) + 1
    approve(soak, soak.agents["rev-1"], lander, deadline)
    wait_done(soak, lander, deadline)
    approve(soak, soak.agents["rev-2"], subject, deadline)
    revise = wait_revised(soak, subject, "conflict", count, deadline)
    expect(revise["landing_task_id"] == lander, f"revise #{count} did not cite its landing")
    return revise


def conflict_edge(soak, deadline):
    """Landings beat one subject on the same line until its revises reach
    REVISE_LIMIT: the revises below it count, the one at it is serialized
    after its landing instead of parking; the subject then lands."""
    path = f"soak/conflict/edge-{now_ms()}.txt"
    subject = create_task(soak, soak.agents["impl-b"], "edge subject")
    base = soak.tip()
    implement(soak, soak.agents["impl-b"], subject, {path: "subject 0\n"}, base)
    revises = []
    for round_ in range(1, 4):
        lander = create_task(soak, soak.agents["impl-a"], f"edge lander {round_}")
        implement(soak, soak.agents["impl-a"], lander, {path: f"lander {round_}\n"}, base)
        revises.append(edge_round(soak, subject, lander, path, deadline))
        base = soak.tip()
        resubmit(soak, "impl-b", subject, {path: f"subject {round_}\n"}, deadline)
    check_serialized(soak, subject, revises)
    approve(soak, soak.agents["rev-2"], subject, deadline)
    wait_done(soak, subject, deadline)


def check_serialized(soak, subject, revises):
    """Checks that only the revise at the limit is serialized after its
    landing and that none parked the subject."""
    expect(revises[0]["serialized_after"] is None and revises[1]["serialized_after"] is None,
           "a revise below the limit was serialized")
    expect(revises[2]["serialized_after"] == revises[2]["landing_task_id"],
           "the revise at the limit was not serialized after its landing")
    expect(all(r["park_reason"] is None for r in revises), "a revise parked the subject")
    expect(not parked(soak, subject), "the subject is parked")
    note(soak, "3-way: revises 1-2 recorded their landing; revise 3 serialized_after "
               "the third landing, park_reason null; the subject stayed claimable")


def scenario_conflicts(soak, deadline):
    """A two-way conflict and the three-way serialize-before-park case."""
    conflict_pair(soak, deadline)
    conflict_edge(soak, deadline)


def owner_moves_target(soak, deadline):
    """The owner commits to main while an approved subject waits on checks:
    the integrator pins a new result on the new tip and lands that."""
    soak.fake.default(None)
    task_id = create_task(soak, soak.agents["impl-a"], "waits while main moves")
    implement(soak, soak.agents["impl-a"], task_id, {f"soak/moves/{now_ms()}.txt": "subject\n"})
    approve(soak, soak.agents["rev-1"], task_id, deadline)
    first = wait_result(soak, task_id, deadline)
    moved = soak.owner_commit(f"soak/moves/owner-{now_ms()}.txt", "owner\n",
                              "Owner edit straight to main")
    wait_result(soak, task_id, deadline, t0=moved)
    soak.fake.default("success")
    wait_done(soak, task_id, deadline)
    landing = landings_of(soak, task_id)[0]
    expect(landing["t0"] == moved and first["t0"] != moved,
           "the landing did not roll forward onto the owner's commit")
    note(soak, f"roll-forward: {len(results_of(soak, task_id))} results pinned, "
               "landed on the owner's commit")


def agent_trailer_landing(soak, deadline):
    """An out-of-band commit with an agent trailer yields one non-blocking
    `unreviewed_landing` report; the queue keeps moving."""
    since = now_ms()
    sha = soak.owner_commit(f"soak/moves/agent-{now_ms()}.txt", "agent\n",
                            f"Out-of-band agent change\n\n{AGENT_TRAILER}")
    def reported():
        """The unreviewed_landing reports once one names the commit."""
        rows = [r for r in reports_since(soak, since) if r["kind"] == "unreviewed_landing"]
        return rows if any(sha in r["details_json"] for r in rows) else None
    rows = deadline.wait(reported, "the unreviewed_landing report")
    expect(len(rows) == 1 and rows[0]["requires_human"] == 0,
           "expected one non-blocking unreviewed_landing report")
    land(soak, "impl-b", "rev-2", "after the agent landing",
         {"soak/moves/after.txt": "after\n"}, deadline)


def scenario_target_moves(soak, deadline):
    """A human commit while a subject waits, then an agent-trailer commit."""
    owner_moves_target(soak, deadline)
    agent_trailer_landing(soak, deadline)


def pending_subject(soak, impl, rev, label, files, deadline):
    """A subject approved while checks are pending; returns (task, result)."""
    soak.fake.default(None)
    task_id = create_task(soak, soak.agents[impl], label)
    implement(soak, soak.agents[impl], task_id, files)
    approve(soak, soak.agents[rev], task_id, deadline)
    return task_id, wait_result(soak, task_id, deadline)


def flaky_check(soak, deadline):
    """A failure then a pass on rerun publishes with a `flaky` report."""
    task_id, result = pending_subject(soak, "impl-a", "rev-1", "flaky check",
                                      {f"soak/checks/flaky-{now_ms()}.txt": "x\n"}, deadline)
    soak.fake.script(result["r"], ["failure", "success"])
    wait_done(soak, task_id, deadline)
    soak.fake.default("success")
    flaky = [r for r in reports_since(soak, result["created_at"]) if r["kind"] == "flaky"]
    expect(len(flaky) == 1 and flaky[0]["requires_human"] == 0
           and flaky[0]["result_id"] == result["id"], "expected one non-blocking flaky report")


def reproduced_failure(soak, deadline):
    """A failure reproduced by R's rerun while the check passes on X:
    `check_failed` revise; the implementer fixes it and the fix lands."""
    task_id, result = pending_subject(soak, "impl-b", "rev-2", "reproduced failure",
                                      {"soak/checks/broken.txt": "broken\n"}, deadline)
    soak.fake.script(result["t0"], ["success"])
    soak.fake.script(result["r"], ["failure", "failure"])
    wait_revised(soak, task_id, "check_failed", 1, deadline)
    soak.fake.default("success")
    resubmit(soak, "impl-b", task_id, {"soak/checks/broken.txt": "fixed\n"}, deadline)
    approve(soak, soak.agents["rev-2"], task_id, deadline)
    wait_done(soak, task_id, deadline)


def scenario_check_failures(soak, deadline):
    """A flaky check and a reproduced failure."""
    flaky_check(soak, deadline)
    reproduced_failure(soak, deadline)


def crash_after_result(soak, deadline):
    """SIGKILL once R is recorded (checks pending), restart, then land."""
    task_id, _ = pending_subject(soak, "impl-a", "rev-1", "crash after result",
                                 {"soak/crash/result.txt": "x\n"}, deadline)
    soak.integrator.restart()
    soak.fake.default("success")
    wait_done(soak, task_id, deadline)


def crash_during_rerun(soak, deadline):
    """SIGKILL while a rerun of a failed check is in flight, restart; the
    rerun passes and the subject lands (flaky)."""
    task_id, result = pending_subject(soak, "impl-b", "rev-2", "crash during rerun",
                                      {"soak/crash/rerun.txt": "x\n"}, deadline)
    before = soak.fake.reruns()
    soak.fake.script(result["r"], ["failure", None])
    deadline.wait(lambda: soak.fake.reruns() > before, "the integrator's rerun request")
    soak.integrator.restart()
    soak.fake.script(result["r"], ["failure", "success"])
    soak.fake.default("success")
    wait_done(soak, task_id, deadline)


def held_push(soak, label, deadline):
    """A subject whose push of main is parked in the remote's hook after
    push authority was issued; returns (task, result)."""
    soak.fake.default("success")
    held = soak.remote / "soak-held"
    held.unlink(missing_ok=True)
    (soak.remote / "soak-hold").write_text("")
    task_id = create_task(soak, soak.agents["impl-a"], label)
    implement(soak, soak.agents["impl-a"], task_id, {f"soak/crash/{label[:4]}.txt": "x\n"})
    approve(soak, soak.agents["rev-1"], task_id, deadline)
    result = wait_result(soak, task_id, deadline)
    deadline.wait(held.is_file, "the integrator's push of main to reach the hook")
    issued = soak.query("SELECT authority_issued_at FROM integrator_results WHERE id=?",
                        result["id"])[0]["authority_issued_at"]
    expect(issued is not None, "the push began without push authority")
    return task_id, result


def crash_during_push(soak, deadline):
    """SIGKILL the integrator (not its git push) after push authority while
    the push is in flight; the push completes; the restart observes it."""
    task_id, result = held_push(soak, "kill integrator mid-push", deadline)
    soak.integrator.kill()
    (soak.remote / "soak-hold").unlink()
    deadline.wait(lambda: soak.tip() == result["r"], "the orphaned push to land")
    soak.integrator.restart()
    wait_done(soak, task_id, deadline)


def crash_aborting_push(soak, deadline):
    """SIGKILL the integrator's whole process group after push authority,
    aborting the push; the restart re-observes the tip and lands."""
    task_id, result = held_push(soak, "abort push", deadline)
    soak.integrator.kill(group=True)
    (soak.remote / "soak-hold").unlink()
    expect(soak.tip() == result["t0"], "the aborted push moved main")
    soak.integrator.restart()
    wait_done(soak, task_id, deadline)


def crash_server(soak, deadline):
    """SIGKILL the server while an approved subject's checks are pending and
    restart it; the integrator rides out the outage and lands the subject."""
    task_id, _ = pending_subject(soak, "impl-b", "rev-2", "server crash",
                                 {"soak/crash/server.txt": "x\n"}, deadline)
    os.kill(soak.st.pid(), signal.SIGKILL)
    deadline.wait(lambda: soak.st.pid() is None and not staging.healthy(soak.st.origin),
                  "the server to go down")
    time.sleep(soak.args.outage)
    staging.start_server(soak.st)
    soak.fake.default("success")
    wait_done(soak, task_id, deadline)


def scenario_crash_restart(soak, deadline):
    """SIGKILL/restart of the integrator after R is recorded, during a
    rerun, and twice after push authority (push completing, push aborted);
    then SIGKILL/restart of the server."""
    crash_after_result(soak, deadline)
    crash_during_rerun(soak, deadline)
    crash_during_push(soak, deadline)
    crash_aborting_push(soak, deadline)
    crash_server(soak, deadline)
    note(soak, "integrator SIGKILLs: after result, during rerun, mid-push (git survives, "
               "push lands while down), mid-push (process group killed, push aborted); "
               "server SIGKILL with checks pending")


def admin_call(soak, path, body=None, method=None):
    """An admin browser-session call; a refusal raises SoakFailure (staging.Api
    exits on one)."""
    try:
        return soak.admin.call(path, body, method=method)
    except SystemExit as refusal:
        raise SoakFailure(f"admin {path}: {refusal}") from None


def published_result(soak, task_id):
    """The id of the task's published integrator result."""
    return landings_of(soak, task_id)[0]["result_id"]


def agent_revert(soak, deadline):
    """impl-b reverts impl-a's landing with evidence; rev-2 (neither the
    creator nor a contributor) reviews the mechanical candidate; it lands."""
    original = land(soak, "impl-a", "rev-1", "reverted by an agent",
                    {"soak/revert/agent.txt": "defect\n"}, deadline)
    body = {"result_id": published_result(soak, original), "reason": "defect",
            "evidence": {"check": "tests", "first_parent": "pass", "tip": "fail",
                         "note": "scripted soak evidence"}}
    revert = soak.agents["impl-b"].post(f"/api/v1/projects/{soak.project}/reverts", body)
    soak.track(revert["id"], "agent revert")
    approve(soak, soak.agents["rev-2"], revert["id"], deadline)
    wait_done(soak, revert["id"], deadline)
    expect(not git(soak.remote, "ls-tree", "main", "soak/revert/agent.txt"),
           "the reverted file is still on main")


def human_revert(soak, deadline):
    """The admin reverts a landing from the browser session: no review."""
    original = land(soak, "impl-b", "rev-2", "reverted by a human",
                    {"soak/revert/human.txt": "unwanted\n"}, deadline)
    body = {"result_id": published_result(soak, original), "reason": "human"}
    revert = admin_call(soak, f"/api/v1/projects/{soak.project}/reverts", body)
    soak.track(revert["id"], "human revert")
    wait_done(soak, revert["id"], deadline)
    reviews = soak.query("SELECT count(*) AS n FROM review_decisions d JOIN submissions s "
                         "ON s.id=d.submission_id WHERE s.task_id=?", revert["id"])[0]["n"]
    expect(reviews == 0, "the human revert was reviewed")


def scenario_reverts(soak, deadline):
    """An agent revert with evidence and review, and a human revert."""
    agent_revert(soak, deadline)
    human_revert(soak, deadline)


SCENARIOS = {"clean": scenario_clean, "conflicts": scenario_conflicts,
             "target_moves": scenario_target_moves, "check_failures": scenario_check_failures,
             "crash_restart": scenario_crash_restart, "reverts": scenario_reverts}


def mix_clean(soak, deadline):
    """One disjoint subject."""
    impl, rev = random.choice([("impl-a", "rev-1"), ("impl-b", "rev-2")])
    land(soak, impl, rev, "mix clean", {f"soak/mix/{now_ms()}.txt": "mix\n"}, deadline)


MIX = [mix_clean, conflict_pair, owner_moves_target, flaky_check]


def scenario_mix(soak, minutes):
    """The random-mix loop: cheap scenarios picked at random for `minutes`,
    each with its own deadline."""
    end = time.monotonic() + minutes * 60
    picks = collections.Counter()
    while time.monotonic() < end:
        step = random.choice(MIX)
        picks[step.__name__] += 1
        step(soak, Deadline(soak.args.timeout * 60))
    note(soak, "mix: " + ", ".join(f"{name} x{n}" for name, n in sorted(picks.items())))


# ---------------------------------------------------------------- invariants


def fast_forward_violations(soak):
    """Updates of the remote's main (from its reflog) that are not fast-forwards."""
    log = soak.remote / "logs" / "refs" / "heads" / "main"
    violations = []
    for line in log.read_text().splitlines():
        old, new = line.split(" ")[:2]
        if set(old) != {"0"} and not is_ancestor(soak.remote, old, new):
            violations.append(f"main moved {old[:12]} -> {new[:12]} without a fast-forward")
    return violations


def containment_violations(soak):
    """Published observations whose R is not in the observed tip, or whose
    tip is not in the final target."""
    final, violations = soak.tip(), []
    for row in soak.query("SELECT o.tip, r.r, r.id FROM integrator_observations o JOIN "
                          "integrator_results r ON r.id=o.result_id "
                          "WHERE o.disposition IN ('published','already_contained','published_after_reopen')"):
        if not (is_ancestor(soak.remote, row["r"], row["tip"])
                and is_ancestor(soak.remote, row["tip"], final)):
            violations.append(f"published result {row['id']} is not contained in main")
    return violations


def landing_violations(soak):
    """Subjects of this scenario that did not land exactly once."""
    violations = []
    for task in soak.record["tasks"]:
        landed = landings_of(soak, task["id"])
        if len(landed) != 1:
            violations.append(f"{task['label']}: {len(landed)} landings")
    return violations


def outstanding_authority(soak):
    """Results still holding push authority with no later observation."""
    rows = soak.query("SELECT r.id FROM integrator_results r WHERE r.project_id=? AND "
                      "r.authority_expires_at IS NOT NULL AND NOT EXISTS(SELECT 1 FROM "
                      "integrator_observations o WHERE o.result_id=r.id AND "
                      "o.observed_at>=r.authority_issued_at)", soak.project)
    return [f"result {row['id']} still holds push authority" for row in rows]


def human_interventions(soak):
    """Everything in this scenario that would need a human (HRI), by kind: open
    reports that require one, `next`'s human queue and items for both roles,
    parked subjects, and subjects not done that are not parked (stuck).
    `next` counts only the candidates it inspected before the first eligible
    one, so parked subjects are also counted here from the database."""
    record = soak.record
    reports = [r for r in reports_since(soak, record["started_ms"]) if r["requires_human"]]
    views = {as_role: soak.next_view(reader, as_role)
             for as_role, reader in (("implementer", "impl-a"), ("reviewer", "rev-1"))}
    return {
        "requires_human_reports": [f"{r['kind']} {r['id']}" for r in reports],
        "human_queue": [f"{role}: {v['human_queue']}" for role, v in views.items()
                        if v["human_queue"]],
        "human_queue_items": [f"{role}: {len(v['human_queue_items'])}"
                              for role, v in views.items() if v["human_queue_items"]],
        "parked": [t["label"] for t in record["tasks"] if parked(soak, t["id"])],
        "stuck": [t["label"] for t in record["tasks"]
                  if lifecycle(soak, t["id"]) != "done" and not parked(soak, t["id"])],
    }


def hri_count(hri):
    """The number of human-required interventions in an HRI breakdown."""
    total = len(hri["requires_human_reports"]) + len(hri["parked"]) + len(hri["stuck"])
    for entry in hri["human_queue"] + hri["human_queue_items"]:
        total += int(entry.rsplit(" ", 1)[1])
    return total


def check_invariants(soak):
    """Violations of the soak invariants for the current scenario."""
    violations = (fast_forward_violations(soak) + containment_violations(soak)
                  + landing_violations(soak) + outstanding_authority(soak))
    violations += [f"integrator panic: {line[:200]}" for line in soak.integrator.panics()]
    return violations


# ---------------------------------------------------------------- measurements


def latencies(soak):
    """Approval-to-published milliseconds for this scenario's reviewed subjects."""
    values = []
    for task in soak.record["tasks"]:
        for landing in landings_of(soak, task["id"])[:1]:
            approved = approved_at(soak, landing["submission_id"])
            if approved is not None:
                values.append(landing["observed_at"] - approved)
    return values


def measure(soak, record, restarts_before):
    """Fills the record's counts from the database and the integrator log."""
    since, until = record["started_ms"], record["ended_ms"]
    reports = [r for r in reports_since(soak, since) if r["created_at"] <= until]
    record["reports"] = dict(collections.Counter(r["kind"] for r in reports))
    revises = [r for t in record["tasks"] for r in revises_of(soak, t["id"])]
    record["revises"] = dict(collections.Counter(r["reason_code"] for r in revises))
    record["landings"] = sum(len(landings_of(soak, t["id"])) for t in record["tasks"])
    record["latencies_ms"] = latencies(soak)
    record["steps"] = dict(soak.integrator.steps(since, until))
    record["restarts"] = soak.integrator.restarts - restarts_before


def percentile(values, share):
    """Nearest-rank percentile of `values` (None when empty)."""
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, -(-len(ordered) * share // 100) - 1)]


def latency_summary(values):
    """p50/p90/max of latencies, in seconds."""
    seconds = lambda v: None if v is None else round(v / 1000, 1)  # noqa: E731
    return {"count": len(values), "p50_s": seconds(percentile(values, 50)),
            "p90_s": seconds(percentile(values, 90)),
            "max_s": seconds(max(values) if values else None)}


# ---------------------------------------------------------------- running


def reset_fixtures(soak):
    """Leaves checks passing, no push hold, and the integrator running."""
    soak.fake.default("success")
    (soak.remote / "soak-hold").unlink(missing_ok=True)
    if soak.integrator.process.poll() is not None:
        note(soak, f"integrator had exited ({soak.integrator.process.returncode}); restarted")
        soak.integrator.restart()


def run_scenario(soak, name, body):
    """Runs one scenario body, then measures it and checks the invariants."""
    record = {"name": name, "started_ms": now_ms(), "tasks": [], "notes": [], "error": None}
    soak.records.append(record)
    soak.record, restarts = record, soak.integrator.restarts
    try:
        body(soak, Deadline(soak.args.timeout * 60))
    except (Exception, SystemExit) as error:  # noqa: BLE001  (every failure is reported)
        record["error"] = f"{type(error).__name__}: {error}"
        record["trace"] = traceback.format_exc(limit=6)
    finally:
        reset_fixtures(soak)
    conclude(soak, record, restarts)


def conclude(soak, record, restarts):
    """Measures the finished scenario and records its verdict."""
    record["ended_ms"] = now_ms()
    measure(soak, record, restarts)
    record["hri"] = human_interventions(soak)
    record["hri_count"] = hri_count(record["hri"])
    record["violations"] = check_invariants(soak)
    record["passed"] = (record["error"] is None and not record["violations"]
                        and record["hri_count"] == 0)
    verdict = "PASS" if record["passed"] else "FAIL"
    print(f"{verdict} {record['name']} ({(record['ended_ms'] - record['started_ms']) // 1000} s)"
          + (f": {record['error']}" if record["error"] else ""), flush=True)


def overall(soak):
    """Totals over every scenario record."""
    records = soak.records
    values = [v for r in records for v in r["latencies_ms"]]
    summed = lambda key: dict(sum((collections.Counter(r[key]) for r in records),  # noqa: E731
                                  collections.Counter()))
    summary = latency_summary(values)
    return {"passed": bool(records) and all(r["passed"] for r in records),
            "scenarios": len(records), "subjects": sum(len(r["tasks"]) for r in records),
            "landings": sum(r["landings"] for r in records),
            "hri": sum(r["hri_count"] for r in records),
            "restarts": soak.integrator.restarts, "latency": summary,
            "latency_goal_met": summary["p50_s"] is not None
            and summary["p50_s"] * 1000 < LATENCY_GOAL_MS,
            "reports": summed("reports"), "revises": summed("revises"), "steps": summed("steps")}


# ---------------------------------------------------------------- report


def markdown(report):
    """The soak report as Markdown."""
    total = report["overall"]
    latency = total["latency"]
    lines = [f"# Integrator soak {report['started']}", "",
             f"Seed {report['seed_revision'][:12]}, instance {report['dir']}.", "",
             f"**{'PASS' if total['passed'] else 'FAIL'}**: {total['scenarios']} scenarios, "
             f"{total['subjects']} subjects, {total['landings']} landings, HRI {total['hri']}, "
             f"latency p50 {latency['p50_s']} s / p90 {latency['p90_s']} s / "
             f"max {latency['max_s']} s (goal p50 < 600 s: "
             f"{'met' if total['latency_goal_met'] else 'not met'}), "
             f"integrator restarts {total['restarts']}.", "",
             *([f"Aborted before a verdict (counted as FAIL): {', '.join(total['aborted'])}.", ""]
               if total.get("aborted") else []),
             "| scenario | result | subjects | landings | revises | reports | HRI "
             "| p50 s | p90 s | max s | restarts |",
             "|---|---|---|---|---|---|---|---|---|---|---|"]
    lines += [scenario_row(r) for r in report["scenarios"]]
    lines += ["", f"Reports by kind: {counts(total['reports'])}.",
              f"Revises by reason: {counts(total['revises'])}.",
              f"Integrator steps: {counts(total['steps'])}.", ""]
    for record in report["scenarios"]:
        lines += scenario_details(record)
    return "\n".join(lines) + "\n"


def counts(mapping):
    """`a 2, b 1` for a count mapping; `none` when empty."""
    return ", ".join(f"{k} {v}" for k, v in sorted(mapping.items())) or "none"


def scenario_row(record):
    """One scenario's table row."""
    latency = latency_summary(record["latencies_ms"])
    return (f"| {record['name']} | {'pass' if record['passed'] else 'FAIL'} "
            f"| {len(record['tasks'])} | {record['landings']} | {counts(record['revises'])} "
            f"| {counts(record['reports'])} | {record['hri_count']} | {latency['p50_s']} "
            f"| {latency['p90_s']} | {latency['max_s']} | {record['restarts']} |")


def scenario_details(record):
    """One scenario's notes, steps, violations and error."""
    latency = latency_summary(record["latencies_ms"])
    lines = [f"## {record['name']}", "", f"Integrator steps: {counts(record['steps'])}.",
             f"Latency samples (reviewed subjects): {latency['count']}.", ""]
    lines += [f"- {text}" for text in record["notes"]]
    lines += [f"- violation: {text}" for text in record["violations"]]
    lines += [f"- HRI {kind}: {', '.join(items)}"
              for kind, items in record["hri"].items() if items]
    if record["error"]:
        lines += [f"- error: {record['error']}", "", "```", record["trace"].rstrip(), "```"]
    return lines + [""]


def write_report(soak, started):
    """Writes soak-report.json and soak-report.md; returns the Markdown path."""
    aborted = [r["name"] for r in soak.records if "passed" not in r]
    soak.records = [r for r in soak.records if "passed" in r]
    total = overall(soak)
    total.update(aborted=aborted, passed=total["passed"] and not aborted)
    report = {"started": started, "dir": str(soak.dir), "seed_revision": soak.seed_revision,
              "scenarios": soak.records, "overall": total}
    soak.path("soak-report.json").write_text(json.dumps(report, indent=2) + "\n")
    soak.path("soak-report.md").write_text(markdown(report))
    return soak.path("soak-report.md"), report["overall"]


# ---------------------------------------------------------------- command line


def default_dir():
    """The soak directory: under the XDG state home."""
    base = os.environ.get("XDG_STATE_HOME") or str(Path.home() / ".local/state")
    return Path(base) / "agentc-soak"


def parser():
    """Command-line interface; every setting has a local default."""
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--dir", default=default_dir(), help="soak directory (emptied per run)")
    p.add_argument("--port", type=int, default=18091)
    p.add_argument("--server", default=staging.default_binary("agent-coordinator-server"))
    p.add_argument("--cli", default=staging.default_binary("agent-coordinator"))
    p.add_argument("--integrator", default=staging.default_binary("agentc-integrator"))
    p.add_argument("--seed", default=ROOT, help="repository whose revision seeds the remote")
    p.add_argument("--revision", default="HEAD", help="seed revision")
    sub = p.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run", help="run the scenarios and write the report")
    run.add_argument("--scenario", choices=sorted(SCENARIOS), help="run only this one")
    run.add_argument("--minutes", type=float, default=0, help="random mix after the scenarios")
    run.add_argument("--timeout", type=float, default=10, help="minutes per scenario")
    run.add_argument("--outage", type=float, default=12,
                     help="seconds the server stays down in crash_restart")
    run.add_argument("--random-seed", type=int, default=None)
    run.add_argument("--keep", action="store_true", help="keep the instance state")
    sub.add_parser("list", help="print the scenario names")
    return p


def run(args):
    """Sets up the instance, runs the scenarios and writes the report."""
    for binary in (args.server, args.cli, args.integrator):
        if not Path(binary).is_file():
            sys.exit(f"{binary} is missing; build first (see --help)")
    random.seed(args.random_seed)
    soak, started = Soak(args), time.strftime("%Y-%m-%dT%H:%M:%S%z")
    soak.seed_revision = git(args.seed, "rev-parse", args.revision)
    refuse_foreign_dir(soak.dir)
    interrupted = False
    try:
        setup(soak)
        run_all(soak, args)
    except KeyboardInterrupt:
        interrupted = True
        print("interrupted; reporting the scenarios that finished", flush=True)
    finally:
        report = write_report(soak, started) if soak.records else None
        teardown(soak)
    if report is not None:
        print(f"report: {report[0]}")
    if interrupted:
        return 130
    return 0 if report is not None and report[1]["passed"] else 1


def run_all(soak, args):
    """Runs the chosen scenarios, then the random mix when asked."""
    names = [args.scenario] if args.scenario else list(SCENARIOS)
    for name in names:
        run_scenario(soak, name, SCENARIOS[name])
    if args.minutes > 0:
        run_scenario(soak, "mix", lambda s, _deadline: scenario_mix(s, args.minutes))


def terminate(_signum, _frame):
    """Turns SIGTERM/SIGHUP into KeyboardInterrupt so cleanup runs."""
    raise KeyboardInterrupt


def main():
    """Dispatches the subcommand."""
    args = parser().parse_args()
    if args.command == "list":
        for name, body in SCENARIOS.items():
            print(f"{name:15} {body.__doc__.splitlines()[0]}")
        return 0
    signal.signal(signal.SIGTERM, terminate)
    signal.signal(signal.SIGHUP, terminate)
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
