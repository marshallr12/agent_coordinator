#!/usr/bin/env python3
"""Root-owned pull updater for a supervised Linux host (autonomy plan R-P6).

    agentc-update.py [--check] [--retry] [--rollback core|harness]

Run by the agentc-update systemd timer. It fetches the latest GitHub release
of the project, verifies the release's SHA256SUMS asset and its build
attestation, and unpacks the bundle side by side under <prefix>/releases/<tag>.
Nothing live changes until the bundle verifies. It then:

  1. drains the supervisor: creates the kill switch (the loop claims nothing
     more), waits for the running launch to finish, and stops agentc-run;
  2. switches agentc-supervisor, agent-coordinator and agentc-push in
     <prefix>/bin, each by an atomic rename, after saving the old ones;
  3. runs the supervisor's preflight, restarts agentc-run and runs the
     end-to-end canary (e2e-canary.py) for each configured harness;
  4. rolls back automatically, by the same drain, when the switch, the
     preflight, the restart or the canary fails, and pages through ntfy.

A bundle that carries harness binaries (harness.json) gets a second, staged
step once the core update is promoted: the new claude/codex binaries and their
pins in supervisor.toml replace the old ones while the supervisor is drained,
and are promoted only when preflight, the canary and containment-suite.sh all
pass; otherwise the old binaries and pins come back. A rejected release is
remembered and not retried until --retry or a newer release. A launch is never
interrupted: when it outlasts --drain-timeout the run is deferred to the next
timer tick.

--check reports what a run would do and changes nothing. --rollback restores
the binaries (core) or the harness binaries and pins (harness) saved by the
last update, by the same drain, and runs preflight but no canary.

Exit status: 0 up to date, promoted or deferred; 1 the release was rejected and
rolled back; 2 the update could not be attempted (fetch, verification, host
state) or a rollback failed. Every option defaults from an UPDATE_* environment
variable (see parse() and host-setup.sh's update.env); an empty variable counts
as unset and a command-line option wins. Standard library only.
"""
import argparse
import contextlib
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("attention", HERE / "attention.py")
attention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(attention)
env = attention.env

DEFAULT_REPO = "marshallr12/agent_coordinator"
DEFAULT_API = "https://api.github.com"
DEFAULT_ATTEST = "gh attestation verify {file} --repo {repo}"
DEFAULT_PREFIX = "/opt/agentc"
DEFAULT_STATE = "/var/lib/agentc"
DEFAULT_ETC = "/etc/agentc"
DEFAULT_DRAIN_MINUTES = 240.0
DEFAULT_SETTLE_SECONDS = 10.0
DEFAULT_POLL_SECONDS = 15.0
DEFAULT_KEEP = 3
DEFAULT_CANARY = "python3 -I {prefix}/bin/e2e-canary.py --harness {harness} --timeout-minutes {timeout}"
CORE_BINARIES = ("agentc-supervisor", "agent-coordinator", "agentc-push")
HARNESSES = ("claude", "codex")
UNIT = "agentc-run.service"
MAX_DOWNLOAD_BYTES = 512 * 1024 * 1024
MAX_MEMBERS = 256
MAX_EXPANDED_BYTES = 1024 * 1024 * 1024
LOOPBACK_HOSTS = ("127.0.0.1", "localhost", "::1")


class Deferred(Exception):
    """The host is busy; try again at the next timer tick."""


class Rejected(Exception):
    """The release failed a check on this host and was rolled back."""


class Failed(Exception):
    """The update could not be attempted, or could not be undone."""


class Settings:
    """Everything the updater needs, from the command line and UPDATE_* variables."""

    def __init__(self, args):
        self.repo = args.repo
        self.api = args.api_url.rstrip("/")
        self.attest = args.attest_command
        self.prefix = Path(args.prefix)
        self.state_dir = Path(args.state_dir)
        self.etc = Path(args.etc)
        self.systemctl = args.systemctl
        self.drain_seconds = args.drain_timeout_minutes * 60
        self.settle_seconds = args.settle_seconds
        self.poll_seconds = args.poll_seconds
        self.keep = args.keep
        self.harness_updates = args.harness_updates
        self.as_impl = shlex.split(args.as_impl)
        self.canary = args.canary_command or DEFAULT_CANARY
        self.canary_is_default = args.canary_command is None
        self.canary_harnesses = args.canary_harnesses.replace(",", " ").split()
        self.suite = args.suite_command
        self.ntfy_topic = args.ntfy_topic
        self.ntfy_url = args.ntfy_url
        self.allow_insecure_loopback = args.allow_insecure_loopback
        self.canary_timeout_minutes = args.canary_timeout_minutes
        self.mirror_branch = args.mirror_branch

    @property
    def bin(self):
        return self.prefix / "bin"

    @property
    def releases(self):
        return self.prefix / "releases"

    @property
    def config(self):
        return self.etc / "supervisor.toml"

    @property
    def state_file(self):
        return self.state_dir / "update-state.json"

    @property
    def results(self):
        return self.state_dir / "update.jsonl"

    @property
    def lock(self):
        return self.state_dir / "update.lock"

    @property
    def canary_lock(self):
        return self.state_dir / "e2e-canary.lock"

    def supervisor_config(self):
        try:
            return tomllib.loads(self.config.read_text())
        except (OSError, ValueError):
            return {}

    @property
    def kill_switch(self):
        configured = self.supervisor_config().get("health", {}).get("kill_switch")
        return Path(configured) if configured else self.state_dir / "kill-switch"

    @property
    def heartbeat(self):
        return Path(self.supervisor_config().get("state_dir", self.state_dir)) / "heartbeat.json"


