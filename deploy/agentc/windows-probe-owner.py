#!/usr/bin/env python3
"""Role-account mode for windows-containment-probe.py.

The single-account prototype launches as the invoking account, so the protected
owner and the launch identity are one account. This module separates them.

1. The owner runs the probe with `--owner-plant`. It plants the owner's
   canaries, stages the interpreter, the probe source and a Rust toolchain where
   another account can read them, writes a manifest and waits.
2. A separate role account runs the staged probe with `--owner-manifest`. Every
   launch then runs as that account and attacks the owner's targets.
3. The waiting owner step checks that its canaries are untouched, adds those
   rows and writes the final report.

The module imports everywhere; nothing touches Windows until a step runs. The
driver passes itself in as `probe`, which avoids an import cycle.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

HERE = Path(__file__).resolve().parent
# Owner-token operations that make a denied owner row evidence.
OWNER_CONTROLS = ("file_read", "registry_read", "process_open", "pipe_connect", "event_open")
MANIFEST_KEYS = ("schema_version", "owner_sid", "profile", "directory", "file", "credential",
                 "registry_path", "pid", "pipe", "event", "toolchain", "msvc", "windows_kit", "controls")
# Probe files another account needs, relative to this directory.
STAGED_SOURCES = ("windows-containment-probe.py", "windows-probe-native.py", "windows-probe-owner.py")
# Parts of a Python installation the probe never imports.
UNUSED_PYTHON = ("Doc", "tcl", "test", "site-packages", "idlelib", "tkinter", "ensurepip", "__pycache__")


def load_manifest(path):
    """Read an owner manifest, refusing one that lacks a key or names an unknown control."""
    manifest = json.loads(Path(path).read_text(encoding="utf-8"))
    missing = [key for key in MANIFEST_KEYS if key not in manifest]
    unknown = [name for name in manifest.get("controls", {}) if name not in OWNER_CONTROLS]
    if missing or unknown or manifest.get("schema_version") != 1:
        raise ValueError(f"unusable owner manifest: missing {missing}, unknown controls {unknown}")
    return manifest


def control_rows(probe, manifest):
    """One row per owner control; a control the owner did not record as passed fails."""
    rows = []
    for name in OWNER_CONTROLS:
        passed = manifest["controls"].get(name) is True
        key = "control.registry_read" if name == "registry_read" else f"control.owner.{name}"
        rows.append(probe.host_row(key, "control", passed, "owner token, recorded in the manifest"))
    return rows


def identity_row(probe, owner_sid, identity_sid):
    """Role-identity evidence needs a launch identity that is not the owner.

    A run in which both are one account still exercises every step, but it
    proves nothing about a role account, so it blocks the gate.
    """
    distinct = owner_sid != identity_sid
    detail = "launch identity differs from the owner" if distinct else \
        "the launch identity is the owner: plumbing evidence only, not role-identity evidence"
    return probe.host_row("identity.role_account_distinct", "control", distinct, detail)


def spec_values(manifest, identity_sid):
    """The owner's targets, as the fixture's spec names them."""
    return {"owner_dir": manifest["directory"], "owner_file": manifest["file"],
            "owner_credential": manifest["credential"], "owner_pid": manifest["pid"],
            "owner_pipe": manifest["pipe"], "owner_event": manifest["event"],
            "identity_environment_key": f"{identity_sid}\\Environment"}


# --- the owner's step --------------------------------------------------------


def plant_targets(probe, api, identifier):
    """Create the owner's canaries; returns their description and the handles keeping them alive."""
    import winreg
    native = probe.native
    directory = Path(tempfile.mkdtemp(prefix="agentc-owner-"))
    for name in ("canary.txt", "credential-standin.txt"):
        (directory / name).write_bytes(probe.CANARY)
    registry_path = f"Software\\AgentcProbe-{identifier}"
    with winreg.CreateKey(winreg.HKEY_CURRENT_USER, registry_path) as key:
        winreg.SetValueEx(key, "canary", 0, winreg.REG_SZ, probe.CANARY.decode())
    pipe, event = f"\\\\.\\pipe\\agentc-probe-{identifier}-owner", f"agentc-probe-{identifier}-owner"
    handles = [native.create_pipe(api, pipe), native.create_pipe(api, pipe, first=False)]
    handles += [native.require(api.CreateEventW(None, True, False, f"{space}\\{event}"), "owner event")
                for space in ("Local", "Global")]
    targets = {"directory": str(directory), "file": str(directory / "canary.txt"),
               "credential": str(directory / "credential-standin.txt"), "registry_path": registry_path,
               "pid": os.getpid(), "pipe": pipe, "event": event}
    return targets, handles


