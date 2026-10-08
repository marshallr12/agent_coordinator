#!/usr/bin/env python3
"""One-time setup of a host's end-to-end canary project (run once per host).

    canary-setup.py --repository-url URL --seed-dir DIR [--url URL] [--host NAME]
                    [--username NAME] [--password-file FILE]
                    [--required-check IDENTITY:VERSION:ENVIRONMENT ...]

Creates the dedicated canary project on the coordinator (`canary-<host>`, the
autonomy policy with automatic integration and agent or human review, and the
one-check required roster the seed's workflow satisfies) and writes the
repository seed into --seed-dir, a new Git repository with one commit on
`main`: `.agent-coordinator.toml`, `CANARY.md`, a roster and the workflow that
runs `git diff --check`. The script never pushes. It prints the project id and
the remaining owner steps (push the seed to URL, issue the canary token,
supervisor binding, host-setup settings).

The coordinator login is an operator's: the password comes from
--password-file, else the CANARY_SETUP_PASSWORD variable, else a prompt, and
is never printed. Standard library only; it reuses staging.py's API helpers.
"""
import argparse
import getpass
import importlib.util
import os
from pathlib import Path
import socket
import subprocess
import sys

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("staging", HERE / "staging.py")
staging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(staging)

DEFAULT_URL = "https://agents.sithbit.com"
CHECK = "canary-diff-check:v1:any"
CHECK_NAME = "Canary diff check"
WORKFLOW = ".github/workflows/canary.yml"

SEED = {
    "CANARY.md": "# Canary\n\nEach scheduled canary run appends one line below.\n",
    ".agent-coordinator/roster.toml": (
        "# The one required check of the canary project.\n"
        "[[required_checks]]\n"
        'identity = "canary-diff-check"\n'
        f'check_name = "{CHECK_NAME}"\n'
        f'workflow_path = "{WORKFLOW}"\n'),
    WORKFLOW: (
        "name: Canary\n"
        "on: [push, pull_request]\n"
        "jobs:\n"
        "  diff-check:\n"
        f"    name: {CHECK_NAME}\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        "      - uses: actions/checkout@v4\n"
        "        with:\n"
        "          fetch-depth: 2\n"
        "      - run: git diff --check HEAD~1\n"),
}


def write_seed(directory, url, project):
    """Writes the seed tree into `directory` and commits it on `main`.
    Refuses a directory that already holds files."""
    directory = Path(directory)
    if directory.exists() and any(directory.iterdir()):
        sys.exit(f"{directory} is not empty")
    files = dict(SEED)
    files[".agent-coordinator.toml"] = f'service_url = "{url}"\nproject_id = "{project}"\n'
    for name, text in files.items():
        path = directory / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    git = ["git", "-C", str(directory), "-c", "user.name=agentc canary", "-c", "user.email=canary@agentc.invalid"]
    subprocess.run([*git, "init", "--quiet", "--initial-branch=main"], check=True)
    subprocess.run([*git, "add", "--all"], check=True)
    subprocess.run([*git, "commit", "--quiet", "--message", "Canary project seed"], check=True)


def create_project(args, password):
    """Logs in as the operator and creates the project; returns its id."""
    api = staging.Api(args.url.rstrip("/"), args.username, password)
    project = staging.create_project(None, api, args.repository_url, name=f"canary-{args.host}")
    checks = args.required_check or [staging.parse_check(CHECK)]
    staging.set_required_checks(api, project, checks)
    return project


def read_password(args):
    if args.password_file:
        return Path(args.password_file).read_text().strip()
    return os.environ.get("CANARY_SETUP_PASSWORD") or getpass.getpass("coordinator operator password: ")


def next_steps(args, project):
    return f"""canary project {project} created for host {args.host}.
Remaining owner steps:
  1. Push the seed to the canary repository:
       git -C {args.seed_dir} remote add origin {args.repository_url}
       git -C {args.seed_dir} push origin main
  2. Issue an agent token that may create tasks in the project (an
     interactive/write agent in the operator UI), and install it with
       sudo install -o root -g root -m 0400 /dev/stdin /etc/agentc/e2e-canary-token
  3. Serve the project from this host's supervisor: its `[run.binding]` (or
     a second supervisor instance) names service_url = "{args.url}" and
     project_id = "{project}"; reviewer launches must be enabled there.
  4. Set E2E_PROJECT={project}, E2E_NTFY_TOPIC and E2E_HARNESSES in
     /etc/agentc/e2e-canary.env and re-run deploy/agentc/host-setup.sh.
"""


def parse(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--repository-url", required=True, help="the canary repository's Git URL")
    parser.add_argument("--seed-dir", required=True, help="new directory for the repository seed")
    parser.add_argument("--url", default=DEFAULT_URL)
    parser.add_argument("--host", default=socket.gethostname())
    parser.add_argument("--username", default="admin", help="coordinator operator login")
    parser.add_argument("--password-file")
    parser.add_argument("--required-check", action="append", type=staging.parse_check,
                        metavar="IDENTITY:VERSION:ENVIRONMENT",
                        help=f"replaces the default required check ({CHECK})")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse(argv)
    seed = Path(args.seed_dir)
    if seed.exists() and any(seed.iterdir()):
        sys.exit(f"{seed} is not empty")
    project = create_project(args, read_password(args))
    write_seed(seed, args.url, project)
    print(next_steps(args, project), end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
