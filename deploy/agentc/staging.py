#!/usr/bin/env python3
"""Staging coordinator on this host for supervised agents (autonomy plan P2).

    deploy/agentc/staging.py up            # first run bootstraps, then serves
    deploy/agentc/staging.py status
    deploy/agentc/staging.py cli ROLE ...  # ROLE: owner | impl | rev
    deploy/agentc/staging.py credentials   # print the supervised-install commands
    deploy/agentc/staging.py down
    deploy/agentc/staging.py destroy --yes

The server runs on 127.0.0.1:18080 (the port host-setup.sh opens to the agent
uids) with its default configuration and a throwaway project whose Git remote
is a local bare repository. The first `up` issues three credentials (owner:
interactive/write, impl: supervised/write, rev: supervised/read) and one
staging-only operator login, the reviewer's `verification_env` credential.

The production credential can never reach staging: every child process gets
an environment with all AGENT_COORDINATOR_* and COORDINATOR_* variables
removed, and `cli` refuses to run if a token is still present. Secrets are
written only under the 0700 state directory and never printed.
Python's standard library is enough.
"""
import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[2]
ROLES = {"owner": ("interactive", "write"), "impl": ("supervised", "write"),
         "rev": ("supervised", "read")}
# Mirrors the production autonomy policy (plan §3, policy revision 6).
POLICY = {"review_mode": "either", "recovery_mode": "agent", "rules": "",
          "agent_rule_editing": True, "automatic_integration": True,
          "allow_subagent_reviews": True}
AGENT_STATE = Path("/var/lib/agentc")


def default_dir():
    """The state directory: $AGENTC_STAGING_DIR, else under XDG state home."""
    if os.environ.get("AGENTC_STAGING_DIR"):
        return Path(os.environ["AGENTC_STAGING_DIR"])
    base = os.environ.get("XDG_STATE_HOME") or str(Path.home() / ".local/state")
    return Path(base) / "agentc-staging"


def default_binary(name):
    """A built workspace binary from the first profile (release, then debug)
    that has the server, so the server and CLI always come from one build and
    pass the service's exact-client check."""
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    for profile in ("release", "debug"):
        if (target / profile / "agent-coordinator-server").is_file():
            return target / profile / name
    return target / "release" / name


def clean_env(**extra):
    """This process's environment minus every coordinator variable, plus `extra`."""
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("AGENT_COORDINATOR_", "COORDINATOR_"))}
    env.update({k: str(v) for k, v in extra.items()})
    return env


def write_private(path, text):
    """Writes `text` to a new 0600 file (replacing any previous one)."""
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.unlink(missing_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write(text)


class Staging:
    """Paths and operations for one staging instance."""

    def __init__(self, args):
        self.dir = Path(args.dir).resolve()
        self.port = args.port
        self.origin = f"http://127.0.0.1:{self.port}"
        self.server = Path(args.server)
        self.cli_bin = Path(args.cli)

    def path(self, *parts):
        """A path inside the state directory."""
        return self.dir.joinpath(*parts)

    def server_command(self, *args):
        """The server invocation for this instance's database and port."""
        return [str(self.server), "--database", str(self.path("db", "coordinator.sqlite3")),
                "--listen", f"127.0.0.1:{self.port}", "--public-origin", self.origin,
                "--allow-insecure-loopback", *args]

    def state(self):
        """The bootstrap record, or None before the first `up`."""
        path = self.path("state.json")
        return json.loads(path.read_text()) if path.is_file() else None

    def pid(self):
        """The running server's pid, or None (stale pid files are ignored)."""
        path = self.path("server.pid")
        if not path.is_file():
            return None
        pid = int(path.read_text())
        try:
            cmdline = Path(f"/proc/{pid}/cmdline").read_bytes()
        except OSError:
            return None
        return pid if str(self.server).encode() in cmdline else None


def healthy(origin):
    """True when the server answers /healthz."""
    try:
        with urllib.request.urlopen(origin + "/healthz", timeout=1):
            return True
    except (urllib.error.URLError, OSError):
        return False


def start_server(st):
    """Starts `serve` detached, logging to server.log; waits until healthy."""
    log = open(st.path("server.log"), "ab")
    process = subprocess.Popen(st.server_command("serve"), stdout=log, stderr=log,
                               stdin=subprocess.DEVNULL, env=clean_env(),
                               start_new_session=True)
    st.path("server.pid").write_text(str(process.pid))
    for _ in range(200):
        if healthy(st.origin):
            return
        if process.poll() is not None:
            sys.exit(f"server exited; see {st.path('server.log')}")
        time.sleep(0.05)
    sys.exit("server did not become healthy")


class Api:
    """A logged-in operator browser session (cookie + CSRF), like smoke.py."""

    def __init__(self, origin, username, password):
        self.origin = origin
        self.csrf = ""
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        login = self.call("/api/v1/auth/login", {"username": username, "password": password})
        self.csrf = login["csrf_token"]

    def call(self, path, body=None, method=None):
        """Sends one request and returns its `data`; exits on an API error."""
        headers = {"Content-Type": "application/json", "Origin": self.origin,
                   "X-CSRF-Token": self.csrf, "Idempotency-Key": str(uuid.uuid4())}
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.origin + path, data=data,
                                         headers=headers, method=method)
        try:
            with self.opener.open(request, timeout=15) as response:
                return json.load(response)["data"]
        except urllib.error.HTTPError as error:
            failure = json.load(error).get("error", {})
            sys.exit(f"{path}: {error.code} {failure.get('code')} {failure.get('message')}")


