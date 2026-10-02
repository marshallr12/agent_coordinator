#!/usr/bin/env python3
"""Ship a human change to main as a checked fast-forward (autonomy plan §2.6, U2).

Pushes the clean HEAD commit to ``ac/human/<name>``, waits until every required
GitHub Actions workflow succeeds on that exact commit, then fast-forwards
``main`` to it. It never force-pushes, and success is judged by reading the
remote back, never by a push exit code.

Usage: python3 scripts/ship.py [--name NAME] [--remote origin] [--target main]
                             [--checks-timeout SECONDS]
"""
import argparse
import json
import math
import subprocess
import sys
import time

REQUIRED_WORKFLOWS = ("Coordination checks", "Documentation checks")
POLL_SECONDS = 15
APPEAR_TIMEOUT_SECONDS = 300
QUERY_TIMEOUT_SECONDS = 300
CHECKS_TIMEOUT_SECONDS = 1800


def run(*args, check=True, timeout=None):
    """Runs a command and returns its stripped stdout."""
    result = subprocess.run(args, text=True, capture_output=True, timeout=timeout)
    if check and result.returncode != 0:
        sys.exit(f"ship: {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def clean_head():
    """Returns HEAD's commit id, refusing a dirty working tree."""
    if run("git", "status", "--porcelain"):
        sys.exit("ship: commit or stash local changes first")
    return run("git", "rev-parse", "HEAD")


def remote_tip(remote, branch):
    """Reads the remote branch's current commit id ('' when absent)."""
    line = run("git", "ls-remote", "--refs", remote, f"refs/heads/{branch}")
    return line.split()[0] if line else ""


def require_fast_forward(remote, target, sha):
    """Refuses unless the target's remote tip is an ancestor of sha."""
    run("git", "fetch", "--no-tags", remote, f"refs/heads/{target}")
    tip = remote_tip(remote, target)
    ancestor = subprocess.run(["git", "merge-base", "--is-ancestor", tip, sha])
    if ancestor.returncode != 0:
        sys.exit(f"ship: {target} ({tip[:12]}) is not an ancestor; rebase first")


def runs_for(sha, timeout=None):
    """Lists the latest run of each required workflow on sha."""
    fields = "databaseId,workflowName,status,conclusion,createdAt"
    runs = json.loads(run("gh", "run", "list", "--commit", sha, "--json", fields,
                          timeout=timeout))
    latest = {}
    for item in sorted(runs, key=lambda r: r["createdAt"]):
        latest[item["workflowName"]] = item
    return {name: latest[name] for name in REQUIRED_WORKFLOWS if name in latest}


def positive_seconds(value):
    """Parse a finite, positive timeout for argparse and direct callers."""
    try:
        seconds = float(value)
    except (TypeError, ValueError):
        raise argparse.ArgumentTypeError("must be a positive, finite number of seconds") from None
    if not math.isfinite(seconds) or seconds <= 0:
        raise argparse.ArgumentTypeError("must be a positive, finite number of seconds")
    return seconds


def wait_for_checks(sha, checks_timeout=CHECKS_TIMEOUT_SECONDS):
    """Wait for checks on sha within monotonic appearance and overall deadlines."""
    started = time.monotonic()
    deadline = started + positive_seconds(checks_timeout)
    appear_deadline = started + APPEAR_TIMEOUT_SECONDS
    missing = True

    def require_budget(now):
        if now >= deadline:
            sys.exit(f"ship: timed out waiting for required workflows on {sha}")
        if missing and now >= appear_deadline:
            sys.exit(f"ship: required workflows never started on {sha}")

    while True:
        now = time.monotonic()
        require_budget(now)
        # Keep each subprocess timeout safe for OS polling even when callers
        # choose a very large finite overall budget.
        remaining = min(deadline - now, QUERY_TIMEOUT_SECONDS)
        if missing:
            remaining = min(remaining, appear_deadline - now)
        try:
            runs = runs_for(sha, timeout=remaining)
        except subprocess.TimeoutExpired:
            require_budget(time.monotonic())
            sys.exit(f"ship: GitHub Actions query timed out on {sha}")
        now = time.monotonic()
        # A completed response returned after either applicable deadline cannot
        # authorize publication, even when its conclusions all say success.
        require_budget(now)
        missing = len(runs) < len(REQUIRED_WORKFLOWS)
        require_budget(now)
        if len(runs) == len(REQUIRED_WORKFLOWS) and all(r["status"] == "completed" for r in runs.values()):
            failed = [n for n, r in runs.items() if r["conclusion"] != "success"]
            if failed:
                sys.exit(f"ship: {', '.join(failed)} did not succeed on {sha}")
            require_budget(time.monotonic())
            return
        now = time.monotonic()
        require_budget(now)
        sleep_for = min(POLL_SECONDS, deadline - now)
        if missing:
            sleep_for = min(sleep_for, appear_deadline - now)
        time.sleep(sleep_for)


def main():
    """Pushes, waits for green checks, and fast-forwards the target."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--name", default=run("git", "branch", "--show-current") or "ship")
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--target", default="main")
    parser.add_argument("--checks-timeout", type=positive_seconds, default=CHECKS_TIMEOUT_SECONDS,
                        metavar="SECONDS", help="overall Actions wait limit (default: 1800 seconds)")
    args = parser.parse_args()
    sha = clean_head()
    require_fast_forward(args.remote, args.target, sha)
    run("git", "push", args.remote, f"{sha}:refs/heads/ac/human/{args.name}", check=False)
    if remote_tip(args.remote, f"ac/human/{args.name}") != sha:
        sys.exit(f"ship: ac/human/{args.name} does not point at {sha[:12]}")
    print(f"ship: waiting for {', '.join(REQUIRED_WORKFLOWS)} on {sha[:12]}")
    wait_for_checks(sha, checks_timeout=args.checks_timeout)
    require_fast_forward(args.remote, args.target, sha)
    run("git", "push", args.remote, f"{sha}:refs/heads/{args.target}", check=False)
    if remote_tip(args.remote, args.target) != sha:
        sys.exit(f"ship: {args.target} was not fast-forwarded to {sha[:12]}")
    print(f"ship: {args.target} is now {sha}")


if __name__ == "__main__":
    main()
