#!/usr/bin/env python3
"""Disposable native Windows AppContainer experiment, never a host admission gate.

No provider/coordinator credentials, services, firewall rules or existing ACLs
are read or changed. Only newly created probe directories get ACL changes.
Missing/failed probes are failures, never evidence of a security denial.
Run with python -I windows-containment-probe.py --output ABSOLUTE_NEW_JSON_PATH.
"""
import argparse
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time
import uuid


def environment_block(root, windows, local_appdata):
    """Do not copy the invoking harness's credential-bearing environment."""
    values = {"SystemRoot": str(windows), "WINDIR": str(windows),
              "TEMP": str(root), "TMP": str(root),
              "PATH": str(windows / "System32"), "LOCALAPPDATA": str(local_appdata)}
    return "\0".join(f"{key}={value}" for key, value in sorted(values.items())) + "\0\0"


def verdict(rows):
    # Even a complete primitive experiment cannot admit a production host.
    return {"experiment_passed": bool(rows) and all(row["status"] == "pass" for row in rows),
            "host_admitted": False,
            "unverified": ["role service identities", "same-role concurrent launches",
                           "reviewer harness interception", "network/publication brokers",
                           "IPC and registry isolation", "reparse/hard-link races",
                           "SCM lifecycle and recovery", "native Rust toolchain"]}


def checked(program, arguments):
    result = subprocess.run([str(program), *arguments], stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            timeout=60, check=False)
    if result.returncode:
        raise RuntimeError(f"{Path(program).name} exited {result.returncode}")


class Startup(c.Structure):
    _fields_ = [("cb", w.DWORD), ("reserved", w.LPWSTR), ("desktop", w.LPWSTR),
                ("title", w.LPWSTR), ("x", w.DWORD), ("y", w.DWORD),
                ("xsize", w.DWORD), ("ysize", w.DWORD), ("xchars", w.DWORD),
                ("ychars", w.DWORD), ("fill", w.DWORD), ("flags", w.DWORD),
                ("show", w.WORD), ("reserved_size", w.WORD), ("reserved2", c.c_void_p),
                ("stdin", w.HANDLE), ("stdout", w.HANDLE), ("stderr", w.HANDLE)]


class StartupEx(c.Structure):
    _fields_ = [("startup", Startup), ("attributes", c.c_void_p)]


class ProcessInfo(c.Structure):
    _fields_ = [("process", w.HANDLE), ("thread", w.HANDLE),
                ("pid", w.DWORD), ("tid", w.DWORD)]


class SecurityCapabilities(c.Structure):
    _fields_ = [("sid", c.c_void_p), ("capabilities", c.c_void_p),
                ("count", w.DWORD), ("reserved", w.DWORD)]


class BasicLimit(c.Structure):
    _fields_ = [("process_time", c.c_int64), ("job_time", c.c_int64),
                ("flags", w.DWORD), ("minimum", c.c_size_t), ("maximum", c.c_size_t),
                ("processes", w.DWORD), ("affinity", c.c_size_t),
                ("priority", w.DWORD), ("scheduling", w.DWORD)]


class IoCounters(c.Structure):
    _fields_ = [(name, c.c_uint64) for name in
                ("read_ops", "write_ops", "other_ops", "read_bytes", "write_bytes", "other_bytes")]


class ExtendedLimit(c.Structure):
    _fields_ = [("basic", BasicLimit), ("io", IoCounters),
                ("process_memory", c.c_size_t), ("job_memory", c.c_size_t),
                ("peak_process", c.c_size_t), ("peak_job", c.c_size_t)]


