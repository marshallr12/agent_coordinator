#!/usr/bin/env python3
"""Windows API layer for windows-containment-probe.py.

It provides the launch boundary under evaluation (a disposable AppContainer
profile, an optional less-privileged token, a Job Object and a private window
station), the host-side stand-ins the fixture attacks (pipes, events and
listeners) and a handle-based reader that refuses worker-planted links.

The module imports everywhere. Nothing touches a Windows DLL until `Api()` is
constructed, so the portable tests can load it on Linux.
"""
import ctypes as c
from ctypes import wintypes as w
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time
from typing import NamedTuple
import uuid

GENERIC_READ, GENERIC_WRITE, GENERIC_ALL = 0x80000000, 0x40000000, 0x10000000
SHARE_ALL, CREATE_ALWAYS, OPEN_EXISTING = 0x7, 2, 3
OPEN_REPARSE_POINT, BACKUP_SEMANTICS = 0x00200000, 0x02000000
ATTRIBUTE_DIRECTORY, ATTRIBUTE_REPARSE_POINT = 0x10, 0x400
INVALID_HANDLE = c.c_void_p(-1).value
TOKEN_QUERY = 0x8
TOKEN_USER, TOKEN_IS_APPCONTAINER, TOKEN_APPCONTAINER_SID, TOKEN_SECURITY_ATTRIBUTES = 1, 29, 31, 39
# Windows records the "all application packages" opt-out as this token security attribute.
LPAC_ATTRIBUTE = "WIN://NOALLAPPPKG"
# Process creation: extended startup info, Unicode environment, suspended, no console window.
CREATION_FLAGS = 0x00080000 | 0x00000400 | 0x00000004 | 0x08000000
ATTRIBUTE_HANDLE_LIST, ATTRIBUTE_SECURITY_CAPABILITIES = 0x00020002, 0x00020009
ATTRIBUTE_JOB_LIST, ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY = 0x0002000D, 0x0002000F
# Job limits: active process count, per-process and per-job memory, no error
# dialogs, and kill every member when the last job handle closes. Breakaway is
# deliberately absent.
JOB_LIMIT_FLAGS = 0x8 | 0x100 | 0x200 | 0x400 | 0x2000
JOB_UI_RESTRICTIONS_ALL = 0xFF
PIPE_DUPLEX, PIPE_FIRST_INSTANCE, PIPE_REJECT_REMOTE = 0x3, 0x00080000, 0x8
ERROR_PIPE_CONNECTED = 535
SE_GROUP_ENABLED = 0x4


class SecurityAttributes(c.Structure):
    _fields_ = [("length", w.DWORD), ("descriptor", c.c_void_p), ("inherit", w.BOOL)]


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


class SidAndAttributes(c.Structure):
    _fields_ = [("sid", c.c_void_p), ("attributes", w.DWORD)]


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


class Accounting(c.Structure):
    _fields_ = [(name, c.c_int64) for name in ("user", "kernel", "period_user", "period_kernel")] + [
        (name, w.DWORD) for name in ("faults", "total", "active", "terminated")]


class FileInformation(c.Structure):
    _fields_ = [("attributes", w.DWORD), ("created", w.FILETIME), ("accessed", w.FILETIME),
                ("written", w.FILETIME), ("volume", w.DWORD), ("size_high", w.DWORD),
                ("size_low", w.DWORD), ("links", w.DWORD), ("index_high", w.DWORD),
                ("index_low", w.DWORD)]


