#!/usr/bin/env python3
"""Ship a human change to main as a checked fast-forward (autonomy plan §2.6, U2).

Pushes the clean HEAD commit to ``ac/human/<name>``, waits until every required
GitHub Actions workflow succeeds on that exact commit, then fast-forwards
``main`` to it. It never force-pushes, and success is judged by reading the
remote back, never by a push exit code.

The selected remote is resolved once to literal fetch and push URL snapshots
naming one GitHub repository; every later Git and gh operation uses those
snapshots. Git would re-resolve a literal URL through a configured remote of
the same name or a URL rewrite rule, so either is refused before transport,
even when a longer rewrite rule would win. Git configuration must remain
unchanged during shipping; this preflight does not defend against concurrent
malicious configuration changes.

Usage: python3 scripts/ship.py [--name NAME] [--remote origin] [--target main]
                             [--checks-timeout SECONDS]
"""
import argparse
import json
import math
import re
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
    """Reads the remote branch's current commit id ('' when absent).

    ls-remote patterns match any ref ending in the pattern, so a decoy such
    as refs/heads/a/refs/heads/main is also listed; only the exact ref counts.
    """
    ref = f"refs/heads/{branch}"
    for line in run("git", "ls-remote", "--refs", remote, ref).splitlines():
        sha, separator, name = line.partition("\t")
        if separator and name == ref:
            return sha
    return ""


def require_fast_forward(remote, target, sha):
    """Refuses unless the target's remote tip is an ancestor of sha."""
    run("git", "fetch", "--no-tags", remote, f"refs/heads/{target}")
    # Judge the commit actually fetched, not a second, possibly newer readback.
    tip = run("git", "rev-parse", "FETCH_HEAD")
    ancestor = subprocess.run(["git", "merge-base", "--is-ancestor", tip, sha])
    if ancestor.returncode != 0:
        sys.exit(f"ship: {target} ({tip[:12]}) is not an ancestor; rebase first")


REPOSITORY_URL = re.compile(
    r"(?:https://(?i:github\.com)/|git@(?i:github\.com):|"
    r"ssh://git@(?i:github\.com)(?::22)?/)"
    r"([A-Za-z0-9][A-Za-z0-9-]*)/([A-Za-z0-9_.-]+)/?",
    # ASCII-only case folding: Unicode rules would let "gıthub.com" (U+0131) match.
    re.ASCII,
)


def repository_from_url(url):
    """Parses a supported GitHub clone URL into OWNER/NAME without guessing."""
    match = REPOSITORY_URL.fullmatch(url)
    if match:
        owner, name = match.groups()
        if name.endswith(".git"):
            name = name[:-4]
        # Another-case ".GIT" suffix would leave gh and Git naming different repositories.
        if name and name not in (".", "..") and not name.lower().endswith(".git"):
            return f"{owner}/{name}"
    sys.exit("ship: selected remote must have a supported GitHub clone URL")


def remote_urls(remote):
    """Snapshots the remote's single fetch URL and single push URL."""
    fetch_urls = run("git", "remote", "get-url", "--all", remote).splitlines()
    push_urls = run("git", "remote", "get-url", "--push", "--all", remote).splitlines()
    if len(fetch_urls) != 1 or len(push_urls) != 1:
        sys.exit("ship: selected remote must have exactly one fetch and push URL")
    return fetch_urls[0], push_urls[0]


def config_records(pattern, purpose, name_only=False):
    """Reads raw NUL-terminated `git config --get-regexp` records, failing closed.

    Stripping or splitting on whitespace would change what the rules mean, so
    records are kept byte-for-byte; exit 1 means that nothing matched.
    """
    args = ["git", "config", "--null"] + (["--name-only"] if name_only else [])
    result = subprocess.run(args + ["--get-regexp", pattern], text=True, capture_output=True)
    if result.returncode == 1:
        return []
    if result.returncode != 0:
        sys.exit(f"ship: unable to inspect Git {purpose}")
    if not result.stdout.endswith("\0"):
        sys.exit(f"ship: unable to parse Git {purpose}")
    return result.stdout[:-1].split("\0")


def parse_rewrite_rule(record):
    """Splits a `url.<base>.<kind>` record into (replacement, kind, prefix)."""
    key, separator, prefix = record.partition("\n")
    base, _, kind = key.rpartition(".")
    if not separator or not base.startswith("url.") or kind not in ("insteadof", "pushinsteadof"):
        sys.exit("ship: unable to parse Git URL rewrite rules")
    return base[4:], kind, prefix