def log(message):
    print(f"agentc-update: {message}", file=sys.stderr, flush=True)


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def version_key(tag):
    """The release tag's numeric components, or None for a tag that is not one."""
    match = re.fullmatch(r"v?(\d+(?:\.\d+){0,3})", tag.strip())
    return tuple(int(part) for part in match.group(1).split(".")) if match else None


def bare_version(tag):
    return tag.strip().removeprefix("v")


def newer(tag, installed):
    """Whether release `tag` is newer than `installed` (None: unknown, so older than any)."""
    key = version_key(tag)
    if key is None:
        return False
    have = version_key(installed) if installed else None
    return have is None or key > have


# --- host files ---------------------------------------------------------------------------------

def require_protected(path):
    """Refuses a path this updater must not write through: a symlink, a directory not owned by
    the updater's own user (root on a host), or one other users can write."""
    if path.is_symlink() or not path.is_dir():
        raise Failed(f"refusing unsafe directory {path}; owner repair required")
    info = path.stat()
    if info.st_uid != os.geteuid() or info.st_mode & 0o022:
        raise Failed(f"refusing {path}: not owned by {os.geteuid()} or writable by others; owner repair required")


def replace_file(source, destination, mode):
    """Puts a copy of `source` at `destination` by an atomic rename, root-owned when run as root."""
    if destination.is_symlink():
        raise Failed(f"refusing symlink {destination}")
    handle, temp = tempfile.mkstemp(prefix=f".{destination.name}.", dir=destination.parent)
    try:
        with os.fdopen(handle, "wb") as out, open(source, "rb") as origin:
            shutil.copyfileobj(origin, out)
            out.flush()
            os.fsync(out.fileno())
        if os.geteuid() == 0:
            os.chown(temp, 0, 0)
        os.chmod(temp, mode)
        os.replace(temp, destination)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(temp)


def write_atomic(path, text):
    handle, temp = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(handle, "w") as out:
            out.write(text)
            out.flush()
            os.fsync(out.fileno())
        os.chmod(temp, 0o644 if path.suffix == ".toml" else 0o600)
        os.replace(temp, path)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(temp)


def set_pins(text, pins):
    """`text` (supervisor.toml) with each `name = "version"` set under [pinned]."""
    lines = text.splitlines()
    start = next((i for i, line in enumerate(lines) if line.strip() == "[pinned]"), None)
    if start is None:
        return text.rstrip("\n") + "\n\n[pinned]\n" + "".join(f'{n} = "{v}"\n' for n, v in pins.items())
    end = next((i for i in range(start + 1, len(lines)) if lines[i].lstrip().startswith("[")), len(lines))
    for name, version in pins.items():
        entry = f'{name} = "{version}"'
        at = next((i for i in range(start + 1, end) if re.match(rf"\s*{re.escape(name)}\s*=", lines[i])), None)
        if at is None:
            lines.insert(end, entry)
            end += 1
        else:
            lines[at] = entry
    return "\n".join(lines) + "\n"


# --- state --------------------------------------------------------------------------------------

def load_state(settings):
    try:
        state = json.loads(settings.state_file.read_text())
    except (OSError, ValueError):
        state = {}
    state.setdefault("core", {})
    state.setdefault("harness", {})
    state.setdefault("rejected", [])
    state.setdefault("harness_rejected", [])
    return state


def save_state(settings, state):
    write_atomic(settings.state_file, json.dumps(state, indent=2, sort_keys=True) + "\n")