_SID_OUT = c.POINTER(c.POINTER(c.c_void_p))
_ATTRIBUTES = c.POINTER(SecurityAttributes)
SIGNATURES = {
    "kernel32": {
        "CloseHandle": (w.BOOL, [w.HANDLE]),
        "LocalFree": (c.c_void_p, [c.c_void_p]),
        "GetCurrentProcess": (w.HANDLE, []),
        "GetCurrentThread": (w.HANDLE, []),
        "OpenProcess": (w.HANDLE, [w.DWORD, w.BOOL, w.DWORD]),
        "ProcessIdToSessionId": (w.BOOL, [w.DWORD, c.POINTER(w.DWORD)]),
        "CreateFileW": (w.HANDLE, [w.LPCWSTR, w.DWORD, w.DWORD, c.c_void_p, w.DWORD, w.DWORD, w.HANDLE]),
        "ReadFile": (w.BOOL, [w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p]),
        "WriteFile": (w.BOOL, [w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p]),
        "FlushFileBuffers": (w.BOOL, [w.HANDLE]),
        "SetFilePointerEx": (w.BOOL, [w.HANDLE, c.c_int64, c.c_void_p, w.DWORD]),
        "GetFileInformationByHandle": (w.BOOL, [w.HANDLE, c.POINTER(FileInformation)]),
        "GetFinalPathNameByHandleW": (w.DWORD, [w.HANDLE, w.LPWSTR, w.DWORD, w.DWORD]),
        "GetShortPathNameW": (w.DWORD, [w.LPCWSTR, w.LPWSTR, w.DWORD]),
        "QueryDosDeviceW": (w.DWORD, [w.LPCWSTR, w.LPWSTR, w.DWORD]),
        "CreateJobObjectW": (w.HANDLE, [c.c_void_p, w.LPCWSTR]),
        "SetInformationJobObject": (w.BOOL, [w.HANDLE, c.c_int, c.c_void_p, w.DWORD]),
        "QueryInformationJobObject": (w.BOOL, [w.HANDLE, c.c_int, c.c_void_p, w.DWORD, c.c_void_p]),
        "TerminateJobObject": (w.BOOL, [w.HANDLE, w.UINT]),
        "IsProcessInJob": (w.BOOL, [w.HANDLE, w.HANDLE, c.POINTER(w.BOOL)]),
        "InitializeProcThreadAttributeList": (w.BOOL, [c.c_void_p, w.DWORD, w.DWORD, c.POINTER(c.c_size_t)]),
        "UpdateProcThreadAttribute": (w.BOOL, [c.c_void_p, w.DWORD, c.c_size_t, c.c_void_p, c.c_size_t,
                                               c.c_void_p, c.c_void_p]),
        "DeleteProcThreadAttributeList": (None, [c.c_void_p]),
        "CreateProcessW": (w.BOOL, [w.LPCWSTR, w.LPWSTR, c.c_void_p, c.c_void_p, w.BOOL, w.DWORD,
                                    c.c_void_p, w.LPCWSTR, c.POINTER(StartupEx), c.POINTER(ProcessInfo)]),
        "ResumeThread": (w.DWORD, [w.HANDLE]),
        "WaitForSingleObject": (w.DWORD, [w.HANDLE, w.DWORD]),
        "GetExitCodeProcess": (w.BOOL, [w.HANDLE, c.POINTER(w.DWORD)]),
        "CreateNamedPipeW": (w.HANDLE, [w.LPCWSTR, w.DWORD, w.DWORD, w.DWORD, w.DWORD, w.DWORD, w.DWORD,
                                        _ATTRIBUTES]),
        "ConnectNamedPipe": (w.BOOL, [w.HANDLE, c.c_void_p]),
        "DisconnectNamedPipe": (w.BOOL, [w.HANDLE]),
        "CreateEventW": (w.HANDLE, [c.c_void_p, w.BOOL, w.BOOL, w.LPCWSTR]),
        "OpenEventW": (w.HANDLE, [w.DWORD, w.BOOL, w.LPCWSTR]),
    },
    "kernelbase": {
        "DeriveCapabilitySidsFromName": (w.BOOL, [w.LPCWSTR, _SID_OUT, c.POINTER(w.DWORD), _SID_OUT,
                                                  c.POINTER(w.DWORD)]),
    },
    "advapi32": {
        "FreeSid": (c.c_void_p, [c.c_void_p]),
        "ConvertSidToStringSidW": (w.BOOL, [c.c_void_p, c.POINTER(w.LPWSTR)]),
        "ConvertStringSecurityDescriptorToSecurityDescriptorW": (
            w.BOOL, [w.LPCWSTR, w.DWORD, c.POINTER(c.c_void_p), c.c_void_p]),
        "OpenProcessToken": (w.BOOL, [w.HANDLE, w.DWORD, c.POINTER(w.HANDLE)]),
        "OpenThreadToken": (w.BOOL, [w.HANDLE, w.DWORD, w.BOOL, c.POINTER(w.HANDLE)]),
        "GetTokenInformation": (w.BOOL, [w.HANDLE, c.c_int, c.c_void_p, w.DWORD, c.POINTER(w.DWORD)]),
        "ImpersonateNamedPipeClient": (w.BOOL, [w.HANDLE]),
        "RevertToSelf": (w.BOOL, []),
    },
    "userenv": {
        "CreateAppContainerProfile": (c.c_long, [w.LPCWSTR, w.LPCWSTR, w.LPCWSTR, c.c_void_p, w.DWORD,
                                                 c.POINTER(c.c_void_p)]),
        "DeleteAppContainerProfile": (c.c_long, [w.LPCWSTR]),
    },
    "user32": {
        "CreateWindowStationW": (w.HANDLE, [w.LPCWSTR, w.DWORD, w.DWORD, _ATTRIBUTES]),
        "GetProcessWindowStation": (w.HANDLE, []),
        "SetProcessWindowStation": (w.BOOL, [w.HANDLE]),
        "CloseWindowStation": (w.BOOL, [w.HANDLE]),
        "CreateDesktopW": (w.HANDLE, [w.LPCWSTR, w.LPCWSTR, c.c_void_p, w.DWORD, w.DWORD, _ATTRIBUTES]),
        "CloseDesktop": (w.BOOL, [w.HANDLE]),
        "GetUserObjectInformationW": (w.BOOL, [w.HANDLE, c.c_int, c.c_void_p, w.DWORD, c.POINTER(w.DWORD)]),
        "FindWindowW": (w.HANDLE, [w.LPCWSTR, w.LPCWSTR]),
    },
    "shell32": {
        "IsUserAnAdmin": (w.BOOL, []),
    },
}


