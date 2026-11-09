#!/usr/bin/env python3
"""Drive launches through the Rust launcher instead of windows-probe-native.py.

`agentc-supervisor windows-runner` is the Windows backend of the supervisor's
host-runtime interface. It serves typed requests, one JSON object per line.
This module starts one runner per probe run and gives the driver the objects
it already uses: a launch with `start`, `wait`, `terminate` and `close`, and
a reader for worker-written files.

Everything that makes the boundary (the AppContainer profile, the Job Object,
the window station and desktop, the handle list, the read-back before resume)
then happens in the runner. The driver keeps what is not part of a launch: the
canaries, listeners, broker stub and the accepted value of every row.

The module imports everywhere; nothing starts until `Client` is constructed.
"""
import json
from pathlib import Path
import subprocess


class RunnerError(RuntimeError):
    """The runner refused a request; `refusal` names a typed refusal when there is one."""

    def __init__(self, reply):
        super().__init__(reply.get("error", "the runner gave no answer"))
        self.refusal = reply.get("refusal")


class Client:
    """One runner process and the request channel to it."""

    def __init__(self, executable, state_directory, log_path):
        self.executable = Path(executable)
        self.log = open(log_path, "wb")
        arguments = [str(self.executable), "windows-runner", "--state-dir", str(state_directory)]
        self.process = subprocess.Popen(arguments, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.log)
        self.identity = self.ask("identity")
        # True once the runner made the window station, which its first launch needs.
        self.station_made = False

    def ask(self, operation, **fields):
        """Send one request and return its answer, raising `RunnerError` when it is refused."""
        line = json.dumps({"op": operation, **fields}) + "\n"
        try:
            self.process.stdin.write(line.encode("utf-8"))
            self.process.stdin.flush()
            reply = json.loads(self.process.stdout.readline() or b"{}")
        except (OSError, ValueError) as error:
            raise RunnerError({"error": f"the runner stopped answering: {error}"}) from error
        if reply.get("ok") is not True:
            raise RunnerError(reply)
        return reply

    def prepare(self, launch, profile, boundary, limits, control, work):
        """Create one launch's profile, directories and desktop; returns its package SID and desktop."""
        spec = {"profile": profile, "less_privileged": boundary.lpac,
                "capabilities": list(boundary.capabilities),
                "limits": {"processes": limits.processes, "process_memory_mb": limits.process_memory_mb,
                           "job_memory_mb": limits.job_memory_mb},
                "control": str(control), "work": str(work)}
        prepared = self.ask("prepare", launch=launch, spec=spec)
        self.station_made = True
        return prepared

    def read(self, root, relative, limit):
        """Read a worker-written file by handle through the runner's reader."""
        reply = self.ask("read", root=str(root), relative=str(relative), limit=limit)
        return bytes.fromhex(reply["hex"])

    def close(self):
        """End the runner; closing its input makes it exit, which ends every launch it held."""
        try:
            self.process.stdin.close()
            self.process.wait(timeout=30)
        except (OSError, subprocess.TimeoutExpired):
            self.process.kill()
            self.process.wait()
        self.process.stdout.close()
        self.log.close()


class Launch:
    """One prepared launch, with the interface of `windows-probe-native.Launch`."""

    def __init__(self, client, name):
        self.client, self.name = client, name
        self.pid, self.process, self.reaped = None, None, False
        self.watching = False

    def hold_file(self, path):
        """Have the runner open `path` with an inheritable handle; returns the handle's value."""
        return self.client.ask("hold_file", launch=self.name, path=str(path))["handle"]

    def start(self, argv, cwd, environment, output_path, passed_handles=()):
        """Start one process tree; only the log and the listed held handles are inherited."""
        started = self.client.ask(
            "start", launch=self.name, argv=[str(value) for value in argv], directory=str(cwd),
            environment={name: str(value) for name, value in environment.items()},
            output=str(output_path), listed=list(passed_handles))
        self.pid, self.process = started["pid"], started

    def wait(self, seconds):
        """The exit code, or None when the process is still running after `seconds`."""
        seen = self.client.ask("observe", launch=self.name, wait_ms=int(seconds * 1000))
        return seen["code"] if seen["state"] == "exited" else None

    def watch(self, pid):
        """Watch a process the launch reported; True when it is alive inside the launch's job."""
        self.watching = True
        return self.client.ask("watch", launch=self.name, pid=pid)["alive_in_job"]

    def terminate(self):
        """Empty the job; when a descendant is watched, also record whether it ended with the job."""
        quiescent = self.client.ask("terminate", launch=self.name)["quiescent"]
        if self.watching:
            self.reaped = self.client.ask("watched_ended", launch=self.name, wait_ms=10000)["ended"]
            self.watching = False
        return quiescent

    def close(self):
        """Nothing to release: the runner closed the job when it was emptied."""

    def cleanup(self):
        """Delete the launch's desktop and profile; True when the profile was deleted."""
        return self.client.ask("cleanup", launch=self.name)["profile_deleted"]
