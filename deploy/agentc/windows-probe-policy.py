#!/usr/bin/env python3
"""Network policy loss test for windows-containment-probe.py.

A launch with no network capability is refused every direct connection. Windows
itself enforces that. This module shows whether the refusal still holds when
the Windows Firewall policy is taken away. It adds network rounds to
role-account mode and has three parts.

1. The owner's `--owner-plant` step lists the firewall conditions in its
   manifest.
2. After the normal matrix, the role run repeats the direct network attempts
   once per condition, from a fresh launch inside the proposed boundary.
3. This file, run elevated by the owner, applies each condition only while its
   round runs, records the state it reached and restores the original state.

The two sides meet through small files in the staging directory. The role run
writes `role/policy/CONDITION.ready` and `.done`; the owner's step writes
`policy/CONDITION.json`, `policy/original.json` and `policy/restored.json`.

Nothing here installs a rule or a service. The module imports everywhere; only
a step that runs touches Windows.
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import subprocess
import sys
import time

BASELINE, PROFILES_OFF, SERVICE_STOP = "baseline", "profiles-off", "service-stop"
CONDITIONS = (BASELINE, PROFILES_OFF, SERVICE_STOP)
# Windows may refuse an administrator this condition; that is recorded, not failed.
MAY_BE_REFUSED = (SERVICE_STOP,)
PROFILES = ("Domain", "Private", "Public")
FIREWALL_SERVICE = "MpsSvc"
# Seconds the role run waits for the owner's step to apply one condition.
ROLE_WAIT = 600
STATE_QUERY = ("$p = Get-NetFirewallProfile | ForEach-Object { @{ name = $_.Name; enabled = [string]$_.Enabled } }; "
               "@{ profiles = @($p); service = [string](Get-Service " + FIREWALL_SERVICE + ").Status } "
               "| ConvertTo-Json -Compress -Depth 4")


# --- what both sides share -------------------------------------------------


def parse_conditions(text):
    """The ordered conditions of a run: known names, no repeats, the untouched state first."""
    names = [name.strip() for name in text.split(",") if name.strip()]
    unknown = [name for name in names if name not in CONDITIONS]
    if unknown or len(set(names)) != len(names) or names[:1] != [BASELINE]:
        raise ValueError(f"conditions must start with {BASELINE}, not repeat, and come from {CONDITIONS}")
    return names


def owner_directory(staging):
    """Where the owner's elevated step writes; the role account can only read it."""
    return Path(staging) / "policy"


def role_directory(staging):
    """Where the role run writes its markers."""
    return Path(staging) / "role" / "policy"


def marker(staging, condition, kind):
    """A role-written marker: `ready` asks for a condition, `done` ends its round."""
    return role_directory(staging) / f"{condition}.{kind}"


def record_file(staging, condition):
    """The owner's record of what it did for one condition."""
    return owner_directory(staging) / f"{condition}.json"


def parse_state(text):
    """Firewall state from the query's JSON: each profile on or off, and the service status."""
    try:
        data = json.loads(text)
        profiles = {entry["name"]: entry["enabled"] == "True" for entry in data["profiles"]}
        service = str(data["service"])
    except (ValueError, KeyError, TypeError):
        return None
    return {"profiles": profiles, "service": service} if set(profiles) == set(PROFILES) else None


def reached(condition, state):
    """Whether a firewall state is the one a condition asks for."""
    if state is None:
        return False
    enabled = list(state["profiles"].values())
    if condition == BASELINE:
        return all(enabled) and state["service"] == "Running"
    if condition == PROFILES_OFF:
        return not any(enabled)
    return state["service"] == "Stopped"


def run_powershell(windows, command, timeout=120):
    """Run one Windows PowerShell command; returns (exit code, output, first error line)."""
    program = Path(windows) / "System32" / "WindowsPowerShell" / "v1.0" / "powershell.exe"
    result = subprocess.run([str(program), "-NoProfile", "-NonInteractive", "-Command", command],
                            stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=timeout, check=False)
    error = (result.stderr.strip().splitlines() or [""])[0]
    return result.returncode, result.stdout.strip(), error