class Api:
    """Typed bindings for every Windows call the probe makes."""

    def __init__(self):
        if sys.platform != "win32":
            raise RuntimeError("native Windows is required")
        for library, functions in SIGNATURES.items():
            self._bind(c.WinDLL(library, use_last_error=True), functions)

    def _bind(self, library, functions):
        """Give each function its exact result and argument types."""
        for name, (result, arguments) in functions.items():
            function = getattr(library, name)
            function.restype = result
            function.argtypes = arguments
            setattr(self, name, function)


def require(result, operation):
    """Return a truthy API result, or raise the thread's last Windows error."""
    if not result or result == INVALID_HANDLE:
        raise OSError(c.get_last_error(), operation)
    return result


def sid_text(api, sid):
    """Render a SID pointer as `S-1-...` text."""
    text = w.LPWSTR()
    require(api.ConvertSidToStringSidW(sid, c.byref(text)), "ConvertSidToStringSidW")
    try:
        return text.value
    finally:
        api.LocalFree(c.cast(text, c.c_void_p))


def token_block(api, token, information_class):
    """Read one token information class into a buffer."""
    buffer = c.create_string_buffer(512)
    needed = w.DWORD()
    require(api.GetTokenInformation(token, information_class, buffer, len(buffer), c.byref(needed)),
            "GetTokenInformation")
    return buffer


def token_sid(api, token, information_class):
    """Text of the SID a token information block starts with, or None when it has none."""
    pointer = c.cast(token_block(api, token, information_class), c.POINTER(c.c_void_p))[0]
    return sid_text(api, pointer) if pointer else None


def token_flag(api, token, information_class):
    """A boolean token fact such as "is an AppContainer"."""
    return c.cast(token_block(api, token, information_class), c.POINTER(w.DWORD))[0] != 0


def token_is_lpac(api, token):
    """Whether a token opted out of "all application packages" access.

    The dedicated information class is rejected on the evaluated build, so the
    opt-out is read from the token's security attributes, which name it.
    """
    buffer, needed = c.create_string_buffer(4096), w.DWORD()
    if not api.GetTokenInformation(token, TOKEN_SECURITY_ATTRIBUTES, buffer, len(buffer), c.byref(needed)):
        return False
    return LPAC_ATTRIBUTE.encode("utf-16-le") in buffer.raw[:needed.value]


def process_token(api, process):
    """Open a process token for queries; the caller closes it."""
    token = w.HANDLE()
    require(api.OpenProcessToken(process, TOKEN_QUERY, c.byref(token)), "OpenProcessToken")
    return token


def current_user_sid(api):
    """The SID of the account running the probe, read from its own token."""
    token = process_token(api, api.GetCurrentProcess())
    try:
        return token_sid(api, token, TOKEN_USER)
    finally:
        api.CloseHandle(token)