def record(settings, result):
    """Appends one JSON line of evidence; an unwritable file must not hide the outcome."""
    result["at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    try:
        with open(settings.results, "a") as handle:
            handle.write(json.dumps(result, sort_keys=True) + "\n")
    except OSError as error:
        log(f"results not recorded ({type(error).__name__})")


def installed_core_version(settings, state):
    """The release the live binaries came from, or None when they are not the ones this updater
    installed (host-setup re-pinned them, or none was ever installed)."""
    core = state["core"]
    live = settings.bin / "agentc-supervisor"
    if core.get("version") and live.is_file() and core.get("sha256") == sha256(live):
        return core["version"]
    return None


# --- release fetch and verification -------------------------------------------------------------

class HttpsOnly(urllib.request.HTTPRedirectHandler):
    """Follows redirects (GitHub serves assets from a CDN) but only to HTTPS, or loopback when
    the host allows it."""

    def __init__(self, allow_loopback):
        self.allow_loopback = allow_loopback

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        check_url(newurl, self.allow_loopback)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def check_url(url, allow_loopback):
    parts = urllib.parse.urlsplit(url)
    if parts.scheme == "https" and parts.hostname:
        return
    if allow_loopback and parts.scheme == "http" and parts.hostname in LOOPBACK_HOSTS:
        return
    raise Failed("refusing a release URL that is not HTTPS")


def http_get(settings, url, limit):
    check_url(url, settings.allow_insecure_loopback)
    opener = urllib.request.build_opener(HttpsOnly(settings.allow_insecure_loopback))
    request = urllib.request.Request(url, headers={"User-Agent": "agentc-update", "Accept": "application/octet-stream, application/vnd.github+json"})
    try:
        with opener.open(request, timeout=60) as response:
            body = response.read(limit + 1)
    except (OSError, urllib.error.URLError) as error:
        raise Failed(f"fetching {urllib.parse.urlsplit(url).path} failed ({type(error).__name__})") from None
    if len(body) > limit:
        raise Failed(f"{urllib.parse.urlsplit(url).path} exceeds {limit} bytes")
    return body


def latest_release(settings):
    """(tag, {asset name: download URL}) of the project's latest published release."""
    body = http_get(settings, f"{settings.api}/repos/{settings.repo}/releases/latest", 4 * 1024 * 1024)
    try:
        release = json.loads(body)
        tag = release["tag_name"]
        assets = {asset["name"]: asset["browser_download_url"] for asset in release["assets"]}
    except (ValueError, KeyError, TypeError):
        raise Failed("the latest release answer is malformed") from None
    if release.get("draft") or release.get("prerelease"):
        raise Failed("the latest release is a draft or prerelease")
    if version_key(tag) is None:
        raise Failed(f"release tag {tag!r} is not a version")
    return tag, assets


def target_arch():
    arch = {"x86_64": "x86_64", "AMD64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}.get(platform.machine())
    if arch is None:
        raise Failed(f"unsupported architecture {platform.machine()}")
    return arch


def bundle_name(tag):
    return f"agentc-host-{bare_version(tag)}-linux-{target_arch()}.tar.gz"


def verify_sums(sums_text, name, archive):
    """The archive must match its line in the release's SHA256SUMS asset."""
    for line in sums_text.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].lstrip("*") == name:
            if parts[0] == sha256(archive):
                return
            raise Failed(f"{name} does not match its SHA256SUMS entry")
    raise Failed(f"SHA256SUMS has no entry for {name}")


def verify_attestation(settings, archive):
    """Runs the build-attestation verifier (gh attestation verify) on the archive; a missing
    verifier or a refusal both reject the release."""
    command = [part.format(file=str(archive), repo=settings.repo) for part in shlex.split(settings.attest)]
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=300)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise Failed(f"build attestation could not be verified ({type(error).__name__})") from None
    if result.returncode != 0:
        raise Failed("the release has no valid build attestation")


def unpack(archive, destination):
    """Extracts the verified bundle into `destination` (member by member: regular files and
    directories only, bounded) and checks every file against the bundle's own SHA256SUMS."""
    destination.mkdir(mode=0o755)
    total = 0
    try:
        with tarfile.open(archive, "r:gz") as bundle:
            members = bundle.getmembers()
            if len(members) > MAX_MEMBERS:
                raise Failed("the bundle has too many members")
            if len({PurePosixPath(m.name).parts[:1] for m in members}) != 1:
                raise Failed("the bundle must have one top directory")
            for member in members:
                path = PurePosixPath(member.name)
                if path.is_absolute() or ".." in path.parts:
                    raise Failed("the bundle has an unsafe path")
                relative = Path(*path.parts[1:]) if len(path.parts) > 1 else None
                if relative is None:
                    continue
                target = destination / relative
                if member.isdir():
                    target.mkdir(mode=0o755, exist_ok=True)
                elif member.isreg():
                    total += member.size
                    if total > MAX_EXPANDED_BYTES:
                        raise Failed("the bundle expands beyond its size bound")
                    target.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
                    with bundle.extractfile(member) as source, open(target, "wb") as out:
                        shutil.copyfileobj(source, out)
                    os.chmod(target, 0o755 if member.mode & 0o111 else 0o644)
                else:
                    raise Failed("the bundle has a member that is not a file or directory")
    except (tarfile.TarError, OSError, EOFError) as error:
        raise Failed(f"the bundle cannot be unpacked ({type(error).__name__})") from None
    verify_bundle(destination)