class Native:
    def __init__(self):
        if sys.platform != "win32":
            raise RuntimeError("native Windows is required")
        self.kernel = c.WinDLL("kernel32", use_last_error=True)
        self.userenv = c.WinDLL("userenv", use_last_error=True)
        self.advapi = c.WinDLL("advapi32", use_last_error=True)
        self.bind(self.userenv, "CreateAppContainerProfile", c.c_long,
                  [w.LPCWSTR, w.LPCWSTR, w.LPCWSTR, c.c_void_p, w.DWORD, c.POINTER(c.c_void_p)])
        self.bind(self.userenv, "DeleteAppContainerProfile", c.c_long, [w.LPCWSTR])
        self.bind(self.advapi, "FreeSid", c.c_void_p, [c.c_void_p])
        self.bind(self.advapi, "ConvertSidToStringSidW", w.BOOL,
                  [c.c_void_p, c.POINTER(w.LPWSTR)])
        self.bind(self.kernel, "LocalFree", c.c_void_p, [c.c_void_p])
        self.bind(self.kernel, "CloseHandle", w.BOOL, [w.HANDLE])
        self.bind(self.kernel, "CreateJobObjectW", w.HANDLE, [c.c_void_p, w.LPCWSTR])
        self.bind(self.kernel, "SetInformationJobObject", w.BOOL,
                  [w.HANDLE, c.c_int, c.c_void_p, w.DWORD])
        self.bind(self.kernel, "TerminateJobObject", w.BOOL, [w.HANDLE, w.UINT])
        self.bind(self.kernel, "InitializeProcThreadAttributeList", w.BOOL,
                  [c.c_void_p, w.DWORD, w.DWORD, c.POINTER(c.c_size_t)])
        self.bind(self.kernel, "UpdateProcThreadAttribute", w.BOOL,
                  [c.c_void_p, w.DWORD, c.c_size_t, c.c_void_p, c.c_size_t,
                   c.c_void_p, c.c_void_p])
        self.bind(self.kernel, "DeleteProcThreadAttributeList", None, [c.c_void_p])
        self.bind(self.kernel, "CreateProcessW", w.BOOL,
                  [w.LPCWSTR, w.LPWSTR, c.c_void_p, c.c_void_p, w.BOOL, w.DWORD,
                   c.c_void_p, w.LPCWSTR, c.POINTER(StartupEx), c.POINTER(ProcessInfo)])
        self.bind(self.kernel, "ResumeThread", w.DWORD, [w.HANDLE])
        self.bind(self.kernel, "WaitForSingleObject", w.DWORD, [w.HANDLE, w.DWORD])
        self.bind(self.kernel, "GetExitCodeProcess", w.BOOL, [w.HANDLE, c.POINTER(w.DWORD)])

    @staticmethod
    def bind(library, name, result, arguments):
        function = getattr(library, name)
        function.restype = result
        function.argtypes = arguments

    @staticmethod
    def require(ok, operation):
        if not ok:
            raise OSError(c.get_last_error(), operation)

    def profile(self, name):
        sid = c.c_void_p()
        result = self.userenv.CreateAppContainerProfile(name, name, "Agentc disposable probe",
                                                        None, 0, c.byref(sid))
        if result != 0:
            raise OSError(result & 0xffffffff, "CreateAppContainerProfile")
        return sid

    def sid_string(self, sid):
        value = w.LPWSTR()
        self.require(self.advapi.ConvertSidToStringSidW(sid, c.byref(value)), "SID conversion")
        try:
            return value.value
        finally:
            self.kernel.LocalFree(c.cast(value, c.c_void_p))

    def run(self, sid, argv, root, windows, lpac, seconds=60):
        """Atomic job assignment; no uncontained retry, inherited handles or environment."""
        job = self.kernel.CreateJobObjectW(None, None)
        self.require(job, "CreateJobObjectW")
        attributes = None
        process = ProcessInfo()
        try:
            limits = ExtendedLimit()
            limits.basic.flags = 0x2000 | 0x8 | 0x100 | 0x200  # kill-on-close, count, memory
            limits.basic.processes = 16
            limits.process_memory = 512 * 1024 * 1024
            limits.job_memory = 1024 * 1024 * 1024
            self.require(self.kernel.SetInformationJobObject(job, 9, c.byref(limits),
                                                             c.sizeof(limits)), "job limits")
            size = c.c_size_t()
            count = 3 if lpac else 2
            self.kernel.InitializeProcThreadAttributeList(None, count, 0, c.byref(size))
            attributes = c.create_string_buffer(size.value)
            self.require(self.kernel.InitializeProcThreadAttributeList(attributes, count, 0,
                                                                       c.byref(size)), "attribute list")
            capabilities = SecurityCapabilities(sid, None, 0, 0)
            jobs = (w.HANDLE * 1)(job)
            optout = w.DWORD(1)
            entries = [(0x20009, capabilities), (0x2000d, jobs)]
            if lpac:
                entries.append((0x2000e, optout))
            for attribute, value in entries:
                self.require(self.kernel.UpdateProcThreadAttribute(attributes, 0, attribute,
                             c.byref(value), c.sizeof(value), None, None), "process attribute")
            startup = StartupEx()
            startup.startup.cb = c.sizeof(startup)
            startup.attributes = c.cast(attributes, c.c_void_p)
            command = c.create_unicode_buffer(subprocess.list2cmdline([str(value) for value in argv]))
            # CreateProcess resolves the package profile using LOCALAPPDATA.
            # This path is not a filesystem grant and carries no credential.
            environment = c.create_unicode_buffer(environment_block(root, windows, os.environ["LOCALAPPDATA"]))
            self.require(self.kernel.CreateProcessW(str(argv[0]), command, None, None, False,
                         0x80000 | 0x400 | 0x4 | 0x8000000, environment, str(root),
                         c.byref(startup), c.byref(process)), "CreateProcessW")
            self.require(self.kernel.ResumeThread(process.thread) != 0xffffffff, "ResumeThread")
            status = self.kernel.WaitForSingleObject(process.process, seconds * 1000)
            if status != 0:
                raise RuntimeError(f"process wait failed or timed out ({status}); no denial proven")
            code = w.DWORD()
            self.require(self.kernel.GetExitCodeProcess(process.process, c.byref(code)), "exit code")
            return code.value
        finally:
            # Also kills descendants after a normal root exit; no detached producer survives.
            terminated = self.kernel.TerminateJobObject(job, 1)
            if process.thread:
                self.kernel.CloseHandle(process.thread)
            if process.process:
                self.kernel.CloseHandle(process.process)
            if attributes is not None:
                self.kernel.DeleteProcThreadAttributeList(attributes)
            self.kernel.CloseHandle(job)
            self.require(terminated, "TerminateJobObject")