def session_id(api):
    """The terminal session the probe runs in; it names AppContainer object directories."""
    session = w.DWORD()
    require(api.ProcessIdToSessionId(os.getpid(), c.byref(session)), "ProcessIdToSessionId")
    return session.value


def capability_sid(api, name):
    """Derive the SID Windows assigns to a named capability.

    Any name yields a SID, which lets a tool directory name one capability in
    its ACL instead of gaining an entry for every launch.
    """
    groups, group_count = c.POINTER(c.c_void_p)(), w.DWORD()
    sids, count = c.POINTER(c.c_void_p)(), w.DWORD()
    require(api.DeriveCapabilitySidsFromName(name, c.byref(groups), c.byref(group_count),
                                            c.byref(sids), c.byref(count)), "DeriveCapabilitySidsFromName")
    return sids[0]


class Descriptor:
    """A security descriptor built from SDDL, with the attributes block APIs expect."""

    def __init__(self, api, sddl, inherit=False):
        pointer = c.c_void_p()
        require(api.ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, 1, c.byref(pointer), None),
                "security descriptor")
        self.attributes = SecurityAttributes(c.sizeof(SecurityAttributes), pointer.value, inherit)

    def reference(self):
        """A pointer suitable for an `LPSECURITY_ATTRIBUTES` argument."""
        return c.byref(self.attributes)


def launch_descriptor(api, owner_sid, package_sid, access="GA"):
    """Grant the owner everything and one package `access`, labelled low integrity.

    An AppContainer token runs at low integrity, so an object it must write
    to needs a low label as well as an entry for its package SID.
    """
    return Descriptor(api, f"D:(A;;GA;;;{owner_sid})(A;;{access};;;{package_sid})S:(ML;;NW;;;LW)")


class Profile:
    """A disposable AppContainer profile; its package SID is the launch identity."""

    def __init__(self, api, name):
        self.api, self.name = api, name
        self.sid = c.c_void_p()
        result = api.CreateAppContainerProfile(name, name, "Agentc disposable probe", None, 0,
                                               c.byref(self.sid))
        if result != 0:
            raise OSError(result & 0xffffffff, "CreateAppContainerProfile")
        self.sid_text = sid_text(api, self.sid)

    def delete(self):
        """Remove the profile and its package folder; returns the HRESULT."""
        self.api.FreeSid(self.sid)
        return self.api.DeleteAppContainerProfile(self.name) & 0xffffffff


class Desktop:
    """A private window station and desktop only the owner and one package may open.

    It gives the launch its own clipboard, atom table and message queue,
    separate from the interactive session and from every other launch.
    """

    def __init__(self, api, owner_sid, package_sid):
        self.api = api
        descriptor = launch_descriptor(api, owner_sid, package_sid)
        self.station = require(api.CreateWindowStationW(None, 0, GENERIC_ALL, descriptor.reference()),
                               "CreateWindowStationW")
        self.desktop = self._create_desktop(descriptor)
        self.name = self._name(self.station) + "\\" + self._name(self.desktop)

    def _create_desktop(self, descriptor):
        """Create the desktop inside the new station, then restore the probe's own station."""
        original = self.api.GetProcessWindowStation()
        require(self.api.SetProcessWindowStation(self.station), "SetProcessWindowStation")
        try:
            return require(self.api.CreateDesktopW("agentc-" + uuid.uuid4().hex, None, None, 0, GENERIC_ALL,
                                                   descriptor.reference()), "CreateDesktopW")
        finally:
            self.api.SetProcessWindowStation(original)

    def _name(self, handle):
        """The object name Windows reports for a station or desktop handle."""
        buffer, needed = c.create_unicode_buffer(256), w.DWORD()
        require(self.api.GetUserObjectInformationW(handle, 2, buffer, c.sizeof(buffer), c.byref(needed)),
                "GetUserObjectInformationW")
        return buffer.value

    def close(self):
        """Close the desktop and its station."""
        self.api.CloseDesktop(self.desktop)
        self.api.CloseWindowStation(self.station)


class Limits(NamedTuple):
    """Job Object bounds for one launch."""
    processes: int
    process_memory_mb: int
    job_memory_mb: int


class Boundary(NamedTuple):
    """The token shape under evaluation: ordinary or less-privileged, plus capabilities."""
    lpac: bool
    capabilities: tuple