def firewall_state(windows):
    """Read the current firewall state; None when it cannot be read. Needs no elevation."""
    try:
        code, output, _ = run_powershell(windows, STATE_QUERY)
    except (OSError, subprocess.SubprocessError):
        return None
    return parse_state(output) if code == 0 else None


def wait_for_file(path, seconds):
    """Poll until a file exists or the time runs out."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if path.exists():
            return True
        time.sleep(0.2)
    return path.exists()


def write_json(path, value):
    """Write a JSON file under a temporary name, then rename, so a reader never sees half."""
    partial = path.with_name(path.name + ".partial")
    partial.write_text(json.dumps(value, indent=2, sort_keys=True), encoding="utf-8")
    os.replace(partial, path)


def read_json(path):
    """A JSON object from a file; None when it is absent or unusable."""
    try:
        value = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return value if isinstance(value, dict) else None


# --- the role run's rounds -------------------------------------------------


def condition_rows(probe, condition, record, observed):
    """Whether the condition was really in force for the round.

    The owner's record says what it did. The role run's own reading of the
    firewall state is the judge, so a record alone proves nothing.
    """
    outcome = record.get("outcome", "missing") if record else "missing"
    detail = f"{outcome}: {record.get('detail', '')}" if record else "the owner's elevated step wrote no record"
    refused = condition in MAY_BE_REFUSED and outcome == "refused"
    rows = [probe.host_row("policy.condition_applied", "egress", outcome == "applied", detail, mandatory=not refused)]
    if not refused:
        rows.append(probe.host_row("policy.state_observed", "egress", reached(condition, observed),
                                   json.dumps(observed, sort_keys=True)))
    return rows


def control_rows(probe, host):
    """Host-token network controls taken in this firewall state, so a denial is evidence."""
    rows = [probe.host_row(f"policy.control.listener.{name}", "control", probe.reaches(listener),
                           "host token, this firewall state") for name, listener in host.listeners.items()]
    for listener in host.listeners.values():
        listener.received.clear()
    resolved = probe.resolve_external(host) is not None
    connected = probe.attempt(lambda: host.external is not None and probe.connects_external(host))
    return rows + [probe.host_row("policy.control.dns", "control", resolved, "host token, this firewall state"),
                   probe.host_row("policy.control.external_connect", "control", connected,
                                  "host token, this firewall state")]


def network_facts(targets):
    """What the expectation table needs to know about the round's targets."""
    return {"tcp": [key.removeprefix("tcp.") for key in targets if key.startswith("tcp.")],
            "udp": [key.removeprefix("udp.") for key in targets if key.startswith("udp.")],
            "dns": "dns_name" in targets}


def observations(probe, text):
    """Parse the launch's rows; unreadable or malformed output yields nothing, so every row fails."""
    try:
        return probe.parse_rows(text.decode("utf-8")) if text is not None else {}
    except (ValueError, UnicodeDecodeError):
        return {}


def attempt_rows(probe, host, contained, executable):
    """Run the network attempts in a fresh launch and judge them as the matrix does."""
    targets = probe.network_targets(host)
    spec = "".join(f"{key}={value}\n" for key, value in targets.items())
    (contained.control / "spec.txt").write_text(spec, encoding="utf-8")
    environment = probe.environment_values(host.windows, contained.work / "tmp", host.local_appdata)
    code, _ = contained.run([executable, "network", contained.control, contained.work], environment, 120)
    text = probe.read_worker_file(host, contained, "result.txt")
    exited = probe.host_row("launch.exit", "control", code == 0, f"exit={code}")
    return [exited] + probe.judge(probe.network_checks(network_facts(targets)), observations(probe, text))


def received_rows(probe, host):
    """Nothing from the launch may have reached a host listener in this round."""
    rows = []
    for name, listener in host.listeners.items():
        rows.append(probe.host_row(f"net.host_received.{name}", "egress", not listener.received,
                                   f"{len(listener.received)} arrivals"))
        listener.received.clear()
    return rows