def experiment(root, windows, rows, lpac):
    native = Native()
    name = "agentc.probe." + uuid.uuid4().hex
    sid = native.profile(name)
    label = "lpac" if lpac else "appcontainer"
    work = root / label
    work.mkdir()
    try:
        sid_text = native.sid_string(sid)
        # Only this newly created directory changes; no grants on the host toolchain.
        checked(windows / "System32/icacls.exe",
                [str(work), "/grant:r", f"*{sid_text}:(OI)(CI)(M)"])
        checked(windows / "System32/icacls.exe", [str(work), "/setintegritylevel", "(OI)(CI)L"])
        source = Path(__file__).with_name("windows-probe-fixture.cs")
        (work / "fixture.cs").write_bytes(source.read_bytes())
        compiler = windows / "Microsoft.NET/Framework64/v4.0.30319/csc.exe"
        argv = [compiler, "/nologo", "/target:exe", "/out:" + str(work / "fixture.exe"),
                str(work / "fixture.cs")]
        # Positive control: the exact compiler/source must work before a denial counts.
        checked(compiler, argv[1:])
        (work / "fixture.exe").unlink()
        code = native.run(sid, argv, work, windows, lpac)
        if code != 0 or not (work / "fixture.exe").is_file():
            raise RuntimeError(f"contained compiler exited {code}; no successful build")
        rows.append({"check": label + ".build", "status": "pass", "detail": "C# compiler ran contained"})
        result = work / "result.txt"
        code = native.run(sid, [work / "fixture.exe", result, root / "host-canary.txt",
                               str(os.getpid())], work, windows, lpac)
        expected = {"appcontainer", "own_write", "host_read_denied", "host_write_denied",
                    "host_process_denied", "child_contained"}
        observed = parse_results(result.read_text(encoding="utf-8-sig"), expected) if result.is_file() else {}
        for check in sorted(expected):
            rows.append({"check": label + "." + check,
                         "status": "pass" if code == 0 and observed.get(check) is True else "fail",
                         "detail": f"contained fixture exit={code}; observation={observed.get(check)}"})
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        rows.append({"check": label + ".execution", "status": "fail", "detail": str(error)})
    finally:
        native.advapi.FreeSid(sid)
        result = native.userenv.DeleteAppContainerProfile(name)
        rows.append({"check": label + ".profile_cleanup", "status": "pass" if result == 0 else "fail",
                     "detail": f"DeleteAppContainerProfile HRESULT={result & 0xffffffff}"})


def parse_results(body, expected):
    result = {}
    for line in body.splitlines():
        key, separator, value = line.partition("=")
        if not separator or key not in expected or key in result or value not in ("true", "false"):
            raise RuntimeError("malformed, duplicate or unexpected fixture observation")
        result[key] = value == "true"
    if set(result) != expected:
        raise RuntimeError("incomplete fixture observations")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != "win32" or not args.output.is_absolute():
        parser.error("requires native Windows and an absolute, new output path")
    # Exclusive creation before side effects; existing evidence is never overwritten.
    with args.output.open("x", encoding="utf-8") as output:
        root = Path(tempfile.mkdtemp(prefix="agentc-windows-probe-"))
        (root / "host-canary.txt").write_text("synthetic, not a credential", encoding="utf-8")
        windows = Path(os.environ.get("SystemRoot", "C:/Windows"))
        rows = []
        try:
            for lpac in (False, True):
                experiment(root, windows, rows, lpac)
        except (OSError, RuntimeError, subprocess.SubprocessError) as error:
            rows.append({"check": "setup", "status": "fail", "detail": str(error)})
        report = {"schema_version": 1, "recorded_at_unix": time.time(),
                  "host": platform.platform(), "work_directory": str(root),
                  "scope": "primitive experiment only; no services or provider harness",
                  "checks": rows, **verdict(rows)}
        json.dump(report, output, indent=2)
        output.write("\n")
    print(json.dumps(report, indent=2))
    return 0 if report["experiment_passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