def verify_bundle(directory):
    """Every file under `directory` is listed in its SHA256SUMS with the listed hash, and every
    listed file exists; the core binaries must be present."""
    try:
        listed = {}
        for line in (directory / "SHA256SUMS").read_text().splitlines():
            digest, name = line.split(None, 1)
            listed[name.strip().lstrip("*")] = digest
    except (OSError, ValueError):
        raise Failed("the bundle has no valid SHA256SUMS") from None
    present = {str(p.relative_to(directory)) for p in directory.rglob("*") if p.is_file() and p.name != "SHA256SUMS"}
    if present != set(listed):
        raise Failed("the bundle's files differ from its SHA256SUMS")
    for name, digest in listed.items():
        if sha256(directory / name) != digest:
            raise Failed(f"{name} does not match the bundle's SHA256SUMS")
    for name in CORE_BINARIES:
        if f"bin/{name}" not in listed:
            raise Failed(f"the bundle has no bin/{name}")


def harness_manifest(directory):
    """{harness: (file, version)} the bundle ships, from harness.json (empty without one)."""
    path = directory / "harness.json"
    if not path.is_file():
        return {}
    try:
        data = json.loads(path.read_text())
        found = {}
        for name, entry in data.items():
            if name not in HARNESSES:
                raise ValueError(name)
            file, version = entry["file"], entry["version"]
            if not version or not (directory / file).is_file() or (directory / file).is_symlink() \
                    or PurePosixPath(file).is_absolute() or ".." in PurePosixPath(file).parts:
                raise ValueError(name)
            found[name] = (directory / file, version)
        return found
    except (ValueError, KeyError, TypeError, AttributeError):
        raise Failed("the bundle's harness.json is invalid") from None


def fetch_release(settings, tag, assets):
    """Downloads, verifies and unpacks the bundle for `tag` side by side under releases/<tag>;
    returns its directory. Nothing outside releases/ changes."""
    name = bundle_name(tag)
    for needed in (name, "SHA256SUMS"):
        if needed not in assets:
            raise Failed(f"the release has no {needed} asset")
    require_protected(settings.prefix)
    settings.releases.mkdir(mode=0o755, exist_ok=True)
    require_protected(settings.releases)
    target = settings.releases / bare_version(tag)
    if target.is_symlink() or target.is_file():
        target.unlink()
    elif target.exists():
        shutil.rmtree(target)
    with tempfile.TemporaryDirectory(prefix=".fetch.", dir=settings.releases) as work:
        archive = Path(work) / name
        archive.write_bytes(http_get(settings, assets[name], MAX_DOWNLOAD_BYTES))
        sums = http_get(settings, assets["SHA256SUMS"], 1024 * 1024).decode("utf-8", "replace")
        verify_sums(sums, name, archive)
        verify_attestation(settings, archive)
        unpack(archive, Path(work) / "unpacked")
        os.replace(Path(work) / "unpacked", target)
    return target


# --- supervisor control -------------------------------------------------------------------------

def systemctl(settings, *args):
    return subprocess.run([settings.systemctl, *args], capture_output=True, text=True)


def unit_active(settings):
    return systemctl(settings, "is-active", "--quiet", UNIT).returncode == 0


def launch_running(settings):
    """Whether the active loop's heartbeat shows a launch in flight."""
    if not unit_active(settings):
        return False
    try:
        return "launch" in json.loads(settings.heartbeat.read_text())
    except (OSError, ValueError):
        return False


class Window:
    """A maintenance window: the kill switch is held and agentc-run is stopped between `open`
    and `close`. The kill switch is removed only if this window created it."""

    def __init__(self, settings, sleep=time.sleep, clock=time.monotonic):
        self.settings, self.sleep, self.clock = settings, sleep, clock
        self.created = False
        self.was_active = False

    def drain(self, force=False):
        """Holds the kill switch, waits for the running launch to end, stops agentc-run.
        Past the drain timeout it raises Deferred, or with `force` (a rollback) stops the loop
        anyway: its SIGTERM drain releases the launch's attempt."""
        s = self.settings
        self.was_active = self.was_active or unit_active(s)
        if not s.kill_switch.exists():
            s.kill_switch.write_text("")
            self.created = True
        deadline = self.clock() + s.drain_seconds
        quiet_since = None
        while True:
            if launch_running(s):
                quiet_since = None
            elif quiet_since is None:
                quiet_since = self.clock()
            elif self.clock() - quiet_since >= s.settle_seconds:
                break
            if self.clock() >= deadline:
                if force:
                    log("drain timeout during a rollback: stopping the loop anyway")
                    break
                self.release_switch()
                raise Deferred("a launch is still running past the drain timeout")
            self.sleep(min(s.poll_seconds, s.settle_seconds) if s.settle_seconds else s.poll_seconds)
        stopped = systemctl(s, "stop", UNIT)
        if stopped.returncode != 0:
            self.release_switch()
            raise Failed("agentc-run would not stop")

    def start(self):
        if systemctl(self.settings, "start", UNIT).returncode != 0:
            raise Rejected("agentc-run would not start")

    def release_switch(self):
        if self.created:
            with contextlib.suppress(FileNotFoundError):
                self.settings.kill_switch.unlink()
            self.created = False


