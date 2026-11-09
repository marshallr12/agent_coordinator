#!/usr/bin/env python3
"""Disposable native Windows containment prototype; never a host admission gate.

For each candidate launch boundary the probe builds and tests a real Rust/MSVC
crate inside the boundary, then runs that crate as three concurrent launches
(two implementers and a reviewer) that attack the host and each other. The
fixture reports raw facts; this driver holds the expected value of every row,
pairs each denial with a positive control and fails closed on anything missing,
unexpected or skipped. No result can admit a host.

No provider, coordinator or publication credential is read: every protected
target is a canary created here. Only directories and one registry key created
by this run are changed, and they are removed or reported at the end.
Run with: python -I windows-containment-probe.py --output ABSOLUTE_NEW_JSON_PATH
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from typing import NamedTuple
import uuid

HERE = Path(__file__).resolve().parent


def load(name):
    """Import a sibling script by path; `python -I` keeps the script directory off sys.path."""
    spec = importlib.util.spec_from_file_location(name.replace("-", "_").removesuffix(".py"), HERE / name)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


native = load("windows-probe-native.py")

# A custom capability: the tool directory's access list names it once, so tools
# need no access entry per launch.
TOOLS_CAPABILITY = "agentcProbeTools"
# Independent candidates. The less-privileged token drops everything granted to
# "all application packages"; cargo needs `registryRead` to initialise Winsock.
VARIANTS = {
    "appcontainer": native.Boundary(lpac=False, capabilities=(TOOLS_CAPABILITY,)),
    "lpac": native.Boundary(lpac=True, capabilities=("registryRead", TOOLS_CAPABILITY)),
}
# The stricter boundary is the one proposed for production; the other is a comparison.
CANDIDATE = "lpac"
LAUNCHES = (("impl-1", "implementer"), ("impl-2", "implementer"), ("rev-1", "reviewer"))
FIXTURE_LIMITS = native.Limits(processes=24, process_memory_mb=512, job_memory_mb=1024)
BUILD_LIMITS = native.Limits(processes=48, process_memory_mb=3072, job_memory_mb=4096)
CANARY = b"synthetic, not a credential"
MSVC_COMPONENT = "Microsoft.VisualStudio.Component.VC.Tools.x86.x64"
LOW_INTEGRITY = "S-1-16-4096"
ALL_APPLICATION_PACKAGES = "S-1-15-2-1"
DENIED = ("error:5",)
# E_ACCESSDENIED, the COM result code 0x80070005, as the fixture prints it.
ACCESS_DENIED_RESULT = "error:-2147024891"
# Guarantees this prototype cannot establish; each needs a later, privileged phase.
UNVERIFIED = (
    "role service identities: every launch here runs as the invoking account",
    "privileged network policy and behaviour when the firewall service or its policy is lost",
    "native provider harness command paths and reviewer candidate interception",
    "production egress and publication brokers",
    "Service Control Manager lifecycle, crash recovery and reboot behaviour",
    "handle-relative creation and deletion by host brokers inside worker-writable trees",
    "COM, RPC and broker surfaces beyond the Task Scheduler",
)


class Check(NamedTuple):
    """One expected observation: where it applies, what it protects and what must be seen."""
    key: str
    scope: str
    resource: str
    expected: tuple
    mandatory: bool = True


# Worker-to-host attacks every launch makes: key, protected resource, accepted observations.
HOST_ATTACKS = (
    ("host.file_read", "file", DENIED), ("host.file_write", "file", DENIED),
    ("host.file_delete", "file", DENIED), ("host.dir_list", "file", DENIED),
    ("host.dir_create", "file", DENIED), ("host.profile_list", "file", DENIED),
    ("host.tool_write", "file", DENIED),
    ("host.credential_read", "credential", DENIED),
    ("host.registry_owner_read", "credential", DENIED),
    ("host.registry_owner_write", "credential", DENIED),
    ("host.registry_environment_open", "credential", DENIED),
    ("host.registry_run_write", "credential", DENIED),
    ("host.process_open_vm", "process", DENIED), ("host.process_open_query", "process", DENIED),
    ("host.process_open_terminate", "process", DENIED), ("host.process_open_dup", "process", DENIED),
    ("host.pipe_connect", "ipc", DENIED),
    ("host.event_open_local", "ipc", DENIED), ("host.event_open_global", "ipc", DENIED),
    ("host.station_open", "ipc", DENIED), ("host.window_find", "ipc", ("error:1400",)),
    ("escape.breakaway", "child_escape", DENIED),
    ("escape.scheduler_connect", "child_escape", (ACCESS_DENIED_RESULT,)),
    ("job.exceed_process_limit", "child_escape", ("error:1816",)),
    # A sandboxed token may not create junctions or symbolic links at all; then the path is absent.
    ("path.junction_read", "path", ("error:3", "error:5")),
    ("path.symlink_read", "path", ("error:2", "error:5")),
    ("path.hardlink_host", "path", DENIED), ("path.hardlink_tool", "path", DENIED),
    ("path.stream_host", "path", DENIED),
)
# Operations a launch must be able to do; a failure means a later denial proves nothing.
OWN_CONTROLS = ("own.victim_note", "own.pipe_server", "own.pipe_connect", "own.event_create",
                "own.event_open", "own.write_read", "own.tool_read", "own.child_contained",
                "own.child_open", "own.broker_route", "token.appcontainer", "token.in_job")
# Attacks on each concurrent peer: key suffix, protected resource, accepted observations.
PEER_ATTACKS = (
    ("file_read", "file", DENIED), ("file_write", "file", DENIED), ("dir_list", "file", DENIED),
    ("process_open_vm", "process", DENIED), ("process_open_query", "process", DENIED),
    ("pipe_connect", "ipc", DENIED), ("event_open", "ipc", ("error:2",)),
    ("broker_connect", "ipc", DENIED),
)
INFORMATIONAL = ("path.junction_create", "path.symlink_create", "path.planted", "desktop.clipboard_formats")


# The positive controls that make each denial evidence, by check-name prefix.
# A denial counts only when the same operation is shown to work where it is
# authorized: with the host token, or on the launch's own resource.
CONTROLS = {
    "host.file_": ("control.canary_read", "own.write_read"),
    "host.dir_": ("control.canary_read", "own.write_read"),
    "host.profile_list": ("control.canary_read", "own.write_read"),
    "host.credential_read": ("control.canary_read", "own.write_read"),
    "host.alias.": ("control.canary_read", "own.write_read"),
    "host.tool_write": ("own.tool_read",),
    "host.registry_": ("control.registry_read",),
    "host.process_": ("control.process_open", "own.child_open"),
    "host.pipe_connect": ("control.pipe_connect", "own.pipe_connect"),
    "host.event_": ("control.event_open", "own.event_open"),
    "host.station_open": ("control.shell_window", "desktop.station"),
    "host.window_find": ("control.shell_window", "desktop.station"),
    "boundary.all_packages_read": ("own.tool_read",),
    "escape.breakaway": ("own.child_contained",),
    "escape.scheduler_connect": ("control.scheduler_connect",),
    "escape.unlisted_handle": ("control.listed_handle_readable",),
    "job.descendants_reaped": ("control.orphan_alive",),
    "job.": ("own.child_open",),
    "path.": ("control.canary_read", "own.write_read"),
    "hostpath.self.": ("hostpath.self.plain_accepted",),
    "hostpath.worker.": ("hostpath.worker.plain_accepted",),
    "net.tcp.external": ("control.external_connect",),
    "net.tcp.": ("control.listener.",),
    "net.udp.": ("control.listener.",),
    "net.host_received.": ("control.listener.",),
    "net.dns": ("control.dns",),
    "peer.": ("own.victim_note", "own.pipe_connect", "own.event_open", "own.broker_route",
              "control.peer_pipe_path"),
    "broker.identity_from_os": ("own.broker_route",),
    "workload.": ("workload.cargo_test",),
}


def controls_for(key):
    """The positive controls paired with a check, chosen by its longest matching prefix."""
    prefixes = [prefix for prefix in CONTROLS if key.startswith(prefix)]
    return CONTROLS[max(prefixes, key=len)] if prefixes else ()


def relation(role, peer_role):
    """Name the isolation an attack from `role` on `peer_role` exercises."""
    if role == peer_role:
        return "same_role_cross_launch"
    return f"{role}_to_{peer_role}"


def environment_values(windows, temporary, local_appdata):
    """The whole launch environment; nothing from the invoking session is copied."""
    return {"SystemRoot": str(windows), "WINDIR": str(windows), "TEMP": str(temporary),
            "TMP": str(temporary), "PATH": str(Path(windows) / "System32"),
            "LOCALAPPDATA": str(local_appdata)}


def identity_checks(facts):
    """Facts the OS must report about the launch's own token, desktop and environment."""
    lpac = "yes" if facts["lpac"] else "no"
    exact = (("token.package_sid", facts["package_sid"]), ("token.lpac", lpac),
             ("token.integrity", LOW_INTEGRITY), ("desktop.station", facts["station"]),
             ("env.names", ",".join(sorted(facts["environment"]))),
             ("broker.reply", "sid=" + facts["package_sid"]))
    return [Check(key, "own", "control", ("value:" + value,)) for key, value in exact] + [packages_check(facts)]


