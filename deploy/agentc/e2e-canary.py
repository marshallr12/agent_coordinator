#!/usr/bin/env python3
"""Daily end-to-end canary for one agentc host and harness (autonomy plan §2.0.3).

    e2e-canary.py --harness claude|codex [--url URL] [--project ID]
                  [--token-file FILE] [--ntfy-topic TOPIC] [--ntfy-url URL]
                  [--priority 0] [--timeout-minutes 90] [--poll-seconds 20]
                  [--results FILE] [--ledger FILE] [--reported FILE]
                  [--no-report] [--backfill]

Creates one trivial, harmless task in the host's canary project (see
canary-setup.py) and waits for it to reach `done`: the host's supervisor has to
claim it, launch the harness, get a review and have the integrator land it.
Each run appends one JSON line to --results with the host, harness, outcome
(`ok`, `failed` or `timeout`), the duration in seconds and a detail, and a
failed or timed-out run pages through ntfy. Exit status: 0 done in time, 1
failed or timed out, 2 the page could not be delivered.

Each run's result is also reported to the coordinator (POST
`/projects/<project>/telemetry/canary-results`, with the same token) so the
console shows canary history per project. The results file stays the durable
source: --reported (default `<results>.reported`) lists the rows the
coordinator accepted, every run resends the rows not listed there, and a
row's identity is the SHA-256 of its line, so a lost response or a re-import
never duplicates a result. A report that fails is logged and never changes
the exit status. --backfill only reports the unsent rows of an existing
results file (once, without creating a task); --no-report skips reporting.

The host's supervisor chooses the vendor (`[run] harness`, then
`[health.fallback]`). With --ledger (the supervisor's `costs.jsonl`) the
canary also reads which harness served the task and fails the run when it was
not --harness, so a fallback never passes for the primary.

The task is created at --priority (default 0, urgent: the highest the service
accepts), so in a project shared with other work the supervisor's `next` hands
it out ahead of every P1-P3 task and the --timeout-minutes clock measures the
pipeline, not the queue.

The token file holds one bearer token for an agent allowed to create tasks in
the canary project; it is never printed. The task is created in the `canary`
admission class, which keeps it out of the coordinator's weekly agent-task
budget; the coordinator accepts that class only from an agent principal named
in its `--canary-principals` (`COORDINATOR_CANARY_PRINCIPALS`). ntfy credentials come from NTFY_TOKEN
when set. Every option defaults from an E2E_* environment variable (see
parse() and host-setup.sh's e2e-canary.env); an empty variable counts as unset
and a command-line option wins. Standard library only.
"""
import argparse
import calendar
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import socket
import sys
import time
import urllib.error
import urllib.request
import uuid

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("attention", HERE / "attention.py")
attention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(attention)

DEFAULT_URL = attention.DEFAULT_URL
DEFAULT_TOKEN_FILE = "/etc/agentc/e2e-canary-token"
DEFAULT_RESULTS = "/var/lib/agentc/e2e-canary.jsonl"
DEFAULT_LEDGER = "/var/lib/agentc/costs.jsonl"
DEFAULT_TIMEOUT_MINUTES = 90.0
DEFAULT_POLL_SECONDS = 20.0
# The coordinator takes priorities 0 (urgent) through 3 (low); `next` orders
# ready tasks by priority, so the canary defaults to the first in line.
PRIORITIES = (0, 1, 2, 3)
DEFAULT_PRIORITY = PRIORITIES[0]
CREATE_ATTEMPTS = 3
# The coordinator takes at most 200 results per report.
REPORT_BATCH = 100
OUTCOMES = ("ok", "failed", "timeout")
HARNESSES = ("claude", "codex")
# Lifecycles from which a task never reaches `done` on its own.
DEAD_LIFECYCLES = ("canceled", "cancelled")