def as_impl(settings, *command):
    return [*settings.as_impl, *command]


def preflight(settings, harnesses):
    """The supervisor's preflight as the implementer for each harness, on a fresh hardened clone
    of the mirror (the same setup containment-suite.sh uses). Raises Rejected on any problem."""
    supervisor = str(settings.bin / "agentc-supervisor")
    base = settings.state_dir / "impl"
    clone, run = str(base / "clones" / "update-check"), str(base / "runs" / "update-check")
    mirror = str(settings.state_dir / "mirror.git")
    revision = subprocess.run(["git", "-C", mirror, "rev-parse", settings.mirror_branch], capture_output=True, text=True)
    origin = subprocess.run(["git", "-C", mirror, "config", "--get", "remote.origin.url"], capture_output=True, text=True)
    if revision.returncode != 0:
        raise Rejected(f"preflight failed: the mirror has no {settings.mirror_branch}")
    clear = as_impl(settings, "rm", "-rf", clone, run)
    steps = [("clean", clear),
             ("clone", as_impl(settings, supervisor, "clone", "--url", mirror, "--revision", revision.stdout.strip(),
                               "--dest", clone, "--origin-url", origin.stdout.strip())),
             ("run directory", as_impl(settings, "mkdir", "-p", run)),
             ("prompt", as_impl(settings, "sh", "-c", 'echo agentc-update > "$1/prompt.md"', "sh", run))]
    for harness in harnesses:
        spec_args = ["--role", "implementer", "--clone", clone, "--run", run, "--harness", harness]
        steps.append((f"{harness} prepare", as_impl(settings, supervisor, "prepare", *spec_args)))
        steps.append((f"{harness} preflight", as_impl(settings, supervisor, "preflight", *spec_args)))
    try:
        for label, step in steps:
            result = subprocess.run(step, capture_output=True, text=True)
            if result.returncode != 0:
                detail = (result.stdout + result.stderr).strip().splitlines()[-1:] or ["no output"]
                raise Rejected(f"preflight failed at {label}: {detail[0][:200]}")
    finally:
        subprocess.run(clear, capture_output=True)


def run_canary(settings):
    """The end-to-end canary for each configured harness; Rejected when any fails."""
    for harness in settings.canary_harnesses:
        command = [part.format(harness=harness, prefix=str(settings.prefix), timeout=settings.canary_timeout_minutes)
                   for part in shlex.split(settings.canary)]
        result = subprocess.run(command, capture_output=True, text=True)
        if result.returncode != 0:
            tail = (result.stderr.strip().splitlines() or ["no output"])[-1][:200]
            raise Rejected(f"the {harness} canary failed (exit {result.returncode}): {tail}")


def run_suite(settings, release_dir):
    command = settings.suite or str(release_dir / "scripts" / "containment-suite.sh")
    parts = shlex.split(command)
    if not Path(parts[0]).is_file():
        raise Rejected("no containment-suite.sh to run for the staged harness")
    result = subprocess.run(parts, capture_output=True, text=True)
    if result.returncode != 0:
        failures = [line for line in result.stdout.splitlines() if line.startswith("FAIL")]
        raise Rejected(f"containment-suite.sh failed: {(failures or ['exit ' + str(result.returncode)])[0][:200]}")


# --- core and harness switching -----------------------------------------------------------------

def snapshot(settings, kind, names, extra=None):
    """Saves the live files `names` (and `extra` {name: path}) under releases/rollback-<kind>-<ts>
    and returns that directory."""
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    directory = Path(tempfile.mkdtemp(prefix=f"rollback-{kind}-{stamp}-", dir=settings.releases))
    directory.chmod(0o755)
    (directory / "bin").mkdir(mode=0o755)
    for name in names:
        live = settings.bin / name
        if live.is_file():
            shutil.copy2(live, directory / "bin" / name)
    for name, path in (extra or {}).items():
        if path.is_file():
            shutil.copy2(path, directory / name)
    return directory


def restore(settings, directory, names):
    """Puts the saved binaries `names` back into bin/ (removing a binary the snapshot lacks)."""
    for name in names:
        saved = directory / "bin" / name
        if saved.is_file():
            replace_file(saved, settings.bin / name, 0o755)
        else:
            with contextlib.suppress(FileNotFoundError):
                (settings.bin / name).unlink()