def packages_check(facts):
    """A file granted only to "all application packages" separates the two token kinds.

    The ordinary token must read it, which proves the grant works; the
    less-privileged token must be denied, which proves the opt-out is real.
    """
    if facts["lpac"]:
        return Check("boundary.all_packages_read", "worker_to_host", "file", DENIED)
    return Check("boundary.all_packages_read", "own", "control", ("allowed",))


def limit_checks(limits):
    """The job must refuse processes and memory at or below its configured bounds."""
    return [Check("job.spawned_before_refusal", "worker_to_host", "child_escape",
                  (f"value:<={limits.processes - 1}",)),
            Check("job.memory_refused_at_mb", "worker_to_host", "child_escape",
                  (f"value:<={limits.process_memory_mb}",)),
            Check("job.orphan_pid", "own", "control", ("value:#",))]


# A blocked public connection is refused at once. A connection to this machine
# is dropped silently, so it times out and the host's listener log is the authority.
EGRESS_REFUSED = ("error:10013",)
EGRESS_REFUSED_OR_DROPPED = ("error:10013", "error:TimedOut")


def network_checks(facts):
    """Direct egress must be blocked; UDP is judged by the host's listener, not the sender."""
    blocked = [Check(f"net.tcp.{name}", "worker_to_host", "egress",
                     EGRESS_REFUSED if name == "external" else EGRESS_REFUSED_OR_DROPPED)
               for name in facts["tcp"]]
    datagrams = [Check(f"net.udp.{name}", "worker_to_host", "egress", (), False) for name in facts["udp"]]
    # Host not found, unrecoverable resolver failure, or no data. "Try again" is
    # transient and proves nothing.
    lookup = [Check("net.dns", "worker_to_host", "egress", ("error:11001", "error:11003", "error:11004"))]
    return blocked + datagrams + (lookup if facts["dns"] else [])


def peer_checks(role, peers):
    """Every attack on every peer, scoped by the role pair it crosses."""
    return [Check(f"peer.{name}.{suffix}", relation(role, peer_role), resource, expected)
            for name, peer_role in peers for suffix, resource, expected in PEER_ATTACKS]


def launch_checks(role, peers, facts):
    """Every observation one launch must produce, with its accepted values."""
    checks = [Check(key, "own", "control", ("ok",)) for key in OWN_CONTROLS]
    checks += identity_checks(facts)
    checks += [Check(key, "worker_to_host", resource, expected) for key, resource, expected in HOST_ATTACKS]
    checks += [Check(f"host.alias.{name}", "worker_to_host", "path", DENIED) for name in facts["aliases"]]
    checks += limit_checks(facts["limits"]) + network_checks(facts) + peer_checks(role, peers)
    return checks + [Check(key, "own", "info", (), False) for key in INFORMATIONAL]


def number(observed):
    """The integer in a `value:N` observation, or None."""
    text = observed.removeprefix("value:")
    return int(text) if observed.startswith("value:") and text.isdigit() else None