class Launch:
    """One contained process tree: an AppContainer token inside a Job Object.

    The process starts suspended and is resumed only after the OS confirms the
    job and token it actually received. There is no uncontained fallback.
    """

    def __init__(self, api, profile, boundary, desktop_name, limits):
        if profile is None or boundary is None:
            raise RuntimeError("a contained launch requires a package identity and a boundary")
        self.api, self.profile, self.boundary = api, profile, boundary
        self.desktop_name, self.limits = desktop_name, limits
        self.job = self.process = self.thread = self.output = None
        self.pid = None

    def start(self, argv, cwd, environment, output_path, passed_handles=()):
        """Create the job, start the process inside the boundary and resume it once verified.

        Only the output log and `passed_handles` are inherited; every other
        inheritable handle the host holds stays behind.
        """
        self.job = self._job()
        self.output = self._output(output_path)
        self.passed_handles = tuple(passed_handles)
        attributes = self._attributes()
        try:
            self._create(argv, cwd, environment, attributes)
            self._verify()
        except BaseException:
            self.terminate()
            raise
        finally:
            self.api.DeleteProcThreadAttributeList(attributes)
        require(self.api.ResumeThread(self.thread) != 0xffffffff, "ResumeThread")

    def _job(self):
        """A job that bounds processes and memory and kills every member on close."""
        job = require(self.api.CreateJobObjectW(None, None), "CreateJobObjectW")
        limits = ExtendedLimit()
        limits.basic.flags = JOB_LIMIT_FLAGS
        limits.basic.processes = self.limits.processes
        limits.process_memory = self.limits.process_memory_mb << 20
        limits.job_memory = self.limits.job_memory_mb << 20
        require(self.api.SetInformationJobObject(job, 9, c.byref(limits), c.sizeof(limits)), "job limits")
        restrictions = w.DWORD(JOB_UI_RESTRICTIONS_ALL)
        require(self.api.SetInformationJobObject(job, 4, c.byref(restrictions), c.sizeof(restrictions)),
                "job user-interface restrictions")
        return job

    def _output(self, path):
        """A host-created log file whose handle is the only one the launch inherits."""
        inheritable = SecurityAttributes(c.sizeof(SecurityAttributes), None, True)
        return require(self.api.CreateFileW(str(path), GENERIC_WRITE, 0x1, c.byref(inheritable),
                                            CREATE_ALWAYS, 0, None), "launch output file")

    def _attributes(self):
        """The attribute list that fixes the job, the token and the inherited handles at creation."""
        self._capabilities = self._capability_block()
        listed = (self.output, *self.passed_handles)
        self._handles = (w.HANDLE * len(listed))(*listed)
        self._jobs = (w.HANDLE * 1)(self.job)
        entries = [(ATTRIBUTE_JOB_LIST, self._jobs), (ATTRIBUTE_SECURITY_CAPABILITIES, self._capabilities),
                   (ATTRIBUTE_HANDLE_LIST, self._handles)]
        if self.boundary.lpac:
            self._policy = w.DWORD(1)
            entries.append((ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY, self._policy))
        return self._attribute_list(entries)

    def _capability_block(self):
        """`SECURITY_CAPABILITIES` naming the package SID and each capability SID."""
        sids = [capability_sid(self.api, name) for name in self.boundary.capabilities]
        self._capability_array = (SidAndAttributes * max(1, len(sids)))()
        for index, sid in enumerate(sids):
            self._capability_array[index] = SidAndAttributes(sid, SE_GROUP_ENABLED)
        pointer = c.cast(self._capability_array, c.c_void_p) if sids else None
        return SecurityCapabilities(self.profile.sid, pointer, len(sids), 0)

    def _attribute_list(self, entries):
        """Allocate and fill a process attribute list."""
        size = c.c_size_t()
        self.api.InitializeProcThreadAttributeList(None, len(entries), 0, c.byref(size))
        attributes = c.create_string_buffer(size.value)
        require(self.api.InitializeProcThreadAttributeList(attributes, len(entries), 0, c.byref(size)),
                "attribute list")
        for attribute, value in entries:
            require(self.api.UpdateProcThreadAttribute(attributes, 0, attribute, c.byref(value),
                                                       c.sizeof(value), None, None), "process attribute")
        return attributes

    def _create(self, argv, cwd, environment, attributes):
        """Start the process suspended with an explicit environment, desktop and output handle."""
        startup = StartupEx()
        startup.startup.cb = c.sizeof(startup)
        startup.startup.flags = 0x100
        startup.startup.stdout = startup.startup.stderr = self.output
        startup.startup.desktop = self.desktop_name
        startup.attributes = c.cast(attributes, c.c_void_p)
        command = c.create_unicode_buffer(subprocess.list2cmdline([str(value) for value in argv]))
        info = ProcessInfo()
        require(self.api.CreateProcessW(str(argv[0]), command, None, None, True, CREATION_FLAGS,
                                        environment_block(environment), str(cwd), c.byref(startup),
                                        c.byref(info)), "CreateProcessW")
        self.process, self.thread, self.pid = info.process, info.thread, info.pid

    def _verify(self):
        """Confirm, before the first instruction runs, that the OS applied the whole boundary."""
        inside = w.BOOL()
        require(self.api.IsProcessInJob(self.process, self.job, c.byref(inside)), "IsProcessInJob")
        token = process_token(self.api, self.process)
        try:
            facts = {"in_job": bool(inside.value),
                     "appcontainer": token_flag(self.api, token, TOKEN_IS_APPCONTAINER),
                     "package": token_sid(self.api, token, TOKEN_APPCONTAINER_SID) == self.profile.sid_text,
                     "lpac": token_is_lpac(self.api, token) == self.boundary.lpac}
        finally:
            self.api.CloseHandle(token)
        refuse_unless_contained(facts)

    def wait(self, seconds):
        """The exit code, or None when the process is still running after `seconds`."""
        if self.api.WaitForSingleObject(self.process, int(seconds * 1000)) != 0:
            return None
        code = w.DWORD()
        require(self.api.GetExitCodeProcess(self.process, c.byref(code)), "GetExitCodeProcess")
        return code.value

    def terminate(self):
        """Kill every process in the job and report whether the job became empty."""
        if not self.job:
            return True
        self.api.TerminateJobObject(self.job, 1)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self._active() == 0:
                return True
            time.sleep(0.02)
        return False

    def _active(self):
        """How many processes the job still holds, or None when the query fails."""
        accounting = Accounting()
        if not self.api.QueryInformationJobObject(self.job, 1, c.byref(accounting), c.sizeof(accounting), None):
            return None
        return accounting.active

    def close(self):
        """Release every handle; closing the job also kills anything still inside it."""
        for handle in (self.thread, self.process, self.output, self.job):
            if handle:
                self.api.CloseHandle(handle)
        self.job = self.process = self.thread = self.output = None