def switch_core(settings, release_dir):
    for name in CORE_BINARIES:
        replace_file(release_dir / "bin" / name, settings.bin / name, 0o755)


def update_core(settings, state, tag, release_dir, window):
    """Drains, switches, checks; on any failure rolls back and raises Rejected."""
    window.drain()
    previous = None
    try:
        try:
            previous = snapshot(settings, "core", CORE_BINARIES)
            switch_core(settings, release_dir)
            preflight(settings, settings.canary_harnesses)
            window.release_switch()
            window.start()
            run_canary(settings)
        except Rejected as error:
            reason = str(error)
        except (OSError, Failed) as error:
            reason = f"switch failed ({type(error).__name__}: {error})"
        else:
            state["core"] = {"version": bare_version(tag), "sha256": sha256(settings.bin / "agentc-supervisor"),
                             "previous_dir": str(previous), "previous_version": state["core"].get("version")}
            return
        log(f"rolling back: {reason}")
        roll_back_core(settings, previous, window)
        raise Rejected(reason)
    finally:
        window.release_switch()


def roll_back_core(settings, previous, window):
    """Drains (forced), restores the saved binaries (none when the snapshot itself failed), and
    restarts the loop."""
    try:
        window.drain(force=True)
        if previous:
            restore(settings, previous, CORE_BINARIES)
        window.release_switch()
        if window.was_active:
            window.start()
    except (OSError, Failed, Deferred, Rejected) as error:
        raise Failed(f"ROLLBACK FAILED, host needs the owner ({error})") from None


def update_harness(settings, state, tag, release_dir, window):
    """The staged harness step: new binaries and pins go in while the supervisor is drained and
    are promoted only if preflight, the canary and the containment suite pass."""
    wanted = harness_manifest(release_dir)
    changes = {name: (file, version) for name, (file, version) in wanted.items()
               if not (settings.bin / name).is_file() or sha256(settings.bin / name) != sha256(file)}
    if not changes:
        state["harness"] = {**state["harness"], "release": bare_version(tag)}
        return
    window.drain()
    previous = None
    try:
        try:
            config_before = settings.config.read_text()
            previous = snapshot(settings, "harness", list(changes), {"supervisor.toml": settings.config})
            for name, (file, _) in changes.items():
                staged = settings.releases / f".staged-{name}"
                replace_file(file, staged, 0o755)
                check_version(settings, staged, name, changes[name][1])
                os.replace(staged, settings.bin / name)
            write_atomic(settings.config, set_pins(config_before, {n: v for n, (_, v) in changes.items()}))
            preflight(settings, sorted(changes))
            run_suite(settings, release_dir)
            window.release_switch()
            window.start()
            run_canary(settings)
        except Rejected as error:
            reason = str(error)
        except (OSError, Failed) as error:
            reason = f"harness switch failed ({type(error).__name__}: {error})"
        else:
            state["harness"] = {"release": bare_version(tag), "previous_dir": str(previous)}
            return
        log(f"reverting the staged harness: {reason}")
        roll_back_harness(settings, previous, list(changes), window)
        raise Rejected(reason)
    finally:
        window.release_switch()
        for name in HARNESSES:
            with contextlib.suppress(OSError):
                (settings.releases / f".staged-{name}").unlink()


def check_version(settings, binary, name, version):
    """The staged binary must report the version it will be pinned to (preflight's own rule)."""
    result = subprocess.run(as_impl(settings, str(binary), "--version"), capture_output=True, text=True)
    if result.returncode != 0 or version not in result.stdout + result.stderr:
        raise Rejected(f"the staged {name} does not report version {version}")


def roll_back_harness(settings, previous, names, window):
    try:
        window.drain(force=True)
        if previous:
            restore(settings, previous, names)
            if (previous / "supervisor.toml").is_file():
                write_atomic(settings.config, (previous / "supervisor.toml").read_text())
        window.release_switch()
        if window.was_active:
            window.start()
    except (OSError, Failed, Deferred, Rejected) as error:
        raise Failed(f"ROLLBACK FAILED, host needs the owner ({error})") from None


