#!/usr/bin/env python3
"""Renew one explicitly configured attempt and verify live ownership after every renewal.

The helper only runs `renew`, `retry` and `tasks show` through the native client. It
never claims, releases or generates a mutation key: a failed renewal gets one exact
`retry` of the client's saved mutation. A renewal receipt is never authority; only a
fresh `tasks show` that matches the claim configuration and shows an unexpired lease is.
A failed fresh read is retried read-only, but never past the last lease deadline that a
fresh read verified. Output never includes client stdout/stderr or the claim file.
"""

import argparse
import datetime
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
from typing import Any, Callable, NamedTuple

RENEW_TIMEOUT = 120.0
READ_TIMEOUT = 60.0
DEFAULT_RENEW_AFTER = 900.0
SAFE_CODE = re.compile(r"[A-Za-z0-9_.-]{1,64}")


class Stop(Exception):
    """A terminal condition: no further ownership-dependent work is safe."""


class ReadFailure(Exception):
    """A fresh ownership read produced no usable answer; the message is sanitized."""


class Claim(NamedTuple):
    attempt: str
    generation: int
    task: str
    session_id: str
    renew_after: float


class Result(NamedTuple):
    ok: bool
    body: dict[str, Any]
    problem: str


class Lease(NamedTuple):
    expires: datetime.datetime
    deadline: float  # local monotonic instant at which the verified lease ends


def load_claim(path: Path) -> Claim:
    """Read the external claim file; errors name a field, never a value."""
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        raise ValueError("claim config is unreadable or not JSON") from None
    if not isinstance(value, dict):
        raise ValueError("claim config must be a JSON object")
    for field in ("attempt", "task", "session_id"):
        if not isinstance(value.get(field), str) or not value[field]:
            raise ValueError(f"claim config field {field} must be a non-empty string")
    generation = value.get("generation")
    if isinstance(generation, bool) or not isinstance(generation, int):
        raise ValueError("claim config field generation must be an integer")
    renew_after = value.get("renew_after_seconds", DEFAULT_RENEW_AFTER)
    if isinstance(renew_after, bool) or not isinstance(renew_after, (int, float)) or renew_after <= 0:
        raise ValueError("claim config field renew_after_seconds must be a positive number")
    return Claim(value["attempt"], generation, value["task"], value["session_id"], float(renew_after))


def timestamp(value: Any) -> datetime.datetime | None:
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return parsed if parsed.tzinfo else None


def error_code(body: dict[str, Any]) -> str:
    error = body.get("error")
    code = error.get("code") if isinstance(error, dict) else None
    return code if isinstance(code, str) and SAFE_CODE.fullmatch(code) else "unavailable"