def refuse_unless_contained(facts):
    """Raise unless every boundary fact the OS reported is true; there is no partial launch."""
    missing = sorted(name for name, held in facts.items() if not held)
    if missing or not facts:
        raise RuntimeError(f"launch started outside its boundary: {', '.join(missing) or 'no facts'}")


def environment_block(values):
    """A Windows environment block holding exactly `values` and nothing inherited."""
    nul = chr(0)
    entries = [f"{name}={value}" for name, value in sorted(values.items())]
    return c.create_unicode_buffer(nul.join(entries) + nul + nul)


def run_tool(windows, name, arguments, timeout=120):
    """Run a Windows system tool with the host token; returns its exit code."""
    result = subprocess.run([str(Path(windows) / "System32" / name), *arguments], stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=timeout,
                            check=False)
    return result.returncode


def grant(windows, path, sid, rights, recursive=False):
    """Add one inheritable access entry for `sid` to a probe-owned directory."""
    arguments = [str(path), "/grant:r", f"*{sid}:(OI)(CI)({rights})", "/Q"]
    if run_tool(windows, "icacls.exe", arguments + (["/T"] if recursive else [])) != 0:
        raise RuntimeError(f"could not grant {rights} on {path}")


def label_low(windows, path):
    """Give a probe-owned directory a low integrity label so a contained process may write to it."""
    if run_tool(windows, "icacls.exe", [str(path), "/setintegritylevel", "(OI)(CI)L", "/Q"]) != 0:
        raise RuntimeError(f"could not label {path}")


class Guid(c.Structure):
    _fields_ = [("first", c.c_uint32), ("second", c.c_uint16), ("third", c.c_uint16), ("rest", c.c_ubyte * 8)]