def refuse_url_rewrites(fetch_url, push_url):
    """Refuses any insteadOf/pushInsteadOf rule that would change a snapshot."""
    pattern = r"^url\..*\.(insteadof|pushinsteadof)$"
    for record in config_records(pattern, "URL rewrite rules"):
        replacement, kind, prefix = parse_rewrite_rule(record)
        # insteadOf also affects ls-remote readback through the push URL.
        urls = (fetch_url, push_url) if kind == "insteadof" else (push_url,)
        for url in urls:
            if url.startswith(prefix) and replacement + url[len(prefix):] != url:
                sys.exit("ship: Git URL rewrite rules would change a selected URL")


def refuse_remote_name_aliases(fetch_url, push_url):
    """Refuses a configured remote whose name equals a snapshot URL.

    Git resolves a positional repository argument as a remote name before
    treating it as a URL, so `remote.<snapshot>.url` or `.pushurl` would
    silently send fetch, push or readback to an unchecked repository.
    """
    snapshots = {fetch_url.casefold(), push_url.casefold()}
    for key in config_records(r"^remote\.", "remote configuration", name_only=True):
        if not key.startswith("remote."):
            sys.exit("ship: unable to parse Git remote configuration")
        name, separator, _ = key[len("remote."):].rpartition(".")
        if not separator:
            continue  # A section-level key such as remote.pushDefault names no remote.
        if name.casefold() in snapshots:
            sys.exit("ship: a Git remote named like a selected URL would redirect it")


def validated_remote(remote):
    """Snapshots one GitHub identity and refuses any later Git re-resolution."""
    fetch_url, push_url = remote_urls(remote)
    repository = repository_from_url(fetch_url)
    if repository.lower() != repository_from_url(push_url).lower():
        sys.exit("ship: selected remote fetch and push repositories differ")
    refuse_url_rewrites(fetch_url, push_url)
    refuse_remote_name_aliases(fetch_url, push_url)
    return fetch_url, push_url, repository


def runs_for(sha, repository, timeout=None):
    """Lists the latest run of each required workflow on sha in repository."""
    fields = "databaseId,workflowName,status,conclusion,createdAt"
    # A full github.com host stops GH_HOST from redirecting the query.
    runs = json.loads(run("gh", "run", "list", "--repo", f"github.com/{repository}",
                          "--commit", sha, "--json", fields, timeout=timeout))
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


def wait_for_checks(sha, repository, checks_timeout=CHECKS_TIMEOUT_SECONDS):
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
            runs = runs_for(sha, repository, timeout=remaining)
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


def parse_args():
    """Parses the command line; the defaults ship the current branch to main."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--name", default=run("git", "branch", "--show-current") or "ship")
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--target", default="main")
    parser.add_argument("--checks-timeout", type=positive_seconds, default=CHECKS_TIMEOUT_SECONDS,
                        metavar="SECONDS", help="overall Actions wait limit (default: 1800 seconds)")
    return parser.parse_args()


def push_and_confirm(push_url, sha, branch, failure):
    """Pushes sha to branch and judges success only by reading the remote back."""
    run("git", "push", push_url, f"{sha}:refs/heads/{branch}", check=False)
    if remote_tip(push_url, branch) != sha:
        sys.exit(f"ship: {failure}")


def main():
    """Pushes, waits for green checks, and fast-forwards the target."""
    args = parse_args()
    sha = clean_head()
    fetch_url, push_url, repository = validated_remote(args.remote)
    require_fast_forward(fetch_url, args.target, sha)
    human = f"ac/human/{args.name}"
    push_and_confirm(push_url, sha, human, f"{human} does not point at {sha[:12]}")
    print(f"ship: waiting for {', '.join(REQUIRED_WORKFLOWS)} on {sha[:12]}")
    wait_for_checks(sha, repository, checks_timeout=args.checks_timeout)
    require_fast_forward(fetch_url, args.target, sha)
    push_and_confirm(push_url, sha, args.target,
                     f"{args.target} was not fast-forwarded to {sha[:12]}")
    print(f"ship: {args.target} is now {sha}")


if __name__ == "__main__":
    main()