def read_registry(probe, registry_path):
    """Whether the owner's registry canary still holds its value."""
    import winreg
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, registry_path) as key:
        return winreg.QueryValueEx(key, "canary")[0] == probe.CANARY.decode()


def opens_events(api, event):
    """Open both owner events with the owner token."""
    opened = [api.OpenEventW(0x00100002, False, f"{space}\\{event}") for space in ("Local", "Global")]
    for handle in opened:
        if handle:
            api.CloseHandle(handle)
    return all(opened)


def evaluate_controls(probe, api, targets):
    """Run each owner control with the owner token."""
    native = probe.native
    actions = {
        "file_read": lambda: Path(targets["file"]).read_bytes() == probe.CANARY,
        "registry_read": lambda: read_registry(probe, targets["registry_path"]),
        "process_open": lambda: api.CloseHandle(api.OpenProcess(0x38, False, os.getpid())),
        "pipe_connect": lambda: native.open_pipe(targets["pipe"]).close() is None,
        "event_open": lambda: opens_events(api, targets["event"]),
    }
    return {name: probe.attempt(action) for name, action in actions.items()}


def copy_python(destination):
    """Stage the running interpreter without the parts the probe never imports."""
    shutil.copytree(sys.base_prefix, destination, ignore=shutil.ignore_patterns(*UNUSED_PYTHON))
    return destination / "python.exe"


def copy_sources(destination):
    """Stage the probe scripts and the fixture crate."""
    destination.mkdir(parents=True)
    for name in STAGED_SOURCES:
        shutil.copy2(HERE / name, destination / name)
    fixture = "windows-probe-fixture"
    shutil.copytree(HERE / fixture, destination / fixture, ignore=shutil.ignore_patterns("target"))


def stage(probe, staging, arguments):
    """Stage everything the role account needs; returns the tool paths for the manifest."""
    staging.mkdir(parents=True)
    copy_python(staging / "python")
    copy_sources(staging / "source")
    probe.copy_toolchain(probe.locate_toolchain(arguments), staging / "toolchain")
    (staging / "role").mkdir()
    return {"toolchain": str(staging / "toolchain"), "msvc": str(probe.locate_msvc(arguments)),
            "windows_kit": str(probe.locate_kit(arguments))}


def grant_role(probe, windows, staging, account):
    """Let the role account read the staged tree and modify only its own report directory."""
    native = probe.native
    for path, rights in ((staging, "RX"), (staging / "role", "M")):
        arguments = [str(path), "/grant", f"{account}:(OI)(CI)({rights})", "/T", "/Q"]
        if native.run_tool(windows, "icacls.exe", arguments) != 0:
            raise RuntimeError(f"could not grant {account} {rights} on {path}")


def role_command(staging):
    """The command the role account runs: the staged interpreter on the staged probe."""
    return [str(staging / "python" / "python.exe"), "-I", "-B",
            str(staging / "source" / "windows-containment-probe.py"),
            "--owner-manifest", str(staging / "manifest.json"),
            "--output", str(staging / "role" / "report.json")]