def scheduler_connects():
    """Connect to the local Task Scheduler through COM with the host token.

    This is the positive control for the fixture's own scheduler check: the
    same connection, made where it is authorized, must succeed.
    """
    ole = c.WinDLL("ole32")
    ole.CoInitializeEx(None, 0)
    scheduler = Guid(0x0F87369F, 0xA4E5, 0x4CFC, (c.c_ubyte * 8)(0xBD, 0x3E, 0x73, 0xE6, 0x15, 0x45, 0x72, 0xDD))
    interface = Guid(0x2FABA4C7, 0x4DA9, 0x4013, (c.c_ubyte * 8)(0x96, 0x97, 0x20, 0xCC, 0x3F, 0xD4, 0x0F, 0x85))
    service = c.c_void_p()
    if ole.CoCreateInstance(c.byref(scheduler), None, 1, c.byref(interface), c.byref(service)) != 0:
        return False
    return _scheduler_connect(service) == 0


def _scheduler_connect(service):
    """Call `ITaskService::Connect` with four empty variants, then release the object."""
    table = c.cast(c.cast(service, c.POINTER(c.c_void_p))[0], c.POINTER(c.c_void_p))
    pointer = c.c_void_p
    connect = c.WINFUNCTYPE(c.c_long, pointer, pointer, pointer, pointer, pointer)(table[10])
    empty = (c.c_uint64 * 3)()
    result = connect(service, c.byref(empty), c.byref(empty), c.byref(empty), c.byref(empty))
    c.WINFUNCTYPE(c.c_ulong, pointer)(table[2])(service)
    return result


class PathRefused(Exception):
    """A worker-written path was not a plain single-link file inside its launch root."""


def validated_read(api, root, relative, limit=1 << 20):
    """Read a worker-written file by handle without following any link.

    The file is opened once with its final reparse point left unresolved. Every
    check then uses that handle, so a rename or junction swap after the open
    cannot change what is read, and a link in a parent directory is caught
    because the handle's real path leaves the launch root.
    """
    handle = _open_unresolved(api, Path(root) / relative)
    try:
        _refuse_links(api, handle, directory=False)
        _refuse_outside(api, handle, root)
        return _read(api, handle, limit)
    finally:
        api.CloseHandle(handle)


def _open_unresolved(api, path):
    """Open for reading without resolving a final reparse point."""
    handle = api.CreateFileW(str(path), GENERIC_READ, SHARE_ALL, None, OPEN_EXISTING,
                             OPEN_REPARSE_POINT | BACKUP_SEMANTICS, None)
    if not handle or handle == INVALID_HANDLE:
        raise PathRefused(f"open failed with Windows error {c.get_last_error()}")
    return handle


def _refuse_links(api, handle, directory):
    """Refuse reparse points, the wrong object kind and files with more than one name."""
    information = FileInformation()
    require(api.GetFileInformationByHandle(handle, c.byref(information)), "GetFileInformationByHandle")
    if information.attributes & ATTRIBUTE_REPARSE_POINT:
        raise PathRefused("reparse point")
    if bool(information.attributes & ATTRIBUTE_DIRECTORY) != directory:
        raise PathRefused("unexpected object kind")
    if not directory and information.links != 1:
        raise PathRefused(f"{information.links} hard links")


def _final_path(api, handle):
    """The real path of an open handle, as Windows resolves it."""
    buffer = c.create_unicode_buffer(1024)
    length = api.GetFinalPathNameByHandleW(handle, buffer, len(buffer), 0)
    if length == 0 or length >= len(buffer):
        raise PathRefused("final path unavailable")
    return buffer.value.casefold()


def _refuse_outside(api, handle, root):
    """Refuse a handle whose real path is not inside the real path of `root`."""
    root_handle = _open_unresolved(api, Path(root))
    try:
        _refuse_links(api, root_handle, directory=True)
        prefix = _final_path(api, root_handle) + "\\"
    finally:
        api.CloseHandle(root_handle)
    if not _final_path(api, handle).startswith(prefix):
        raise PathRefused("outside the launch root")


def _read(api, handle, limit):
    """Read the whole file through the validated handle, refusing oversized content."""
    buffer, count = c.create_string_buffer(limit + 1), w.DWORD()
    require(api.ReadFile(handle, buffer, limit + 1, c.byref(count), None), "ReadFile")
    if count.value > limit:
        raise PathRefused("larger than the read limit")
    return buffer.raw[:count.value]