def accepts(pattern, observed):
    """Whether one accepted pattern matches an observation."""
    if pattern == "value:#":
        return number(observed) is not None
    if pattern.startswith("value:<="):
        return number(observed) is not None and number(observed) <= int(pattern.removeprefix("value:<="))
    return pattern == observed


def judged(check, observed):
    """A report row: informational checks never pass or fail, everything else must match."""
    if not check.mandatory:
        status = "info"
    else:
        status = "pass" if any(accepts(pattern, observed) for pattern in check.expected) else "fail"
    return {"check": check.key, "scope": check.scope, "resource": check.resource,
            "mandatory": check.mandatory, "expected": list(check.expected),
            "observed": observed, "status": status, "controls": list(controls_for(check.key))}


def judge(checks, observed):
    """Compare observations with expectations; a missing or unexpected row fails."""
    rows = [judged(check, observed.get(check.key, "missing")) for check in checks]
    known = {check.key for check in checks}
    extra = [Check(key, "own", "control", ("no such observation is expected",))
             for key in sorted(observed) if key not in known]
    return rows + [judged(check, observed[check.key]) for check in extra]


def parse_rows(text):
    """Parse fixture output strictly: every line is `key=value` and no key repeats."""
    observed = {}
    for line in text.splitlines():
        key, separator, value = line.partition("=")
        if not separator or not key or key in observed:
            raise ValueError("malformed or repeated fixture observation")
        observed[key] = value
    return observed


def host_row(key, resource, passed, detail, mandatory=True, scope="host_side"):
    """A row for something the host itself observed or controlled."""
    status = ("pass" if passed else "fail") if mandatory else "info"
    return {"check": key, "scope": scope, "resource": resource, "mandatory": mandatory,
            "expected": ["pass"] if mandatory else [], "observed": detail, "status": status,
            "controls": list(controls_for(key))}


def verdict(rows, evaluated=True):
    """A variant passes only when it was evaluated and no mandatory row failed."""
    blocking = sorted({f"{row.get('launch', '-')}:{row['check']}" for row in rows
                       if row["mandatory"] and row["status"] != "pass"})
    if not evaluated:
        blocking.append("-:variant was not evaluated")
    return {"gate": "pass" if rows and not blocking else "blocked", "blocking": blocking}


def summary(variant_rows, shared_rows):
    """Per-variant verdicts and the overall gate; no outcome can admit a host."""
    variants = {name: verdict(shared_rows + rows, evaluated=bool(rows))
                for name, rows in variant_rows.items()}
    candidate = variants.get(CANDIDATE, {"gate": "blocked"})
    return {"variants": variants, "candidate_boundary": CANDIDATE,
            "prototype_gate": candidate["gate"], "host_admitted": False,
            "unverified": list(UNVERIFIED)}


# --- host-side setup -------------------------------------------------------


class Host:
    """Everything the host side owns for one run."""

    def __init__(self, api, arguments):
        self.api, self.arguments = api, arguments
        self.windows = Path(os.environ.get("SystemRoot", "C:/Windows"))
        self.root = Path(tempfile.mkdtemp(prefix="agentc-windows-probe-"))
        self.identifier = uuid.uuid4().hex[:12]
        self.owner_sid = native.current_user_sid(api)
        self.session = native.session_id(api)
        self.local_appdata = Path(os.environ["LOCALAPPDATA"])
        self.private, self.logs = self.folder("host"), self.folder("logs")
        self.listeners, self.handles, self.notes, self.contained = {}, [], [], []
        self.external = None

    def folder(self, *parts):
        """Create and return a directory under the run root."""
        path = self.root.joinpath(*parts)
        path.mkdir(parents=True, exist_ok=True)
        return path

    def log(self, name):
        """A host-owned log path; launches receive only an inherited handle to it."""
        return self.logs / name


def sha256(path):
    """Hex digest of a file."""
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def newest(directory, required):
    """The highest-versioned subdirectory that contains `required`, or None."""
    candidates = [path for path in Path(directory).glob("*") if (path / required).exists()]
    return max(candidates, key=lambda path: [int(part) for part in path.name.split(".") if part.isdigit()],
               default=None)


def program_files_x86():
    """The 32-bit Program Files root; a cleared job environment may not define the variable."""
    return Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)"))


def locate_toolchain(arguments):
    """The Rust toolchain root: the explicit argument, or the one rustup has active."""
    if arguments.toolchain:
        return arguments.toolchain
    located = subprocess.run(["rustup", "which", "rustc"], capture_output=True, text=True, check=False)
    if located.returncode != 0:
        raise RuntimeError("no Rust toolchain: pass --toolchain TOOLCHAIN_ROOT")
    return Path(located.stdout.strip()).parents[1]


def locate_msvc(arguments):
    """The newest MSVC tool set of the newest Visual Studio installation."""
    if arguments.msvc:
        return arguments.msvc
    vswhere = program_files_x86() / "Microsoft Visual Studio/Installer/vswhere.exe"
    found = subprocess.run([str(vswhere), "-latest", "-products", "*", "-requires", MSVC_COMPONENT,
                            "-property", "installationPath"], capture_output=True, text=True, check=False)
    tools = newest(Path(found.stdout.strip()) / "VC/Tools/MSVC", "bin/Hostx64/x64/link.exe")
    if found.returncode != 0 or tools is None:
        raise RuntimeError("no MSVC tool set: pass --msvc MSVC_TOOLS_ROOT")
    return tools


def locate_kit(arguments):
    """The newest Windows SDK library directory."""
    if arguments.windows_kit:
        return arguments.windows_kit
    kit = newest(program_files_x86() / "Windows Kits/10/Lib", "um/x64")
    if kit is None:
        raise RuntimeError("no Windows SDK: pass --windows-kit SDK_LIB_VERSION_ROOT")
    return kit


def copy_toolchain(source, destination):
    """Copy only cargo, rustc and the MSVC target libraries, without debug symbols."""
    (destination / "bin").mkdir(parents=True)
    for item in (Path(source) / "bin").iterdir():
        if item.suffix in (".exe", ".dll") and item.name.startswith(("cargo.", "rustc.", "rustc_driver")):
            shutil.copy2(item, destination / "bin" / item.name)
    target = "lib/rustlib/x86_64-pc-windows-msvc"
    shutil.copytree(Path(source) / target, destination / target, ignore=shutil.ignore_patterns("*.pdb"))