def round_rows(probe, host, executable, condition):
    """One round: controls, the contained attempts, then what the listeners saw."""
    rows = control_rows(probe, host)
    contained = probe.Contained(host, probe.CANDIDATE, f"policy-{condition}", "implementer", probe.FIXTURE_LIMITS)
    try:
        rows += attempt_rows(probe, host, contained, executable)
    finally:
        rows += probe.disposal_rows(contained)
    return rows + received_rows(probe, host)


def one_round(probe, host, executable, staging, condition):
    """Ask the owner's step for a condition, probe under it and release it.

    The `done` marker is written whatever happens, so the owner's step restores
    the firewall at once.
    """
    marker(staging, condition, "ready").write_text("ready", encoding="ascii")
    wait_for_file(record_file(staging, condition), host.manifest.get("policy_wait_seconds", ROLE_WAIT))
    record = read_json(record_file(staging, condition))
    try:
        rows = condition_rows(probe, condition, record, firewall_state(host.windows))
        if record and record.get("outcome") == "applied":
            rows += round_rows(probe, host, executable, condition)
    finally:
        marker(staging, condition, "done").write_text("done", encoding="ascii")
    return probe.tag(rows, probe.CANDIDATE, f"policy-{condition}")


def unbuilt_rows(probe, condition):
    """The failing row a condition gets when there is no fixture to launch."""
    row = probe.host_row("policy.condition_applied", "egress", False, "the fixture was not built")
    return probe.tag([row], probe.CANDIDATE, f"policy-{condition}")


def role_rounds(probe, host, executable):
    """Run one network round per condition the owner's manifest lists.

    Without a built fixture no round can run, and each condition fails instead
    of being skipped.
    """
    conditions = host.manifest.get("policy_conditions", [])
    if executable is None or not conditions:
        return [row for name in conditions for row in unbuilt_rows(probe, name)]
    staging = Path(host.arguments.owner_manifest).parent
    role_directory(staging).mkdir(exist_ok=True)
    return [row for name in conditions for row in one_round(probe, host, executable, staging, name)]


# --- the waiting owner step ------------------------------------------------


def restored_row(probe, record):
    """The final report fails unless the owner's step confirmed the original firewall state is back."""
    if record is None:
        return probe.host_row("policy.host_firewall_restored", "cleanup", False,
                              "the owner's elevated step wrote no restore record")
    same = record.get("restored") is True and record.get("final") == record.get("original")
    detail = json.dumps({key: record.get(key) for key in ("original", "final", "changed")}, sort_keys=True)
    return probe.host_row("policy.host_firewall_restored", "cleanup", same, detail)


def restored_rows(probe, staging, seconds=180):
    """Wait for the elevated step's restore record and turn it into a shared row."""
    path = owner_directory(staging) / "restored.json"
    wait_for_file(path, seconds)
    return probe.tag([restored_row(probe, read_json(path))], "-", "-")


# --- the owner's elevated step ---------------------------------------------


def apply_condition(windows, condition, observe_only=False):
    """Change the firewall for one condition; returns (outcome, detail).

    The untouched state needs no change, and an observe-only run makes none.
    """
    if condition == BASELINE:
        return "applied", "no change"
    if observe_only:
        return "not-applied", "observe-only run"
    if condition == PROFILES_OFF:
        code, _, error = run_powershell(windows, f"Set-NetFirewallProfile -Name {','.join(PROFILES)} -Enabled False")
        return ("applied", "every profile disabled") if code == 0 else ("failed", error)
    code, _, error = run_powershell(windows, f"Stop-Service -Name {FIREWALL_SERVICE} -Force -ErrorAction Stop")
    return ("applied", "service stopped") if code == 0 else ("refused", error)


def confirm(condition, outcome, detail, state):
    """An applied condition counts only when the state read back afterwards matches it."""
    if outcome == "applied" and not reached(condition, state):
        return "failed", f"{detail}, but the state read back does not match; a policy may enforce it"
    return outcome, detail


def restore(windows, original):
    """Put every profile and the service back as first read; returns the state afterwards."""
    for name, enabled in original["profiles"].items():
        run_powershell(windows, f"Set-NetFirewallProfile -Name {name} -Enabled {'True' if enabled else 'False'}")
    if original["service"] == "Running":
        run_powershell(windows, f"Start-Service -Name {FIREWALL_SERVICE}")
    return firewall_state(windows)