def create_pipe(api, name, descriptor=None, first=True):
    """One listening instance of a local-only, byte-mode named pipe."""
    mode = PIPE_DUPLEX | (PIPE_FIRST_INSTANCE if first else 0)
    attributes = descriptor.reference() if descriptor else None
    return require(api.CreateNamedPipeW(name, mode, PIPE_REJECT_REMOTE, 8, 4096, 4096, 0, attributes),
                   "CreateNamedPipeW")


def open_pipe(path):
    """Connect to a pipe as a client with the host token; the caller closes the file."""
    return open(path, "r+b", buffering=0)


class Broker(threading.Thread):
    """Stub of an authenticated broker endpoint serving exactly one launch.

    Its access list names one package SID. It answers each request with the
    package SID the OS reports for the connected client, and records the name
    the client claimed, so a forged claim is visible and never trusted.
    """

    def __init__(self, api, name, owner_sid, package_sid):
        super().__init__(daemon=True)
        self.api, self.path = api, "\\\\.\\pipe\\" + name
        self.pipe = create_pipe(api, self.path, launch_descriptor(api, owner_sid, package_sid, "GRGW"))
        self.exchanges = []

    def run(self):
        """Serve one client at a time until the host sends its stop message."""
        while self._serve_one():
            self.api.DisconnectNamedPipe(self.pipe)
        self.api.CloseHandle(self.pipe)

    def _serve_one(self):
        """Handle one connection; returns False for the host's stop message."""
        connected = self.api.ConnectNamedPipe(self.pipe, None)
        if not connected and c.get_last_error() != ERROR_PIPE_CONNECTED:
            return False
        claimed = self._receive()
        identity = self._client_package()
        if identity is None and claimed == "stop":
            return False
        self.exchanges.append({"claimed": claimed, "os_package_sid": identity})
        self._send(f"sid={identity}")
        return True

    def _receive(self):
        """The client's request line, without trusting its content."""
        buffer, count = c.create_string_buffer(256), w.DWORD()
        if not self.api.ReadFile(self.pipe, buffer, 256, c.byref(count), None):
            return ""
        return buffer.raw[:count.value].decode("utf-8", "replace").strip()

    def _client_package(self):
        """The connected client's package SID, taken from its token by impersonation."""
        if not self.api.ImpersonateNamedPipeClient(self.pipe):
            return None
        token = w.HANDLE()
        opened = self.api.OpenThreadToken(self.api.GetCurrentThread(), TOKEN_QUERY, True, c.byref(token))
        self.api.RevertToSelf()
        if not opened:
            return None
        try:
            return token_sid(self.api, token, TOKEN_APPCONTAINER_SID)
        finally:
            self.api.CloseHandle(token)

    def _send(self, text):
        """Write the reply and wait until the client has read it."""
        data, count = text.encode("utf-8"), w.DWORD()
        self.api.WriteFile(self.pipe, data, len(data), c.byref(count), None)
        self.api.FlushFileBuffers(self.pipe)

    def stop(self):
        """Unblock the server with a host-token request it recognises as stop."""
        try:
            with open_pipe(self.path) as client:
                client.write(b"stop\n")
        except OSError:
            pass
        self.join(timeout=5)


class Listener(threading.Thread):
    """Accepts on one local address and records every peer that reaches it."""

    def __init__(self, family, kind, host):
        super().__init__(daemon=True)
        self.kind, self.received = kind, []
        self.socket = socket.socket(family, kind)
        self.socket.bind((host, 0))
        self.socket.settimeout(0.2)
        if kind == socket.SOCK_STREAM:
            self.socket.listen(8)
        self.address = self.socket.getsockname()
        self._stopping = threading.Event()

    def run(self):
        """Record arrivals until asked to stop."""
        while not self._stopping.is_set():
            try:
                self.received.append(self._arrival())
            except socket.timeout:
                continue
            except OSError:
                return

    def _arrival(self):
        """Wait for one connection or datagram and return who sent it."""
        if self.kind == socket.SOCK_STREAM:
            connection, peer = self.socket.accept()
            connection.close()
            return str(peer[1])
        return str(self.socket.recvfrom(64)[1][1])

    def endpoint(self):
        """`address:port` text in the form Rust's `SocketAddr` parses."""
        host, port = self.address[0], self.address[1]
        return f"[{host}]:{port}" if ":" in host else f"{host}:{port}"

    def close(self):
        """Stop accepting and release the socket."""
        self._stopping.set()
        self.join(timeout=2)
        self.socket.close()