def tool_versions(rust):
    """The versions the staged compiler and build driver report with the host token."""
    versions = {}
    for name in ("rustc", "cargo"):
        result = subprocess.run([str(rust / "bin" / f"{name}.exe"), "--version"], capture_output=True,
                                text=True, check=False)
        versions[name] = result.stdout.strip()
    return versions


def prepare_tools(host):
    """Stage a read-only tool root that launches reach through one capability."""
    source, msvc, kit = (locate_toolchain(host.arguments), locate_msvc(host.arguments),
                         locate_kit(host.arguments))
    host.rust, host.msvc, host.kit = host.root / "tools" / "rust", Path(msvc), Path(kit)
    copy_toolchain(source, host.rust)
    capability = native.sid_text(host.api, native.capability_sid(host.api, TOOLS_CAPABILITY))
    native.grant(host.windows, host.root / "tools", capability, "RX", recursive=True)
    pinned = [host.rust / "bin/cargo.exe", host.rust / "bin/rustc.exe", host.msvc / "bin/Hostx64/x64/link.exe"]
    host.tools = {"toolchain_source": str(source), "msvc": str(msvc), "windows_kit": str(kit),
                  "tools_capability_sid": capability, "versions": tool_versions(host.rust),
                  "sha256": {path.name: sha256(path) for path in pinned}}


def build_environment(host, work):
    """The launch environment plus what an offline cargo build with the MSVC linker needs."""
    values = environment_values(host.windows, work / "tmp", host.local_appdata)
    linker = host.msvc / "bin/Hostx64/x64"
    values.update({
        "CARGO_HOME": str(work / "cargo-home"), "CARGO_TARGET_DIR": str(work / "target"),
        "CARGO_BUILD_JOBS": "2", "CARGO_TERM_COLOR": "never", "RUSTC": str(host.rust / "bin/rustc.exe"),
        "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER": str(linker / "link.exe"),
        "LIB": ";".join(str(path) for path in (host.msvc / "lib/x64", host.kit / "ucrt/x64", host.kit / "um/x64")),
        "PATH": ";".join(str(path) for path in (host.rust / "bin", linker, host.windows / "System32")),
    })
    return values


def plant_canaries(host):
    """Create the host-only canaries: a file, a credential stand-in, a held handle and a registry key."""
    import winreg
    host.canary = host.private / "canary.txt"
    host.canary.write_bytes(CANARY)
    host.credential = host.private / "credential-standin.txt"
    host.credential.write_bytes(CANARY)
    inheritable = native.SecurityAttributes(native.c.sizeof(native.SecurityAttributes), None, True)
    host.held = native.require(host.api.CreateFileW(str(host.canary), native.GENERIC_READ, native.SHARE_ALL,
                                                    native.c.byref(inheritable), native.OPEN_EXISTING, 0, None),
                               "canary handle")
    host.shared = host.folder("all-packages") / "note.txt"
    host.shared.write_bytes(b"granted to all application packages only")
    native.grant(host.windows, host.shared.parent, ALL_APPLICATION_PACKAGES, "RX", recursive=True)
    host.registry_path = f"Software\\AgentcProbe-{host.identifier}"
    with winreg.CreateKey(winreg.HKEY_CURRENT_USER, host.registry_path) as key:
        winreg.SetValueEx(key, "canary", 0, winreg.REG_SZ, CANARY.decode())


def candidate_aliases(host):
    """Other spellings of the canary path that Windows resolves to the same file."""
    path = str(host.canary)
    device, short = native.c.create_unicode_buffer(512), native.c.create_unicode_buffer(1024)
    host.api.QueryDosDeviceW(path[:2], device, len(device))
    host.api.GetShortPathNameW(path, short, len(short))
    return {"upper": path.upper(), "verbatim": "\\\\?\\" + path, "device": "\\\\.\\" + path,
            "globalroot": "\\\\?\\GLOBALROOT" + device.value + path[2:], "short": short.value,
            "trailing_dot": path + ".", "default_stream": path + "::$DATA",
            "forward_slash": path.replace("\\", "/")}


def opens_canary(alias):
    """Whether the host token reads the canary through an alias."""
    try:
        with open(alias, "rb") as source:
            return source.read() == CANARY
    except (OSError, ValueError):
        return False


def usable_aliases(host):
    """Keep the aliases the host itself can open; the others prove nothing on this volume."""
    usable = {}
    for name, alias in candidate_aliases(host).items():
        if alias != str(host.canary) and opens_canary(alias):
            usable[name] = alias
        else:
            host.notes.append(f"alias {name} is not usable on this host and was not attacked")
    return usable


def host_objects(host):
    """Create the host's own pipe and events with default security."""
    host.pipe_path = f"\\\\.\\pipe\\agentc-probe-{host.identifier}-host"
    host.handles += [native.create_pipe(host.api, host.pipe_path),
                     native.create_pipe(host.api, host.pipe_path, first=False)]
    host.event = f"agentc-probe-{host.identifier}-host"
    for namespace in ("Local", "Global"):
        host.handles.append(native.require(
            host.api.CreateEventW(None, True, False, f"{namespace}\\{host.event}"), "host event"))