def serve(windows, staging, condition, original, parsed):
    """Apply one condition when the role run asks for it, and undo it when the round ends.

    The firewall is back as it was after `--round-seconds` at the latest, even
    when the role run never reports that its round ended.
    """
    if not wait_for_file(marker(staging, condition, "ready"), parsed.wait):
        write_json(record_file(staging, condition), {"condition": condition, "outcome": "failed",
                                                     "detail": "the role run never asked for this condition"})
        return False
    changed = not parsed.observe_only and condition != BASELINE
    try:
        outcome, detail = apply_condition(windows, condition, parsed.observe_only)
        outcome, detail = confirm(condition, outcome, detail, firewall_state(windows))
        write_json(record_file(staging, condition), {"condition": condition, "outcome": outcome, "detail": detail})
        wait_for_file(marker(staging, condition, "done"), parsed.round_seconds)
    finally:
        if changed:
            restore(windows, original)
    return changed


def save_original(staging, state):
    """Record the untouched state once; a second run must never overwrite it with a changed one."""
    path = owner_directory(staging) / "original.json"
    if not path.exists():
        write_json(path, state)
    return read_json(path)


def finish(windows, staging, original, changed, observe_only):
    """Restore once more, read the state back and write the restore record."""
    final = firewall_state(windows) if observe_only else restore(windows, original)
    record = {"restored": final == original, "original": original, "final": final,
              "changed": changed, "observe_only": observe_only}
    write_json(owner_directory(staging) / "restored.json", record)
    return record


def owner_step(parsed):
    """Serve every condition the manifest lists, then restore and report."""
    windows = Path(os.environ.get("SystemRoot", "C:/Windows"))
    manifest = read_json(parsed.staging / "manifest.json") or {}
    conditions = parse_conditions(",".join(manifest.get("policy_conditions", [])))
    owner_directory(parsed.staging).mkdir(exist_ok=True)
    original = save_original(parsed.staging, firewall_state(windows))
    if original is None:
        raise RuntimeError("the firewall state could not be read; nothing was changed")
    changed = []
    try:
        for name in conditions:
            if serve(windows, parsed.staging, name, original, parsed):
                changed.append(name)
    finally:
        record = finish(windows, parsed.staging, original, changed, parsed.observe_only)
    print(json.dumps(record, indent=2, sort_keys=True))
    return 0 if record["restored"] else 1


def restore_only(parsed):
    """Recovery after an interrupted run: put back the state saved in `original.json`."""
    original = read_json(owner_directory(parsed.staging) / "original.json")
    if original is None:
        raise RuntimeError("no saved original state in the staging directory")
    windows = Path(os.environ.get("SystemRoot", "C:/Windows"))
    record = finish(windows, parsed.staging, original, ["restore-only"], False)
    print(json.dumps(record, indent=2, sort_keys=True))
    return 0 if record["restored"] else 1


def arguments():
    """Parse the command line of the owner's elevated step."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--staging", type=Path, required=True, help="the staging directory of the owner step")
    parser.add_argument("--wait", type=int, default=3600, help="seconds to wait for the role run to ask for a round")
    parser.add_argument("--round-seconds", type=int, default=180,
                        help="longest a changed firewall state is kept before it is restored")
    parser.add_argument("--observe-only", action="store_true",
                        help="change nothing; exercises the handshake without elevation, proves no policy loss")
    parser.add_argument("--restore", action="store_true", help="only restore the saved original state, then exit")
    return parser.parse_args()


def main():
    """Run the owner's elevated step; the exit status is zero only when the firewall is as it was."""
    parsed = arguments()
    if sys.platform != "win32":
        raise SystemExit("requires native Windows")
    if not parsed.observe_only and not ctypes.windll.shell32.IsUserAnAdmin():
        raise SystemExit("changing firewall state needs an elevated shell; nothing was changed")
    return restore_only(parsed) if parsed.restore else owner_step(parsed)


if __name__ == "__main__":
    raise SystemExit(main())