def init_admin(st):
    """Creates the database and its admin; the password stays in secrets/."""
    password = secrets.token_urlsafe(24)
    result = subprocess.run(st.server_command("init-admin", "--username", "staging-admin",
                                              "--password-stdin"),
                            input=password + "\n", text=True, capture_output=True,
                            env=clean_env(), timeout=60)
    if result.returncode != 0:
        sys.exit(f"init-admin failed: {result.stderr.strip()}")
    write_private(st.path("secrets", "admin.json"),
                  json.dumps({"username": "staging-admin", "password": password}))


def seed_remote(st, seed):
    """Creates the bare remote from `seed`'s main branch (local clone, cheap)."""
    remote = st.path("remote.git")
    subprocess.run(["git", "clone", "--quiet", "--bare", "--single-branch",
                    "--branch", "main", str(seed), str(remote)], check=True)
    return remote


def create_project(st, api, remote):
    """Creates the throwaway project and applies the autonomy policy."""
    project = api.call("/api/v1/projects", {"name": "Staging", "target_branch": "main",
                                            "repository_url": str(remote)})
    policy = dict(POLICY, expected_revision=project["policy_revision"],
                  lease_seconds=project["lease_seconds"])
    api.call(f"/api/v1/projects/{project['id']}/policy", policy, method="PATCH")
    return project["id"]


def issue_credentials(st, api):
    """Issues one agent credential per role; each lands in its own 0600 file."""
    issued = {}
    for role, (klass, access) in ROLES.items():
        agent = api.call("/api/v1/admin/agents", {"name": f"staging-{role}",
                                                  "class": klass, "access": access})
        write_private(st.path("home", role, "credentials.toml"),
                      f'[[credentials]]\norigin = "{st.origin}"\ntoken = "{agent["token"]}"\n')
        issued[role] = agent["credential_id"]
    return issued


def create_verifier(st, api, project):
    """Creates the staging-only operator login for UI verification (plan M2)."""
    password = secrets.token_urlsafe(24)
    api.call("/api/v1/admin/operators", {"name": "staging-verifier", "role": "operator",
                                         "password": password})
    Api(st.origin, "staging-verifier", password)  # proves the login works
    write_private(st.path("secrets", "verification.json"), json.dumps(
        {"project_id": project, "url": st.origin, "username": "staging-verifier",
         "password": password}, indent=2) + "\n")


def bootstrap(st, seed):
    """First `up`: database, server, project, credentials, binding, state."""
    st.dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    st.path("db").mkdir(mode=0o700, exist_ok=True)
    init_admin(st)
    start_server(st)
    admin = json.loads(st.path("secrets", "admin.json").read_text())
    api = Api(st.origin, admin["username"], admin["password"])
    project = create_project(st, api, seed_remote(st, seed))
    credentials = issue_credentials(st, api)
    create_verifier(st, api, project)
    st.path("binding.toml").write_text(
        f'service_url = "{st.origin}"\nproject_id = "{project}"\n')
    st.path("state.json").write_text(json.dumps(
        {"origin": st.origin, "project_id": project, "credentials": credentials}, indent=2))


def cmd_up(st, args):
    """Bootstraps on first use, else (re)starts the server if it is not running."""
    if st.state() is None:
        bootstrap(st, args.seed)
    elif st.pid() is None:
        start_server(st)
    print(f"staging up: {st.origin} project {st.state()['project_id']} (pid {st.pid()})")