def manual_rollback(settings, state, what):
    """--rollback: restores what the last update saved, by the same drain, with preflight and
    no canary (the canary is what proves a release; this goes back to one that already passed).
    The release it leaves is remembered as rejected so the timer does not reinstall it."""
    section = state[what if what == "core" else "harness"]
    previous = Path(section.get("previous_dir", ""))
    if not section.get("previous_dir") or not previous.is_dir():
        raise Failed(f"no saved {what} to roll back to")
    names = list(CORE_BINARIES) if what == "core" else [n for n in HARNESSES if (previous / "bin" / n).is_file()]
    window = Window(settings)
    window.drain(force=True)
    try:
        restore(settings, previous, names)
        if what == "harness" and (previous / "supervisor.toml").is_file():
            write_atomic(settings.config, (previous / "supervisor.toml").read_text())
        preflight(settings, settings.canary_harnesses)
        window.release_switch()
        if window.was_active:
            window.start()
    except (OSError, Rejected, Failed) as error:
        window.release_switch()
        raise Failed(f"rollback failed ({error}); the host needs the owner") from None
    left = section.get("version") if what == "core" else section.get("release")
    if what == "core":
        live = settings.bin / "agentc-supervisor"
        state["core"] = {"version": section.get("previous_version"), "sha256": sha256(live)}
        state["rejected"].append(left)
    else:
        state["harness"] = {}
        state["harness_rejected"].append(left)


def prune(settings, state):
    """Keeps the newest `keep` release directories plus those the state still points at."""
    keep_names = {Path(state[k].get("previous_dir", "")).name for k in ("core", "harness")}
    keep_names.add(state["core"].get("version") or "")
    releases = sorted((p for p in settings.releases.iterdir() if p.is_dir() and not p.name.startswith(".")
                       and not p.name.startswith("rollback-")), key=lambda p: version_key(p.name) or (), reverse=True)
    keep_names.update(p.name for p in releases[:settings.keep])
    for entry in settings.releases.iterdir():
        if entry.name not in keep_names and not entry.name.startswith(".") and entry.is_dir() and not entry.is_symlink():
            shutil.rmtree(entry, ignore_errors=True)


# --- the run ------------------------------------------------------------------------------------

def notify(settings, message):
    """One ntfy page; without a topic the message goes to the journal only."""
    log(message)
    if not settings.ntfy_topic:
        return
    request = urllib.request.Request(f"{settings.ntfy_url.rstrip('/')}/{settings.ntfy_topic}",
                                     data=message.encode(), method="POST")
    request.add_header("Title", "agentc host update")
    request.add_header("Priority", "high")
    request.add_header("Tags", "warning")
    token = os.environ.get("NTFY_TOKEN")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            response.read()
    except (OSError, urllib.error.URLError) as error:
        log(f"ntfy page not delivered ({type(error).__name__})")


def check_ready(settings):
    """Fails before anything changes when the host cannot prove a release: no running loop, or a
    canary that is not configured."""
    for path in (settings.prefix, settings.bin):
        require_protected(path)
    if not unit_active(settings):
        raise Failed("agentc-run is not active, so there is nothing to canary against; start it first")
    if settings.kill_switch.exists():
        raise Failed(f"the kill switch {settings.kill_switch} is set; clear it before an update")
    if settings.canary_is_default and not os.environ.get("E2E_PROJECT"):
        raise Failed("the end-to-end canary is not configured (E2E_PROJECT); see e2e-canary.env")


def run_update(settings, retry=False, check_only=False):
    """One update attempt; returns the result dict (outcome current, would-update, ok,
    deferred or rejected)."""
    state = load_state(settings)
    if retry:
        state["rejected"], state["harness_rejected"] = [], []
    tag, assets = latest_release(settings)
    version = bare_version(tag)
    installed = installed_core_version(settings, state)
    core_needed = version not in state["rejected"] and newer(tag, installed)
    harness_needed = (settings.harness_updates and state["harness"].get("release") != version
                      and version not in state["harness_rejected"] and (core_needed or installed == version))
    result = {"release": version, "installed": installed, "core": core_needed, "harness": harness_needed}
    if not core_needed and not harness_needed:
        return {**result, "outcome": "current", "detail": f"{installed or 'unknown'} is current for release {version}"}
    if check_only:
        return {**result, "outcome": "would-update", "detail": f"would update to {version}"}
    check_ready(settings)
    release_dir = fetch_release(settings, tag, assets)
    window = Window(settings)
    detail = []
    try:
        if core_needed:
            update_core(settings, state, tag, release_dir, window)
            save_state(settings, state)
            detail.append(f"core {installed or 'unknown'} -> {version}")
        if harness_needed:
            try:
                update_harness(settings, state, tag, release_dir, window)
            except Rejected as error:
                state["harness_rejected"].append(version)
                notify(settings, f"agentc harness update from release {version} rejected and reverted: {error}")
                detail.append(f"harness reverted ({error})")
            else:
                detail.append("harness promoted")
            save_state(settings, state)
    except Rejected as error:
        state["rejected"].append(version)
        save_state(settings, state)
        notify(settings, f"agentc host update to release {version} rejected and rolled back: {error}")
        return {**result, "outcome": "rejected", "detail": str(error)}
    except Deferred as error:
        return {**result, "outcome": "deferred", "detail": "; ".join([*detail, str(error)])}
    prune(settings, state)
    return {**result, "outcome": "ok", "detail": "; ".join(detail) or "nothing to change"}