def local_address():
    """This machine's outbound IPv4 address, found without sending a packet."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        try:
            probe.connect(("192.0.2.1", 9))
            return probe.getsockname()[0]
        except OSError:
            return None


def start_listeners(host):
    """Listen on every local address a launch might try to reach directly."""
    wanted = [("loopback4", socket.AF_INET, socket.SOCK_STREAM, "127.0.0.1"),
              ("loopback6", socket.AF_INET6, socket.SOCK_STREAM, "::1"),
              ("lan4", socket.AF_INET, socket.SOCK_STREAM, local_address()),
              ("udp-loopback4", socket.AF_INET, socket.SOCK_DGRAM, "127.0.0.1")]
    for name, family, kind, address in wanted:
        try:
            host.listeners[name] = native.Listener(family, kind, address)
            host.listeners[name].start()
        except (OSError, TypeError):
            host.notes.append(f"listener {name} could not be created and was not attacked")


def resolve_external(host):
    """Resolve the public test name with the host token; None when the host is offline."""
    try:
        found = socket.getaddrinfo(host.arguments.external_name, 443, socket.AF_INET, socket.SOCK_STREAM)
        return found[0][4][0]
    except OSError:
        return None


# --- host positive controls ------------------------------------------------


def reaches(listener):
    """Whether a host-token client reaches a listener and the listener records it."""
    before = len(listener.received)
    try:
        with socket.socket(listener.socket.family, listener.kind) as client:
            client.settimeout(3)
            if listener.kind == socket.SOCK_STREAM:
                client.connect(listener.address)
            else:
                client.sendto(b"control", listener.address)
    except OSError:
        return False
    return wait_for(lambda: len(listener.received) > before, 3) is not None


def attempt(action):
    """Run a host-token operation; True when it succeeds and returns something truthy."""
    try:
        return bool(action())
    except OSError:
        return False


def read_registry_canary(host):
    """Read back the registry canary with the host token."""
    import winreg
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, host.registry_path) as key:
        return winreg.QueryValueEx(key, "canary")[0] == CANARY.decode()


def connects_external(host):
    """Open the public test connection with the host token."""
    with socket.create_connection((host.external, 443), timeout=5):
        return True


def opens_events(host):
    """Open both host events with the host token."""
    opened = [host.api.OpenEventW(0x00100002, False, f"{space}\\{host.event}") for space in ("Local", "Global")]
    for handle in opened:
        if handle:
            host.api.CloseHandle(handle)
    return all(opened)


def object_controls(host):
    """Host-token operations on the exact objects the launches will attack."""
    api = host.api
    return {
        "control.canary_read": lambda: host.canary.read_bytes() == CANARY,
        "control.process_open": lambda: api.CloseHandle(api.OpenProcess(0x38, False, os.getpid())),
        "control.pipe_connect": lambda: native.open_pipe(host.pipe_path).close() is None,
        "control.event_open": lambda: opens_events(host),
        "control.registry_read": lambda: read_registry_canary(host),
        "control.shell_window": lambda: api.FindWindowW("Shell_TrayWnd", None),
        "control.scheduler_connect": native.scheduler_connects,
        "control.dns": lambda: host.external is not None,
        "control.external_connect": lambda: host.external is not None and connects_external(host),
    }


def host_controls(host):
    """Show each attacked operation succeeds when authorized, so a denial is evidence."""
    rows = [host_row(key, "control", attempt(action), "host token") for key, action in object_controls(host).items()]
    for name, listener in host.listeners.items():
        rows.append(host_row(f"control.listener.{name}", "control", reaches(listener), listener.endpoint()))
        listener.received.clear()
    elevated = host_row("host.elevated", "info", False, str(bool(host.api.IsUserAnAdmin())), False)
    return rows + reader_self_test(host) + [elevated]


def refused(host, root, relative):
    """Whether the handle-based reader refuses a path."""
    try:
        native.validated_read(host.api, root, relative)
    except native.PathRefused:
        return True
    return False


def reader_self_test(host):
    """Prove the handle-based reader against real links the host plants itself."""
    area = host.folder("reader-self-test")
    (area / "single.txt").write_bytes(b"single")
    (area / "plain.txt").write_bytes(b"plain")
    os.link(area / "plain.txt", area / "second-name.txt")
    native.run_tool(host.windows, "cmd.exe", ["/d", "/c", "mklink", "/J", str(area / "redirect"), str(host.private)])
    behind = f"redirect\\{host.canary.name}"
    accepted = attempt(lambda: native.validated_read(host.api, area, "single.txt") == b"single")
    return [host_row("hostpath.self.plain_accepted", "path", accepted, "single-link file"),
            host_row("hostpath.self.hardlink_refused", "path", refused(host, area, "plain.txt"), "two names"),
            host_row("hostpath.self.junction_refused", "path", refused(host, area, "redirect"), "junction"),
            host_row("hostpath.self.parent_junction_refused", "path",
                     (area / behind).exists() and refused(host, area, behind), "file behind a junction")]


# --- contained launches ----------------------------------------------------


class Contained:
    """One prepared launch: its profile, desktop, directories, broker and process tree."""

    def __init__(self, host, variant, name, role, limits):
        self.host, self.variant, self.name, self.role = host, variant, name, role
        self.profile = native.Profile(host.api, f"agentc.probe.{host.identifier}.{variant}.{name}")
        self.sid = self.profile.sid_text
        self.control, self.work = host.folder(variant, name, "control"), host.folder(variant, name, "work")
        self.grant_directories()
        self.desktop = native.Desktop(host.api, host.owner_sid, self.sid)
        self.broker = native.Broker(host.api, f"agentc-probe-{host.identifier}-{variant}-{name}-broker",
                                    host.owner_sid, self.sid)
        self.broker.start()
        self.launch = native.Launch(host.api, self.profile, VARIANTS[variant], self.desktop.name, limits)
        self.runs, self.observed, self.orphan, self.ready, self.disposal = 0, {}, None, False, None
        host.contained.append(self)

    def grant_directories(self):
        """The launch may read its control directory and modify only its working directory."""
        (self.work / "tmp").mkdir()
        native.grant(self.host.windows, self.control, self.sid, "RX")
        native.grant(self.host.windows, self.work, self.sid, "M")
        native.label_low(self.host.windows, self.work)

    def start(self, argv, environment, passed_handles=()):
        """Start one process tree inside the boundary; its output goes to a host-owned log."""
        self.runs += 1
        self.output = self.host.log(f"{self.variant}-{self.name}-{self.runs}.txt")
        self.launch.start(argv, self.work, environment, self.output, passed_handles)

    def run(self, argv, environment, seconds, passed_handles=()):
        """Run to completion; the job is emptied afterwards whatever happened."""
        self.start(argv, environment, passed_handles)
        code = self.launch.wait(seconds)
        quiescent = self.launch.terminate()
        self.launch.close()
        return code, quiescent

    def dispose(self):
        """Stop the broker, empty the job, close the desktop and delete the profile, once."""
        if self.disposal is None:
            self.broker.stop()
            quiescent = self.launch.terminate()
            self.launch.close()
            self.desktop.close()
            self.disposal = (quiescent, self.profile.delete())
        return self.disposal


def tag(rows, variant, launch):
    """Label rows with the variant and launch they belong to."""
    for row in rows:
        row.setdefault("variant", variant)
        row.setdefault("launch", launch)
    return rows


def fixture_source():
    """The fixture crate beside this script."""
    return HERE / "windows-probe-fixture"


def source_digests():
    """Digests of every probe source file, so a report names the exact code that ran."""
    files = [HERE / "windows-containment-probe.py", HERE / "windows-probe-native.py"]
    files += sorted(path for path in fixture_source().rglob("*")
                    if path.is_file() and "target" not in path.parts)
    return {str(path.relative_to(HERE)).replace("\\", "/"): sha256(path) for path in files}


def workload_row(step, code, quiescent, output):
    """A real cargo step must exit zero, leave an empty job and, for tests, report success."""
    text = output.read_text(encoding="utf-8", errors="replace")
    passed = code == 0 and quiescent and (step != "test" or "test result: ok" in text)
    return host_row(f"workload.cargo_{step}", "workload", passed,
                    f"exit={code}; job empty={quiescent}; log={output.name}")


def export_fixture(host, builder, environment):
    """Have the builder copy its executable to a fresh single-link file.

    Cargo hard-links its output, and the host's reader refuses any file with
    more than one name, so the launch exports a plain copy for the host to read.
    """
    source = builder.work / "target" / "debug" / "fixture.exe"
    argv = [host.windows / "System32" / "cmd.exe", "/d", "/c", "copy", "/b", source, builder.work / "fixture-export.exe"]
    code, quiescent = builder.run(argv, environment, 60)
    return host_row("workload.fixture_exported", "workload", code == 0 and quiescent, f"exit={code}")


def stage_fixture(host, variant, builder, rows):
    """Copy the worker-built executable, read by handle, into a host-owned read-only directory."""
    try:
        data = native.validated_read(host.api, builder.work, "fixture-export.exe", limit=64 << 20)
    except (native.PathRefused, OSError) as error:
        rows.append(host_row("workload.fixture_staged", "workload", False, str(error)))
        return None
    destination = host.folder(variant, "bin") / "fixture.exe"
    destination.write_bytes(data)
    native.grant(host.windows, destination.parent, host.tools["tools_capability_sid"], "RX", recursive=True)
    rows.append(host_row("workload.fixture_staged", "workload", True, f"sha256={sha256(destination)}"))
    return destination


def disposal_rows(contained):
    """Rows for emptying the job and deleting the profile."""
    quiescent, deleted = contained.dispose()
    return [host_row("job.quiescent", "child_escape", quiescent, "job reported no active process"),
            host_row("cleanup.profile_deleted", "cleanup", deleted == 0, f"HRESULT={deleted}", False)]


def build(host, variant):
    """Build and test the fixture inside the boundary; returns rows and the built executable."""
    builder = Contained(host, variant, "builder", "builder", BUILD_LIMITS)
    crate = builder.work / "crate"
    shutil.copytree(fixture_source(), crate, ignore=shutil.ignore_patterns("target"))
    (builder.work / "cargo-home").mkdir()
    environment = build_environment(host, builder.work)
    cargo, rows, executable = host.rust / "bin/cargo.exe", [], None
    try:
        for step in ("test", "build"):
            argv = [cargo, step, "--offline", "--locked", "--manifest-path", crate / "Cargo.toml"]
            code, quiescent = builder.run(argv, environment, 900)
            rows.append(workload_row(step, code, quiescent, builder.output))
        rows.append(export_fixture(host, builder, environment))
        executable = stage_fixture(host, variant, builder, rows)
    finally:
        rows += disposal_rows(builder)
    return tag(rows, variant, "builder"), executable


def wait_for(predicate, seconds):
    """Poll until `predicate` returns something truthy or the time runs out."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    return None