def cmd_status(st, _args):
    """Prints the pid, health and project; exit 1 when not serving."""
    state, pid = st.state(), st.pid()
    ok = pid is not None and healthy(st.origin)
    print(json.dumps({"dir": str(st.dir), "origin": st.origin, "pid": pid, "healthy": ok,
                      "project_id": state and state["project_id"]}, indent=2))
    return 0 if ok else 1


def cmd_down(st, _args):
    """Stops the server gracefully (SIGTERM, then SIGKILL after 10 s)."""
    pid = st.pid()
    if pid is None:
        print("staging not running")
        return 0
    os.kill(pid, signal.SIGTERM)
    for _ in range(100):
        if st.pid() is None:
            break
        time.sleep(0.1)
    else:
        os.kill(pid, signal.SIGKILL)
    st.path("server.pid").unlink(missing_ok=True)
    print("staging stopped")
    return 0


def cmd_destroy(st, args):
    """Stops the server and deletes the whole state directory."""
    if not args.yes:
        sys.exit("destroy deletes the staging database and credentials; pass --yes")
    cmd_down(st, args)
    subprocess.run(["rm", "-rf", "--", str(st.dir)], check=True)
    print(f"removed {st.dir}")
    return 0


def cli_env(st, role):
    """The staging-only CLI environment for `role`; never carries a token."""
    env = clean_env(AGENT_COORDINATOR_HOME=st.path("home", role),
                    AGENT_COORDINATOR_STATE_DIR=st.path("state", role),
                    AGENT_COORDINATOR_REPO_CONFIG=st.path("binding.toml"),
                    AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK="true",
                    AGENT_COORDINATOR_SESSION=os.environ.get("AGENTC_STAGING_SESSION",
                                                             f"staging-{role}"))
    leaked = [k for k in env if k.endswith("_TOKEN") and k.startswith("AGENT_COORDINATOR")]
    if leaked:
        sys.exit(f"refusing: {leaked} present in the staging environment")
    return env


def cmd_cli(st, args):
    """Runs the CLI as `role` against staging (replaces this process). The
    session name defaults to `staging-<role>`; set AGENTC_STAGING_SESSION to
    run several independent harness sessions for one role."""
    if st.state() is None:
        sys.exit("staging is not bootstrapped; run `up` first")
    st.path("state", args.role).mkdir(mode=0o700, parents=True, exist_ok=True)
    env = cli_env(st, args.role)
    os.execve(str(st.cli_bin), [str(st.cli_bin), *args.args], env)


def cmd_credentials(st, _args):
    """Prints the root commands that install the supervised credentials and the
    reviewer's verification login where supervised launches read them."""
    state = st.state() or sys.exit("staging is not bootstrapped; run `up` first")
    for role in ("impl", "rev"):
        target = AGENT_STATE / role / "coordinator" / "credentials.toml"
        print(f"sudo install -o agentc-{role} -g agentc-{role} -m 0600 "
              f"{st.path('home', role, 'credentials.toml')} {target}")
    target = AGENT_STATE / "rev" / "verification" / f"{state['project_id']}.json"
    print(f"sudo install -o agentc-rev -g agentc-rev -m 0600 "
          f"{st.path('secrets', 'verification.json')} {target}")
    print("# Replaces any existing credentials.toml; merge by hand if production "
          "entries are already installed (entries are keyed by origin).")
    return 0


def parser():
    """Command-line interface; every setting has a local default."""
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--dir", default=default_dir(), help="state directory")
    p.add_argument("--port", type=int, default=int(os.environ.get("STAGING_PORT", 18080)))
    p.add_argument("--server", default=default_binary("agent-coordinator-server"))
    p.add_argument("--cli", default=default_binary("agent-coordinator"))
    sub = p.add_subparsers(dest="command", required=True)
    up = sub.add_parser("up", help="bootstrap on first use and serve")
    up.add_argument("--seed", default=ROOT, help="repository whose main seeds the remote")
    for name in ("status", "down", "credentials"):
        sub.add_parser(name)
    destroy = sub.add_parser("destroy")
    destroy.add_argument("--yes", action="store_true")
    cli = sub.add_parser("cli", help="run agent-coordinator as a staging role")
    cli.add_argument("role", choices=sorted(ROLES))
    cli.add_argument("args", nargs=argparse.REMAINDER)
    return p


def main():
    """Dispatches the subcommand."""
    args = parser().parse_args()
    handlers = {"up": cmd_up, "status": cmd_status, "down": cmd_down, "destroy": cmd_destroy,
                "cli": cmd_cli, "credentials": cmd_credentials}
    return handlers[args.command](Staging(args), args) or 0


if __name__ == "__main__":
    sys.exit(main())