class Cli:
    """The native client bound to one repository and one existing session."""

    def __init__(self, prefix: list[str], run: Callable[..., Any] = subprocess.run):
        self.prefix = prefix
        self.run = run

    def call(self, args: list[str], timeout: float) -> Result:
        try:
            done = self.run(self.prefix + args + ["--json"], capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            return Result(False, {}, "client timed out")
        except OSError:
            return Result(False, {}, "client could not be started")
        try:
            body = json.loads(done.stdout)
        except (ValueError, TypeError):
            body = None
        parsed = isinstance(body, dict)
        body = body if parsed else {}
        if done.returncode:
            return Result(False, body, f"client exit={done.returncode} code={error_code(body)}")
        if not parsed:
            return Result(False, body, "client output was not a JSON object")
        return Result(True, body, "")


class Renewer:
    def __init__(
        self,
        cli: Cli,
        claim: Claim,
        *,
        min_remaining: float = 960.0,
        retry_delay: float = 2.0,
        retry_max_delay: float = 30.0,
        startup_seconds: float = 120.0,
        clock: Callable[[], float] = time.monotonic,
        sleep: Callable[[float], None] = time.sleep,
        log: Callable[[str], None] | None = None,
        alert: Callable[[str], None] | None = None,
    ):
        self.cli = cli
        self.claim = claim
        self.min_remaining = min_remaining
        self.retry_delay = retry_delay
        self.retry_max_delay = retry_max_delay
        self.startup_seconds = startup_seconds
        self.clock = clock
        self.sleep = sleep
        self.emit = log or self.print_line
        self.alert = alert or self.print_alert

    @staticmethod
    def stamp() -> str:
        return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")

    def print_line(self, message: str) -> None:
        print(self.stamp(), message, flush=True)

    def print_alert(self, message: str) -> None:
        print(self.stamp(), message, file=sys.stderr, flush=True)

    def check(self, body: dict[str, Any], started: float, minimum: float, previous: Lease | None) -> Lease:
        """Judge one fresh `tasks show` answer: ReadFailure if unusable, Stop if ownership is lost."""
        data = body.get("data")
        server_now = timestamp(body.get("server_time"))
        attempts = data.get("attempts") if isinstance(data, dict) else None
        if server_now is None or not isinstance(attempts, list):
            raise ReadFailure("task answer was malformed")
        attempt = next((a for a in attempts if isinstance(a, dict) and a.get("id") == self.claim.attempt), None)
        if attempt is None:
            raise Stop("ownership changed: the attempt is no longer listed on the task")
        claim = self.claim
        for field, held in (
            ("current attempt", data.get("current_attempt_id") == claim.attempt),
            ("task generation", data.get("generation") == claim.generation),
            ("attempt generation", attempt.get("generation") == claim.generation),
            ("session", attempt.get("session_id") == claim.session_id),
            ("attempt state", attempt.get("state") == "active"),
        ):
            if not held:
                raise Stop(f"ownership changed: {field} no longer matches the claim")
        expires = timestamp(attempt.get("expires_at"))
        if expires is None:
            raise ReadFailure("attempt expiry was missing or malformed")
        remaining = (expires - server_now).total_seconds()
        if remaining <= 0:
            raise Stop("lease expired")
        if remaining < minimum:
            raise Stop(f"lease has {remaining:.0f}s left, below the required {minimum:.0f}s; renewal did not take effect")
        if previous is not None and expires < previous.expires:
            raise Stop("lease expiry moved backwards")
        return Lease(expires, started + remaining)

    def read_ownership(self, deadline: float, minimum: float, previous: Lease | None) -> Lease:
        """Fresh read with read-only retries, never attempted at or after `deadline`."""
        delay = self.retry_delay
        failures = 0
        problem = ""
        while True:
            started = self.clock()
            left = deadline - started
            if left <= 0:
                raise Stop(self.unverified(failures, problem))
            result = self.cli.call(["tasks", "show", "--id", self.claim.task], min(READ_TIMEOUT, left))
            try:
                if not result.ok:
                    raise ReadFailure(result.problem)
                return self.check(result.body, started, minimum, previous)
            except ReadFailure as failure:
                problem = str(failure)
            failures += 1
            left = deadline - self.clock()
            wait = min(delay, left)
            if wait >= left:
                raise Stop(self.unverified(failures, problem))
            self.emit(
                f"fresh ownership read failed ({problem}); read-only retry {failures} in {wait:.0f}s,"
                f" verified lease ends in {left:.0f}s"
            )
            self.sleep(wait)
            delay = min(delay * 2, self.retry_max_delay)

    @staticmethod
    def unverified(failures: int, problem: str) -> str:
        if not failures:
            return "lease expired before a fresh ownership read could be made"
        return (
            f"no fresh ownership read succeeded within the last verified lease after {failures} failed"
            f" read(s); last failure: {problem}"
        )

    def renew(self) -> str:
        """Renew once; a failure gets one exact retry of the client's saved mutation."""
        claim = self.claim
        result = self.cli.call(["renew", "--attempt", claim.attempt, "--generation", str(claim.generation)], RENEW_TIMEOUT)
        if result.ok:
            return "renew"
        self.emit(f"renew failed ({result.problem}); retrying exact saved mutation once")
        if not self.cli.call(["retry"], RENEW_TIMEOUT).ok:
            raise Stop("exact retry failed; preserve the journal and inspect before any further mutation")
        return "retry"

    def verified(self, effect: str, lease: Lease) -> None:
        stamp = lease.expires.isoformat().replace("+00:00", "Z")
        self.emit(f"{effect} verified active attempt={self.claim.attempt} generation={self.claim.generation} expires={stamp}")

    def run(self, max_cycles: int | None = None) -> int:
        try:
            lease = self.read_ownership(self.clock() + self.startup_seconds, 0.0, None)
            self.verified("startup", lease)
            cycles = 0
            while max_cycles is None or cycles < max_cycles:
                if cycles:
                    self.sleep(self.claim.renew_after)
                if self.clock() >= lease.deadline:
                    raise Stop("verified lease expired before the next renewal; not renewing")
                effect = self.renew()
                lease = self.read_ownership(lease.deadline, self.min_remaining, lease)
                self.verified(effect, lease)
                cycles += 1
        except Stop as stop:
            self.stopped(str(stop))
            return 1
        except Exception as error:  # noqa: BLE001 - the stop alert must always be emitted
            self.stopped(f"unexpected {type(error).__name__}")
            return 1
        return 0

    def stopped(self, reason: str) -> None:
        message = f"STOPPED: {reason}; inspect pilot and journal; no new claim attempted"
        self.emit(message)
        self.alert(message)


def parse(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Renew one explicitly configured attempt, verifying ownership after each renewal.")
    parser.add_argument("--cli", required=True, help="path to the agent-coordinator executable")
    parser.add_argument("--repo-config", required=True, help="repository binding passed to the client")
    parser.add_argument("--session", required=True, help="existing local harness-session name; never created or reclaimed")
    parser.add_argument("--claim-config", required=True, help="JSON file: attempt, generation, task, session_id, renew_after_seconds")
    parser.add_argument("--lock-file", help="exclusive producer lock (default: the claim config path plus .lock)")
    parser.add_argument("--min-remaining-seconds", type=float, default=960.0, help="stop if a verified renewal leaves less lease than this")
    parser.add_argument("--read-retry-seconds", type=float, default=2.0, help="first delay between read-only retries; doubles")
    parser.add_argument("--read-retry-max-seconds", type=float, default=30.0, help="longest delay between read-only retries")
    parser.add_argument("--startup-read-seconds", type=float, default=120.0, help="time allowed for the first ownership read")
    args = parser.parse_args(argv)
    for name in ("min_remaining_seconds", "read_retry_seconds", "read_retry_max_seconds", "startup_read_seconds"):
        if not getattr(args, name) > 0:
            parser.error(f"--{name.replace('_', '-')} must be positive")
    return args


def main(argv: list[str] | None = None) -> int:
    os.umask(0o077)
    args = parse(argv)
    claim_path = Path(args.claim_config)
    try:
        claim = load_claim(claim_path)
    except ValueError as error:
        print(f"pilot_renew: {error}", file=sys.stderr)
        return 2
    cli = shutil.which(args.cli)
    if cli is None:
        print("pilot_renew: --cli is not an executable file", file=sys.stderr)
        return 2
    lock = open(args.lock_file or f"{claim_path}.lock", "a")  # held for the process lifetime
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print("pilot_renew: another renewal helper already holds this producer lock", file=sys.stderr)
        return 2
    renewer = Renewer(
        Cli([cli, "--repo-config", args.repo_config, "--session", args.session]),
        claim,
        min_remaining=args.min_remaining_seconds,
        retry_delay=args.read_retry_seconds,
        retry_max_delay=args.read_retry_max_seconds,
        startup_seconds=args.startup_read_seconds,
    )
    return renewer.run()


if __name__ == "__main__":
    raise SystemExit(main())