def read_worker_file(host, contained, name):
    """A worker-written file read by handle; None while absent or refused."""
    try:
        return native.validated_read(host.api, contained.work, name)
    except (native.PathRefused, OSError):
        return None


def await_worker_file(host, contained, name, seconds):
    """Wait for a worker-written file; None when the launch exits or time runs out without it."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        data = read_worker_file(host, contained, name)
        if data is not None:
            return data
        if contained.launch.wait(0) is not None:
            return read_worker_file(host, contained, name)
        time.sleep(0.05)
    return None


def network_targets(host):
    """Every direct network target, named as the expectation table names it."""
    targets = {}
    for name, listener in host.listeners.items():
        kind = "udp" if name.startswith("udp-") else "tcp"
        targets[f"{kind}.{name.removeprefix('udp-')}"] = listener.endpoint()
    if host.external:
        targets["tcp.external"] = f"{host.external}:443"
        targets["dns_name"] = host.arguments.external_name
    return targets


def peer_pipe_path(host, peer):
    """The full path of a peer's pipe inside its package's object directory."""
    return (f"\\\\.\\pipe\\Sessions\\{host.session}\\AppContainerNamedObjects\\{peer.sid}"
            f"\\agentc-victim-{peer.name}")


def peer_targets(host, peer):
    """What an attacker needs to know about one peer; every value is assumed public."""
    prefix = f"peer.{peer.name}"
    return {f"{prefix}.dir": peer.work, f"{prefix}.pid": peer.launch.pid,
            f"{prefix}.pipe": peer_pipe_path(host, peer),
            f"{prefix}.event": f"agentc-victim-{peer.name}-event",
            f"{prefix}.broker_pipe": peer.broker.path}


def host_targets(host, contained, peers):
    """The host objects, owner registry keys and limits one launch attacks."""
    owner = host.owner_sid
    return {
        "package_sid": contained.sid, "host_dir": host.private, "host_file": host.canary,
        "host_credential": host.credential, "owner_profile": Path.home(),
        "tool_file": host.rust / "bin/rustc.exe", "all_packages_file": host.shared,
        "host_pid": os.getpid(),
        "process_limit": FIXTURE_LIMITS.processes, "memory_limit_mb": FIXTURE_LIMITS.process_memory_mb,
        "forged_launch": peers[0].name, "broker_pipe": contained.broker.path, "host_pipe": host.pipe_path,
        "host_event": host.event, "owner_hive_key": f"{owner}\\{host.registry_path}",
        "owner_environment_key": f"{owner}\\Environment",
        "owner_run_key": f"{owner}\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
    }


def spec_lines(host, contained, peers, aliases):
    """The targets one launch attacks, as `key=value` lines."""
    values = host_targets(host, contained, peers)
    values.update({f"alias.{name}": alias for name, alias in aliases.items()})
    values.update(network_targets(host))
    for peer in peers:
        values.update(peer_targets(host, peer))
    return "".join(f"{key}={value}\n" for key, value in values.items())


def launch_facts(host, contained, aliases):
    """What the expectation table needs to know about one launch."""
    targets = network_targets(host)
    environment = environment_values(host.windows, contained.work / "tmp", host.local_appdata)
    return {"package_sid": contained.sid, "lpac": VARIANTS[contained.variant].lpac,
            "station": contained.desktop.name, "environment": environment, "aliases": aliases,
            "tcp": [key.removeprefix("tcp.") for key in targets if key.startswith("tcp.")],
            "udp": [key.removeprefix("udp.") for key in targets if key.startswith("udp.")],
            "dns": "dns_name" in targets, "limits": FIXTURE_LIMITS}


def start_fixtures(host, variant, executable):
    """Start every launch and wait until each reports its own resources are ready."""
    group = [Contained(host, variant, name, role, FIXTURE_LIMITS) for name, role in LAUNCHES]
    for contained in group:
        environment = environment_values(host.windows, contained.work / "tmp", host.local_appdata)
        contained.start([executable, "launch", contained.name, contained.control, contained.work], environment)
    for contained in group:
        ready = await_worker_file(host, contained, "ready", 60) or b""
        contained.ready = ready.decode("ascii", "replace") == str(contained.launch.pid)
    return group


def fixture_rows(host, contained, group, aliases, text):
    """Judge one launch's observations; unreadable or malformed output yields only failures."""
    peers = [(peer.name, peer.role) for peer in group if peer is not contained]
    checks = launch_checks(contained.role, peers, launch_facts(host, contained, aliases))
    try:
        contained.observed = parse_rows(text.decode("utf-8")) if text is not None else {}
    except (ValueError, UnicodeDecodeError):
        contained.observed = {}
    ready = host_row("launch.ready", "control", contained.ready, "ready file names the launched process")
    return [ready] + judge(checks, contained.observed)