def task_body(harness, host, run_id, priority=DEFAULT_PRIORITY):
    """The canary task: append one line to CANARY.md and change nothing else."""
    line = f"canary {run_id} {harness} {host}"
    return {
        "title": f"Canary {harness} on {host} ({run_id})",
        "description": ("A scheduled end-to-end canary; it changes nothing but one line of a log file.\n\n"
                        f"Append exactly this line to the end of CANARY.md, keeping every earlier line:\n\n{line}\n"),
        "acceptance_criteria": [f"CANARY.md ends with the line `{line}` and no other file changed"],
        "kind": "code",
        "priority": priority,
        # The coordinator exempts the task from its weekly agent-task budget
        # only for a principal it designates as a canary (--canary-principals).
        "admission_class": "canary",
    }


def project_url(args, route):
    return f"{args.url.rstrip('/')}/api/v1/projects/{args.project}/{route}"


def create_task(args, token, body, sleep=time.sleep):
    """The new task's id. One Idempotency-Key covers every retry, so a lost
    response never creates a second task."""
    key = str(uuid.uuid4())
    request_body = json.dumps(body).encode()
    last = None
    for attempt in range(CREATE_ATTEMPTS):
        if attempt:
            sleep(min(2.0 * attempt, args.poll_seconds))
        request = urllib.request.Request(project_url(args, "tasks"), data=request_body, method="POST")
        request.add_header("Authorization", f"Bearer {token}")
        request.add_header("Content-Type", "application/json")
        request.add_header("Idempotency-Key", key)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.loads(response.read())["data"]["id"]
        except urllib.error.HTTPError as error:
            if error.code < 500:
                raise RuntimeError(f"creating the task answered {error.code}") from None
            last = f"creating the task answered {error.code}"
        except (OSError, ValueError, KeyError) as error:
            last = f"creating the task failed ({type(error).__name__})"
    raise RuntimeError(last)


def wait_done(args, token, task_id, clock=time.monotonic, sleep=time.sleep):
    """('ok'|'failed'|'timeout', detail): polls the task until it is done,
    dead, or the deadline passes. A poll that errors is retried next round."""
    deadline = clock() + args.timeout_minutes * 60
    seen = "no status yet"
    while True:
        try:
            status, body, _ = attention.fetch(project_url(args, f"tasks/{task_id}"), token)
        except OSError as error:
            seen = f"status poll failed ({type(error).__name__})"
        else:
            task = (body or {}).get("data") if status == 200 else None
            if task is None:
                seen = f"status poll answered {status}"
            elif task.get("lifecycle") == "done":
                return "ok", "done"
            elif task.get("lifecycle") in DEAD_LIFECYCLES:
                return "failed", f"task was {task['lifecycle']}"
            else:
                seen = f"{task.get('lifecycle')}/{task.get('work_status')}"
        if clock() >= deadline:
            return "timeout", f"not done after {args.timeout_minutes:g} minutes (last seen {seen})"
        sleep(args.poll_seconds)


def served_by(ledger, task_id):
    """The set of harnesses the supervisor's cost ledger shows for the task's
    implementer launches (empty when the ledger is missing or has none)."""
    found = set()
    try:
        lines = Path(ledger).read_text().splitlines()
    except OSError:
        return found
    for line in lines:
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict) and row.get("role") == "impl" and row.get("task") == task_id:
            found.add(row.get("harness"))
    return found


def run_once(args, token, host, clock=time.monotonic, sleep=time.sleep):
    """One canary run as a result dict (never raises for a coordinator failure)."""
    run_id = uuid.uuid4().hex[:8]
    started = clock()
    result = {"host": host, "harness": args.harness, "project": args.project, "task": None,
              "served_by": None}
    try:
        result["task"] = create_task(args, token, task_body(args.harness, host, run_id, args.priority), sleep)
        outcome, detail = wait_done(args, token, result["task"], clock, sleep)
    except RuntimeError as error:
        outcome, detail = "failed", str(error)
    if outcome == "ok" and args.ledger:
        harnesses = served_by(args.ledger, result["task"])
        result["served_by"] = sorted(harnesses) or None
        if harnesses and harnesses != {args.harness}:
            outcome, detail = "failed", f"served by {', '.join(sorted(harnesses))}, not {args.harness}"
    result.update(outcome=outcome, detail=detail, duration_seconds=round(clock() - started, 1),
                  at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))
    return result