def run(settings, args):
    settings.state_dir.mkdir(parents=True, exist_ok=True)
    with open(settings.lock, "a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            log("another update is running")
            return 0
        # The canary timers share this lock: a scheduled canary must not run, nor be judged,
        # while the host is being switched.
        with open(settings.canary_lock, "a") as canary_lock:
            try:
                fcntl.flock(canary_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                log("an end-to-end canary is running; deferring to the next timer tick")
                return 0
            return locked(settings, args)


def locked(settings, args):
    try:
        if args.rollback:
            state = load_state(settings)
            manual_rollback(settings, state, args.rollback)
            save_state(settings, state)
            result = {"outcome": "rolled-back", "detail": f"restored the previous {args.rollback}"}
        else:
            result = run_update(settings, retry=args.retry, check_only=args.check)
    except (Failed, OSError, subprocess.SubprocessError) as error:
        error = error if isinstance(error, Failed) else f"{type(error).__name__}: {error}"
        notify(settings, f"agentc host update failed: {error}")
        record(settings, {"outcome": "failed", "detail": str(error)})
        return 2
    if not args.check:
        record(settings, result)
    log(f"{result['outcome']}: {result['detail']}")
    return 1 if result["outcome"] == "rejected" else 0


def parse(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--check", action="store_true", help="report what a run would do; change nothing")
    parser.add_argument("--retry", action="store_true", help="forget rejected releases and try again")
    parser.add_argument("--rollback", choices=("core", "harness"), help="restore what the last update saved")
    parser.add_argument("--repo", default=env("UPDATE_REPO", DEFAULT_REPO))
    parser.add_argument("--api-url", default=env("UPDATE_API_URL", DEFAULT_API))
    parser.add_argument("--attest-command", default=env("UPDATE_ATTEST_COMMAND", DEFAULT_ATTEST))
    parser.add_argument("--prefix", default=env("UPDATE_PREFIX", DEFAULT_PREFIX))
    parser.add_argument("--state-dir", default=env("UPDATE_STATE_DIR", DEFAULT_STATE))
    parser.add_argument("--etc", default=env("UPDATE_ETC", DEFAULT_ETC))
    parser.add_argument("--systemctl", default=env("UPDATE_SYSTEMCTL", "systemctl"))
    parser.add_argument("--drain-timeout-minutes", type=float,
                        default=env("UPDATE_DRAIN_TIMEOUT_MINUTES", DEFAULT_DRAIN_MINUTES, float))
    parser.add_argument("--settle-seconds", type=float, default=env("UPDATE_SETTLE_SECONDS", DEFAULT_SETTLE_SECONDS, float))
    parser.add_argument("--poll-seconds", type=float, default=env("UPDATE_POLL_SECONDS", DEFAULT_POLL_SECONDS, float))
    parser.add_argument("--keep", type=int, default=env("UPDATE_KEEP", DEFAULT_KEEP, int))
    parser.add_argument("--harness-updates", type=lambda v: v not in ("0", "false", "no"),
                        default=env("UPDATE_HARNESS", True, lambda v: v not in ("0", "false", "no")))
    parser.add_argument("--as-impl", default=env(
        "UPDATE_AS_IMPL", "setpriv --reuid=agentc-impl --regid=agentc-impl --init-groups "
        "env -i PATH=/usr/bin:/bin HOME=/var/lib/agentc/impl/home"))
    parser.add_argument("--canary-command", default=env("UPDATE_CANARY_COMMAND", None))
    parser.add_argument("--canary-harnesses", default=env("E2E_HARNESSES", "claude"))
    parser.add_argument("--canary-timeout-minutes", type=float, default=env("E2E_TIMEOUT_MINUTES", 90.0, float))
    parser.add_argument("--suite-command", default=env("UPDATE_SUITE_COMMAND"))
    parser.add_argument("--mirror-branch", default=env("UPDATE_MIRROR_BRANCH", "main"))
    parser.add_argument("--ntfy-topic", default=env("UPDATE_NTFY_TOPIC", env("E2E_NTFY_TOPIC")))
    parser.add_argument("--ntfy-url", default=env("UPDATE_NTFY_URL", attention.DEFAULT_NTFY))
    parser.add_argument("--allow-insecure-loopback", action="store_true",
                        default=env("UPDATE_ALLOW_INSECURE_LOOPBACK", False, lambda v: v == "1"))
    args = parser.parse_args(argv)
    if args.drain_timeout_minutes <= 0 or args.poll_seconds <= 0 or args.settle_seconds < 0 or args.keep < 1:
        parser.error("--drain-timeout-minutes and --poll-seconds must be positive, --keep at least 1")
    return args


def main(argv=None):
    args = parse(argv)
    return run(Settings(args), args)


if __name__ == "__main__":
    sys.exit(main())