def observe(host, group, aliases):
    """Release every launch at once and collect each one's rows."""
    for contained in group:
        peers = [peer for peer in group if peer is not contained]
        (contained.control / "spec.txt").write_text(spec_lines(host, contained, peers, aliases), encoding="utf-8")
    for contained in group:
        (contained.control / "go").write_bytes(b"")
    rows = []
    for contained in group:
        text = await_worker_file(host, contained, "result.txt", 240)
        rows += tag(fixture_rows(host, contained, group, aliases, text), contained.variant, contained.name)
    return rows


def planted_rows(host, contained):
    """The handle-based reader must accept the plain file and refuse each planted link."""
    planted = contained.observed.get("path.planted", "value:").removeprefix("value:").split(",")
    plain = read_worker_file(host, contained, "collect\\plain.txt") == b"plain"
    rows = [host_row("hostpath.worker.plain_accepted", "path", plain, "plain worker file read by handle")]
    if "twins" in planted:
        rows.append(host_row("hostpath.worker.hardlink_refused", "path",
                             refused(host, contained.work, "collect\\twin-a.txt"), "worker hard link"))
    if "redirect" in planted:
        rows.append(host_row("hostpath.worker.junction_refused", "path",
                             refused(host, contained.work, f"collect\\redirect\\{host.canary.name}"),
                             "worker junction to the host directory"))
    return rows


def broker_identified(contained):
    """The broker saw one request and took the package SID from the token, not the claim."""
    exchanges = contained.broker.exchanges
    return (len(exchanges) == 1 and exchanges[0]["os_package_sid"] == contained.sid
            and exchanges[0]["claimed"].startswith("launch=")
            and exchanges[0]["claimed"] != f"launch={contained.name}")


def host_observations(host, contained):
    """What the host itself saw of one launch: broker identity, planted links and its pipe."""
    reachable = attempt(lambda: native.open_pipe(peer_pipe_path(host, contained)).close() is None)
    rows = [host_row("broker.identity_from_os", "ipc", broker_identified(contained),
                     "one request; package SID read from the client token while it claimed another launch"),
            host_row("control.peer_pipe_path", "control", reachable,
                     "host token reaches the pipe by the path peers attack")]
    return tag(rows + planted_rows(host, contained), contained.variant, contained.name)


def hold_orphan(host, contained):
    """Open the launch's deliberate orphan so its death is observed by handle, not by PID."""
    pid = number(contained.observed.get("job.orphan_pid", ""))
    contained.orphan = host.api.OpenProcess(0x00100000, False, pid) if pid else None
    alive = bool(contained.orphan) and host.api.WaitForSingleObject(contained.orphan, 0) == 258
    row = host_row("control.orphan_alive", "control", alive, "descendant still running before the job ends")
    return tag([row], contained.variant, contained.name)


def finish(host, contained):
    """Stop one launch, empty its job and show the orphan died with it."""
    (contained.control / "stop").write_bytes(b"")
    code = contained.launch.wait(30) if contained.launch.process else None
    rows = [host_row("launch.exit", "control", code == 0, f"exit={code}")] + disposal_rows(contained)
    reaped = bool(contained.orphan) and host.api.WaitForSingleObject(contained.orphan, 10000) == 0
    if contained.orphan:
        host.api.CloseHandle(contained.orphan)
    rows.append(host_row("job.descendants_reaped", "child_escape", reaped, "orphan ended with the job"))
    return tag(rows, contained.variant, contained.name)