def await_role(staging, child, seconds):
    """Wait for the role run's completion marker; a spawned run that exits also ends the wait."""
    marker, deadline = staging / "role" / "done", time.monotonic() + seconds
    while time.monotonic() < deadline:
        if marker.exists():
            return True
        if child is not None and child.poll() is not None:
            return marker.exists()
        time.sleep(0.5)
    return False


def integrity_rows(probe, targets):
    """After the role run, the owner's canaries must be exactly as planted."""
    directory = Path(targets["directory"])
    files = all(Path(targets[name]).read_bytes() == probe.CANARY for name in ("file", "credential"))
    listing = sorted(path.name for path in directory.iterdir()) == ["canary.txt", "credential-standin.txt"]
    registry = probe.attempt(lambda: read_registry(probe, targets["registry_path"]))
    rows = [probe.host_row("owner.integrity.files_unchanged", "file", files, "owner token after the run"),
            probe.host_row("owner.integrity.directory_unchanged", "file", listing, "no file added or removed"),
            probe.host_row("owner.integrity.registry_unchanged", "credential", registry, "owner token after the run")]
    return probe.tag(rows, "-", "-")


def finish(probe, api, staging, targets, finished):
    """Read the role run's report by handle, add the owner's rows and recompute the verdict."""
    rows = integrity_rows(probe, targets)
    if not finished:
        rows += probe.tag([probe.host_row("owner.role_run_completed", "control", False, "no completion marker")],
                          "-", "-")
        record = {"schema_version": 2, "checks": []}
    else:
        data = probe.native.validated_read(api, staging / "role", "report.json", limit=16 << 20)
        record = json.loads(data.decode("utf-8"))
    record["checks"] = record.get("checks", []) + rows
    record.update(probe.resummarize(record["checks"]))
    record["owner_step"] = {"staging": str(staging), "role_run_completed": finished}
    return record


def cleanup(api, targets, handles):
    """Remove the owner's canaries; the staging directory stays for inspection."""
    import winreg
    for handle in handles:
        api.CloseHandle(handle)
    shutil.rmtree(targets["directory"], ignore_errors=True)
    try:
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, targets["registry_path"])
    except OSError:
        pass


def write_manifest(probe, api, staging, targets, tools):
    """Record the owner's identity, targets, tools and controls for the role run."""
    manifest = {"schema_version": 1, "owner_sid": probe.native.current_user_sid(api),
                "profile": str(Path.home()), **targets, **tools,
                "controls": evaluate_controls(probe, api, targets)}
    (staging / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return manifest


def start_role_run(arguments, staging):
    """Print the role account's command, or start it as a child for a same-account dry run."""
    command = role_command(staging)
    if arguments.same_account_dry_run:
        return subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL)
    print("Run this as the role account, then return here:\n  " + subprocess.list2cmdline(command), flush=True)
    return None


def run(probe, arguments):
    """The owner's step: plant, stage, wait for the role run, then write the final report."""
    api, staging = probe.native.Api(), arguments.staging
    windows = Path(os.environ.get("SystemRoot", "C:/Windows"))
    # Without a named role account, grant this account by SID: a same-account dry run.
    account = arguments.role_account or "*" + probe.native.current_user_sid(api)
    with arguments.output.open("x", encoding="utf-8") as output:
        targets, handles = plant_targets(probe, api, uuid.uuid4().hex[:12])
        try:
            tools = stage(probe, staging, arguments)
            write_manifest(probe, api, staging, targets, tools)
            grant_role(probe, windows, staging, account)
            child = start_role_run(arguments, staging)
            record = finish(probe, api, staging, targets, await_role(staging, child, arguments.role_wait))
        finally:
            cleanup(api, targets, handles)
        json.dump(record, output, indent=2)
        output.write("\n")
    print(json.dumps({key: record[key] for key in ("variants", "prototype_gate", "host_admitted")}, indent=2))
    return 0 if record["prototype_gate"] == "pass" else 1