def record(path, result):
    """Appends the result as one line; a results file that cannot be written
    must not hide the outcome, so the error is reported and the run goes on."""
    try:
        with open(path, "a") as handle:
            handle.write(json.dumps(result, sort_keys=True) + "\n")
    except OSError as error:
        print(f"e2e-canary: results not recorded ({type(error).__name__})", file=sys.stderr)


def row_id(line):
    """A results line's identity at the coordinator: the SHA-256 of the line."""
    return hashlib.sha256(line.strip().encode()).hexdigest()


def report_row(line, default_project, default_host):
    """(project, host, coordinator row) for a results line, or None when the
    line is not a canary result the coordinator would take."""
    try:
        result = json.loads(line)
        occurred = calendar.timegm(time.strptime(result["at"], "%Y-%m-%dT%H:%M:%SZ")) * 1000
        outcome = result["outcome"]
        duration = float(result["duration_seconds"])
        harness = result["harness"]
    except (ValueError, KeyError, TypeError, OverflowError):
        return None
    if outcome not in OUTCOMES or not isinstance(harness, str) or not harness or duration < 0:
        return None
    served = result.get("served_by")
    return (result.get("project") or default_project, result.get("host") or default_host, {
        "row_id": row_id(line), "harness": harness, "outcome": outcome, "duration_seconds": duration,
        "detail": str(result.get("detail") or "")[:4000], "task_id": result.get("task"),
        "served_by": served if isinstance(served, list) else None, "occurred_at_ms": occurred,
    })


def post_report(args, token, project, body, key):
    """The HTTP status of one report; a transport failure raises OSError."""
    request = urllib.request.Request(
        f"{args.url.rstrip('/')}/api/v1/projects/{project}/telemetry/canary-results",
        data=json.dumps(body).encode(), method="POST")
    request.add_header("Authorization", f"Bearer {token}")
    request.add_header("Content-Type", "application/json")
    request.add_header("Idempotency-Key", key)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            response.read()
            return response.status
    except urllib.error.HTTPError as error:
        return error.code


def report_results(args, token):
    """Sends the results file's unsent rows to the coordinator and returns
    how many were delivered. The file is the source and is never changed;
    delivered rows (and rows the coordinator refused as invalid, which would
    never be taken) are listed in --reported. Anything else leaves a row for
    the next run, so nothing is lost while the coordinator is down, and the
    row identity keeps a resend from duplicating it."""
    try:
        lines = [l for l in Path(args.results).read_text().splitlines() if l.strip()]
    except OSError:
        return 0
    reported = args.reported or args.results + ".reported"
    try:
        done = set(Path(reported).read_text().split())
    except OSError:
        done = set()
    groups = {}
    for line in lines:
        if row_id(line) in done:
            continue
        converted = report_row(line, args.project, args.host)
        if converted:
            groups.setdefault(converted[:2], []).append(converted[2])
    sent = 0

    def mark(rows):
        try:
            with open(reported, "a") as handle:
                handle.write("".join(r["row_id"] + "\n" for r in rows))
        except OSError as error:
            print(f"e2e-canary: reported rows not recorded ({type(error).__name__})", file=sys.stderr)

    def deliver(project, host, rows):
        """False when the pass should stop (coordinator or credential unusable)."""
        nonlocal sent
        key = "canary-" + hashlib.sha256(f"{project}\n{host}\n{','.join(r['row_id'] for r in rows)}".encode()).hexdigest()[:40]
        try:
            status = post_report(args, token, project, {"host": host, "rows": rows}, key)
        except OSError as error:
            print(f"e2e-canary: results not reported ({type(error).__name__})", file=sys.stderr)
            return False
        if status == 200:
            mark(rows)
            sent += len(rows)
            return True
        if status in (400, 422):
            if len(rows) == 1:
                print(f"e2e-canary: result {rows[0]['row_id'][:12]} refused ({status}); not resent", file=sys.stderr)
                mark(rows)
                return True
            return all(deliver(project, host, [row]) for row in rows)
        print(f"e2e-canary: results not reported (HTTP {status})", file=sys.stderr)
        return status == 404

    for (project, host), rows in groups.items():
        for start in range(0, len(rows), REPORT_BATCH):
            if not deliver(project, host, rows[start:start + REPORT_BATCH]):
                return sent
    return sent