def received_rows(host, variant):
    """No direct connection or datagram may have reached any host listener."""
    rows = []
    for name, listener in host.listeners.items():
        rows.append(host_row(f"net.host_received.{name}", "egress", not listener.received,
                             f"{len(listener.received)} arrivals at {listener.endpoint()}"))
        listener.received.clear()
    return tag(rows, variant, "-")


# Exit statuses that mean a handle value was not usable: the read failed, or
# strict handle checks ended the process with STATUS_INVALID_HANDLE.
HANDLE_UNUSABLE = (6, 0xC0000008)


def handle_rows(host, variant, executable):
    """A launch may use only the handles the host lists for it.

    The host holds an inheritable handle to the canary. A first process that is
    given that handle reads the canary through it; one that is not must fail.
    """
    probe = Contained(host, variant, "handles", "implementer", FIXTURE_LIMITS)
    environment = environment_values(host.windows, probe.work / "tmp", host.local_appdata)
    argv = [executable, "read-handle", str(host.held)]
    try:
        # Both runs share the host's handle, so each starts reading at the beginning.
        host.api.SetFilePointerEx(host.held, 0, None, 0)
        passed, _ = probe.run(argv, environment, 30, passed_handles=(host.held,))
        host.api.SetFilePointerEx(host.held, 0, None, 0)
        withheld, _ = probe.run(argv, environment, 30)
    finally:
        rows = disposal_rows(probe)
    rows += [host_row("control.listed_handle_readable", "control", passed == 0, f"exit={passed}"),
             host_row("escape.unlisted_handle", "child_escape", withheld in HANDLE_UNUSABLE, f"exit={withheld}")]
    return tag(rows, variant, "handles")


def evaluate(host, variant, aliases):
    """Run the whole matrix for one boundary variant."""
    rows, executable = build(host, variant)
    if executable is None:
        return rows
    rows += handle_rows(host, variant, executable)
    group = start_fixtures(host, variant, executable)
    try:
        rows += observe(host, group, aliases)
        for contained in group:
            rows += host_observations(host, contained) + hold_orphan(host, contained)
    finally:
        for contained in group:
            rows += finish(host, contained)
    return rows + received_rows(host, variant)


# --- report ----------------------------------------------------------------


def windows_build():
    """The exact Windows product, edition, build and update revision."""
    import winreg
    names = ("ProductName", "EditionID", "DisplayVersion", "CurrentBuild", "UBR")
    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") as key:
        return {name: str(winreg.QueryValueEx(key, name)[0]) for name in names}


def prepare(host):
    """Create every host-side resource the launches will attack."""
    prepare_tools(host)
    plant_canaries(host)
    host_objects(host)
    start_listeners(host)
    host.external = resolve_external(host)


def remove_registry_canary(host):
    """Delete the disposable registry key; False when it could not be removed."""
    import winreg
    try:
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, host.registry_path)
    except (OSError, AttributeError):
        return False
    return True


def cleanup(host):
    """Dispose of any launch an error left behind, stop listeners, release handles and
    remove the registry canary and the staged tools.

    The run directory, its logs and every launch directory stay for inspection.
    """
    for contained in host.contained:
        contained.dispose()
    for listener in host.listeners.values():
        listener.close()
    for handle in host.handles + [getattr(host, "held", None)]:
        if handle:
            host.api.CloseHandle(handle)
    shutil.rmtree(host.root / "tools", ignore_errors=True)
    return [host_row("cleanup.registry_canary_removed", "cleanup", remove_registry_canary(host),
                     "disposable key", False)]


def run(host):
    """Execute every variant; returns shared rows and rows per variant."""
    shared, variants = [], {name: [] for name in VARIANTS}
    try:
        prepare(host)
        shared += host_controls(host)
        aliases = usable_aliases(host)
        for variant in VARIANTS:
            variants[variant] = evaluate(host, variant, aliases)
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        shared.append(host_row("setup", "control", False, f"{type(error).__name__}: {error}"))
    finally:
        shared += cleanup(host)
    return tag(shared, "-", "-"), variants


def report(host, shared, variants):
    """The whole evidence record for one run."""
    rows = shared + [row for rows in variants.values() for row in rows]
    machine = {"platform": platform.platform(), "windows": windows_build(), "python": platform.python_version(),
               "elevated": bool(host.api.IsUserAnAdmin()), "session": host.session}
    boundaries = {name: {"lpac": boundary.lpac, "capabilities": list(boundary.capabilities)}
                  for name, boundary in VARIANTS.items()}
    return {"schema_version": 2, "recorded_at_unix": time.time(),
            "scope": "disposable prototype; no service, role identity, provider harness or host admission",
            "host": machine, "work_directory": str(host.root), "tools": getattr(host, "tools", {}),
            "source_sha256": source_digests(), "notes": host.notes, "boundaries": boundaries,
            **summary(variants, shared), "checks": rows}


def arguments():
    """Parse the command line."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="absolute path of a new JSON report")
    parser.add_argument("--toolchain", type=Path, help="Rust toolchain root (default: rustup's active one)")
    parser.add_argument("--msvc", type=Path, help="MSVC tools root, the directory holding bin and lib")
    parser.add_argument("--windows-kit", type=Path, help="Windows SDK Lib version directory")
    parser.add_argument("--external-name", default="example.com", help="public name for the egress probes")
    parsed = parser.parse_args()
    if sys.platform != "win32" or not parsed.output.is_absolute():
        parser.error("requires native Windows and an absolute, new output path")
    return parsed


def main():
    """Run the prototype and write its report; the exit status is zero only when the gate passes."""
    parsed = arguments()
    # Exclusive creation before any side effect: existing evidence is never overwritten.
    with parsed.output.open("x", encoding="utf-8") as output:
        host = Host(native.Api(), parsed)
        shared, variants = run(host)
        record = report(host, shared, variants)
        json.dump(record, output, indent=2)
        output.write("\n")
    print(json.dumps({key: record[key] for key in ("variants", "prototype_gate", "host_admitted")}, indent=2))
    return 0 if record["prototype_gate"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
