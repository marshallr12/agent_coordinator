#!/usr/bin/env python3
"""Install an exact, verified Agent Coordinator CLI while keeping a rollback copy."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile


def fail(message: str) -> "NoReturn":
    raise SystemExit(message)


def target_name() -> str:
    operating_system = {"Linux": "linux", "Windows": "windows", "Darwin": "macos"}.get(platform.system())
    architecture = {"x86_64": "x86_64", "AMD64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}.get(platform.machine())
    if operating_system is None or architecture is None:
        fail(f"unsupported workstation target: {platform.system()} {platform.machine()}")
    return f"{operating_system}-{architecture}"


def client_info(executable: Path) -> dict:
    result = subprocess.run([str(executable), "client-info", "--json"], check=False, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode != 0:
        fail("candidate client does not support machine-readable client-info; obtain the exact compatible build")
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError:
        fail("candidate client returned invalid client-info JSON")
    return value.get("data", value)


def service_contract(path: Path) -> tuple[str, str, set[str], set[str]]:
    try:
        body = json.loads(path.read_text(encoding="utf-8"))
        data = body.get("data", body)
        contract = data["client_compatibility"]
        compatible = contract["compatible_client"]
        commit = compatible["source_commit"]
        repository = compatible["source_repository"]
        targets = set(compatible["supported_targets"])
        capabilities = set(contract["required_capabilities"])
    except (OSError, KeyError, TypeError, json.JSONDecodeError):
        fail("service info file has no valid client_compatibility contract")
    if not isinstance(commit, str) or len(commit) != 40 or any(char not in "0123456789abcdef" for char in commit.lower()):
        fail("service compatibility contract has no exact source commit")
    if not isinstance(repository, str) or not repository.startswith("https://") or "@" in repository:
        fail("service compatibility contract has no trusted HTTPS source repository")
    return commit.lower(), repository.rstrip("/").removesuffix(".git"), targets, capabilities


def verify_candidate(executable: Path, commit: str, repository: str, target: str, capabilities: set[str]) -> None:
    if executable.is_symlink() or not executable.is_file():
        fail(f"candidate must be a regular executable file: {executable}")
    info = client_info(executable)
    build = info.get("build", {})
    observed_target = f"{build.get('target_os')}-{build.get('target_arch')}"
    if build.get("source_commit") != commit:
        fail("candidate source commit does not match the service's exact compatible source commit")
    candidate_repository = str(build.get("source_repository", "")).rstrip("/").removesuffix(".git")
    if candidate_repository != repository:
        fail("candidate source repository does not match the trusted service source")
    if observed_target != target:
        fail(f"candidate target {observed_target} does not match workstation target {target}")
    if build.get("dirty") is not False:
        fail("candidate build is dirty or lacks build provenance")
    advertised = set(info.get("capabilities", []))
    if not capabilities.issubset(advertised):
        fail("candidate does not advertise every service-required client capability")


def build_from_source(source: Path, commit: str, repository: str) -> Path:
    git = shutil.which("git")
    cargo = shutil.which("cargo")
    if git is None or cargo is None:
        fail("source build requires Git and Cargo; keep the current client and report this workstation blocker")
    observed = subprocess.run([git, "-C", str(source), "rev-parse", "HEAD"], check=False,
                              text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    dirty = subprocess.run([git, "-C", str(source), "status", "--porcelain"], check=False,
                           text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if observed.returncode != 0 or observed.stdout.strip().lower() != commit:
        fail("source checkout is not at the exact commit named by service discovery")
    if dirty.returncode != 0 or dirty.stdout.strip():
        fail("source checkout has local changes; use a clean checkout of the exact compatible commit")
    origin = subprocess.run([git, "-C", str(source), "remote", "get-url", "origin"], check=False,
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    remote = origin.stdout.strip()
    if remote.startswith("git@github.com:"):
        remote = "https://github.com/" + remote.removeprefix("git@github.com:")
    elif remote.startswith("https://") and "@" in remote:
        remote = "https://" + remote.rsplit("@", 1)[1]
    remote = remote.rstrip("/").removesuffix(".git")
    if origin.returncode != 0 or remote != repository:
        fail("source checkout origin does not match the service's trusted compatible source repository")
    target_directory = Path(tempfile.mkdtemp(prefix="agent-coordinator-client-build-"))
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = str(target_directory)
    environment["COORDINATOR_BUILD_COMMIT"] = commit
    environment["COORDINATOR_BUILD_DIRTY"] = "false"
    result = subprocess.run([cargo, "build", "--release", "--locked", "-p", "coordinator-cli"],
                           cwd=source, env=environment, check=False)
    if result.returncode != 0:
        shutil.rmtree(target_directory, ignore_errors=True)
        fail("locked source build failed; keep the current client and report the concrete build error")
    executable_name = "agent-coordinator.exe" if os.name == "nt" else "agent-coordinator"
    return target_directory / "release" / executable_name


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def install(active: Path, candidate: Path, rollback: Path, commit: str) -> None:
    if active.is_symlink() or not active.is_file():
        fail(f"installed client must be a regular file: {active}")
    try:
        installed = client_info(active)
    except SystemExit:
        installed = {}
    installed_build = installed.get("build", {})
    candidate_info = client_info(candidate)
    candidate_build = candidate_info.get("build", {})
    if installed_build == candidate_build and installed.get("capabilities") == candidate_info.get("capabilities"):
        print(f"The compatible client is already installed; rollback remains at {rollback}.")
        return
    if rollback.is_symlink() or (rollback.exists() and not rollback.is_file()):
        fail(f"rollback path must be a regular file, not a link or directory: {rollback}")
    if not rollback.exists():
        shutil.copy2(active, rollback)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{active.name}.", dir=active.parent)
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        shutil.copy2(candidate, temporary)
        if os.name != "nt":
            temporary.chmod(temporary.stat().st_mode | 0o111)
        os.replace(temporary, active)
        try:
            verified = client_info(active)
            valid = verified.get("build") == candidate_build and verified.get("capabilities") == candidate_info.get("capabilities")
        except SystemExit:
            valid = False
        if not valid:
            descriptor, restore_name = tempfile.mkstemp(prefix=f".{active.name}.rollback.", dir=active.parent)
            os.close(descriptor)
            restore = Path(restore_name)
            try:
                shutil.copy2(rollback, restore)
                os.replace(restore, active)
            finally:
                restore.unlink(missing_ok=True)
            fail(f"installed client verification failed; restored rollback binary at {rollback}")
    except OSError as error:
        fail(f"atomic client replacement failed without removing the current executable: {error}")
    finally:
        temporary.unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path, help="installed CLI executable")
    parser.add_argument("--service-info", required=True, type=Path, help="saved anonymous /api/v1/info JSON")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--candidate", type=Path, help="downloaded candidate executable")
    source.add_argument("--source-root", type=Path, help="clean checkout at the exact compatible source commit")
    parser.add_argument("--sha256", help="required SHA-256 for a downloaded --candidate")
    args = parser.parse_args()

    target = target_name()
    commit, repository, targets, capabilities = service_contract(args.service_info)
    if target not in targets:
        fail(f"service has no verified compatible client for {target}; do not install another architecture")
    candidate = args.candidate
    build_directory = None
    if args.source_root is not None:
        candidate = build_from_source(args.source_root.resolve(), commit, repository)
        build_directory = candidate.parents[1]
    elif args.sha256 is None or len(args.sha256) != 64 or any(char not in "0123456789abcdef" for char in args.sha256.lower()):
        fail("a downloaded --candidate requires its 64-character release SHA-256")
    elif sha256(candidate) != args.sha256.lower():
        fail("candidate SHA-256 mismatch; the installed client was left unchanged")
    try:
        if args.binary.is_symlink():
            fail("installed client path must not be a symbolic link")
        verify_candidate(candidate, commit, repository, target, capabilities)
        active = args.binary.resolve()
        rollback = active.with_name(active.name + ".rollback")
        install(active, candidate, rollback, commit)
    finally:
        if build_directory is not None:
            shutil.rmtree(build_directory, ignore_errors=True)
    print(f"Installed and verified {commit} for {target}; rollback executable: {rollback}")


if __name__ == "__main__":
    main()