def page(args, result):
    """One ntfy message for a failed or timed-out run."""
    body = (f"{result['harness']} on {result['host']}: {result['outcome']} after "
            f"{result['duration_seconds']:.0f}s\n{result['detail']}\ntask {result['task']}")
    request = urllib.request.Request(
        f"{args.ntfy_url.rstrip('/')}/{args.ntfy_topic}", data=body.encode(), method="POST")
    request.add_header("Title", "agentc e2e canary failed")
    request.add_header("Priority", "high")
    request.add_header("Tags", "rotating_light")
    ntfy_token = os.environ.get("NTFY_TOKEN")
    if ntfy_token:
        request.add_header("Authorization", f"Bearer {ntfy_token}")
    with urllib.request.urlopen(request, timeout=15) as response:
        response.read()


def run(args, clock=time.monotonic, sleep=time.sleep):
    token = attention.read_token(args.token_file)
    if args.backfill:
        print(f"e2e-canary: reported {report_results(args, token)} results", file=sys.stderr)
        return 0
    result = run_once(args, token, args.host, clock, sleep)
    record(args.results, result)
    if not args.no_report:
        report_results(args, token)
    print(f"e2e-canary: {result['harness']} on {result['host']}: {result['outcome']} "
          f"in {result['duration_seconds']:.0f}s ({result['detail']})", file=sys.stderr)
    if result["outcome"] == "ok":
        return 0
    try:
        page(args, result)
    except (OSError, urllib.error.URLError) as error:
        print(f"e2e-canary: ntfy page not delivered ({type(error).__name__})", file=sys.stderr)
        return 2
    return 1


def parse(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    env = attention.env
    parser.add_argument("--harness", choices=HARNESSES, default=env("E2E_HARNESS"))
    parser.add_argument("--url", default=env("E2E_URL", DEFAULT_URL))
    parser.add_argument("--project", default=env("E2E_PROJECT"))
    parser.add_argument("--token-file", default=env("E2E_TOKEN_FILE", DEFAULT_TOKEN_FILE))
    parser.add_argument("--ntfy-topic", default=env("E2E_NTFY_TOPIC"))
    parser.add_argument("--ntfy-url", default=env("E2E_NTFY_URL", attention.DEFAULT_NTFY))
    parser.add_argument("--priority", type=int, choices=PRIORITIES,
                        default=env("E2E_PRIORITY", DEFAULT_PRIORITY, int))
    parser.add_argument("--timeout-minutes", type=float,
                        default=env("E2E_TIMEOUT_MINUTES", DEFAULT_TIMEOUT_MINUTES, float))
    parser.add_argument("--poll-seconds", type=float, default=env("E2E_POLL_SECONDS", DEFAULT_POLL_SECONDS, float))
    parser.add_argument("--results", default=env("E2E_RESULTS", DEFAULT_RESULTS))
    parser.add_argument("--ledger", default=env("E2E_LEDGER", DEFAULT_LEDGER))
    parser.add_argument("--host", default=env("E2E_HOST", socket.gethostname()))
    parser.add_argument("--reported", default=env("E2E_REPORTED"))
    parser.add_argument("--no-report", action="store_true", default=env("E2E_NO_REPORT", "0").lower() not in ("0", "false", "no"))
    parser.add_argument("--backfill", action="store_true")
    args = parser.parse_args(argv)
    # A backfill runs no canary, so it needs neither a harness nor a page topic.
    for name in ("project",) if args.backfill else ("harness", "project", "ntfy_topic"):
        if not getattr(args, name):
            parser.error(f"needs --{name.replace('_', '-')} or E2E_{name.upper()}")
    if args.timeout_minutes <= 0 or args.poll_seconds <= 0:
        parser.error("--timeout-minutes and --poll-seconds must be positive")
    return args


def main(argv=None):
    return run(parse(argv))


if __name__ == "__main__":
    sys.exit(main())
