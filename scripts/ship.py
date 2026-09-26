#!/usr/bin/env python3
"""Ship a human change to main as a checked fast-forward (autonomy plan §2.6, U2).

Pushes the clean HEAD commit to ``ac/human/<name>``, waits until every required
GitHub Actions workflow succeeds on that exact commit, then fast-forwards
``main`` to it. It never force-pushes, and success is judged by reading the
remote back, never by a push exit code.

Usage: python3 scripts/ship.py [--name NAME] [--remote origin] [--target main]
"""
import argparse
import json
import subprocess
import sys
import time

REQUIRED_WORKFLOWS = ("Coordination checks", "Documentation checks")
POLL_SECONDS = 15
APPEAR_TIMEOUT_SECONDS = 300


def run(*args, check=True):
    """Runs a command and returns its stripped stdout."""
    result = subprocess.run(args, text=True, capture_output=True)
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


def runs_for(sha):
    """Lists the latest run of each required workflow on sha."""
    fields = "databaseId,workflowName,status,conclusion,createdAt"
    runs = json.loads(run("gh", "run", "list", "--commit", sha, "--json", fields))
    latest = {}
    for item in sorted(runs, key=lambda r: r["createdAt"]):
        latest[item["workflowName"]] = item
    return {name: latest[name] for name in REQUIRED_WORKFLOWS if name in latest}


def wait_for_checks(sha):
    """Waits until every required workflow finished on sha; exits on failure."""
    deadline = time.time() + APPEAR_TIMEOUT_SECONDS
    while True:
        runs = runs_for(sha)
        if len(runs) == len(REQUIRED_WORKFLOWS) and all(r["status"] == "completed" for r in runs.values()):
            break
        if len(runs) < len(REQUIRED_WORKFLOWS) and time.time() > deadline:
            sys.exit(f"ship: required workflows never started on {sha[:12]}")
        time.sleep(POLL_SECONDS)
    failed = [n for n, r in runs.items() if r["conclusion"] != "success"]
    if failed:
        sys.exit(f"ship: {', '.join(failed)} did not succeed on {sha[:12]}")


def main():
    """Pushes, waits for green checks, and fast-forwards the target."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--name", default=run("git", "branch", "--show-current") or "ship")
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--target", default="main")
    args = parser.parse_args()
    sha = clean_head()
    require_fast_forward(args.remote, args.target, sha)
    run("git", "push", args.remote, f"{sha}:refs/heads/ac/human/{args.name}", check=False)
    if remote_tip(args.remote, f"ac/human/{args.name}") != sha:
        sys.exit(f"ship: ac/human/{args.name} does not point at {sha[:12]}")
    print(f"ship: waiting for {', '.join(REQUIRED_WORKFLOWS)} on {sha[:12]}")
    wait_for_checks(sha)
    require_fast_forward(args.remote, args.target, sha)
    run("git", "push", args.remote, f"{sha}:refs/heads/{args.target}", check=False)
    if remote_tip(args.remote, args.target) != sha:
        sys.exit(f"ship: {args.target} was not fast-forwarded to {sha[:12]}")
    print(f"ship: {args.target} is now {sha}")


if __name__ == "__main__":
    main()
