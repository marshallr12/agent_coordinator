#!/usr/bin/env python3
"""Inspect a Linux release archive and optionally exercise it under systemd in CI."""

import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import shutil
import socket
import ssl
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


MAX_ARCHIVE_BYTES = 256 * 1024 * 1024
MAX_FILES = 64
REQUIRED = {
    "AGENTS.md",
    "BACKLOG.md",
    "DURABLE-RECORD.md",
    "HANDOFF.md",
    "PLAN.md",
    "README.md",
    "SHA256SUMS",
    "bin/agent-coordinator",
    "bin/agent-coordinator-server",
    "deploy/Caddyfile.example",
    "deploy/agent-coordinator-backup.service",
    "deploy/agent-coordinator-backup.timer",
    "deploy/agent-coordinator.service",
    "deploy/service.env.example",
    "docs/CLI.md",
    "docs/backup-restore-guide.md",
    "docs/linux-installation.md",
}
MARKDOWN_LINK = re.compile(r"!?\[[^\]]*\]\((?:<([^>]+)>|([^\s)]+))")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(command: list[str], *, input_text: str | None = None, env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    result = subprocess.run(command, input=input_text, text=True, capture_output=True, env=env, timeout=300)
    if result.returncode != 0:
        raise AssertionError(f"Command failed with status {result.returncode}: {Path(command[0]).name}")
    return result


def verify_outer_checksum(package: Path) -> None:
    checksum = package.with_name(package.name + ".sha256")
    assert checksum.is_file() and not checksum.is_symlink(), "The adjacent package checksum is missing."
    fields = checksum.read_text(encoding="ascii").strip().split()
    assert len(fields) == 2 and fields[1] == package.name, "The package checksum record is malformed."
    assert fields[0] == sha256(package), "The package SHA-256 does not match."


def extract_checked(package: Path, destination: Path) -> Path:
    assert package.stat().st_size <= MAX_ARCHIVE_BYTES, "The release archive exceeds the inspection bound."
    total = 0
    root: str | None = None
    files: set[str] = set()
    with tarfile.open(package, "r:gz") as archive:
        members = archive.getmembers()
        assert len(members) <= MAX_FILES, "The release archive contains too many entries."
        for member in members:
            path = PurePosixPath(member.name)
            assert not path.is_absolute() and ".." not in path.parts and len(path.parts) >= 1
            assert member.isfile() or member.isdir(), "Release archives may contain only files and directories."
            assert not member.issym() and not member.islnk(), "Release archives may not contain links."
            root = root or path.parts[0]
            assert path.parts[0] == root and root.startswith("agent-coordinator-")
            relative = PurePosixPath(*path.parts[1:])
            target = destination.joinpath(*path.parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                target.chmod(0o755)
                continue
            assert str(relative) not in files, "The release archive contains a duplicate file."
            files.add(str(relative))
            total += member.size
            assert total <= MAX_ARCHIVE_BYTES, "Expanded package content exceeds the inspection bound."
            target.parent.mkdir(parents=True, exist_ok=True)
            source = archive.extractfile(member)
            assert source is not None
            with target.open("xb") as output:
                shutil.copyfileobj(source, output, 1024 * 1024)
            target.chmod(member.mode & 0o777)
    assert root is not None and REQUIRED <= files, f"Required package files are missing: {sorted(REQUIRED - files)}"
    assert all(
        (name.endswith(".md") and "/" not in name)
        or name == "SHA256SUMS"
        or (name.startswith("docs/") and name.endswith(".md") and name.count("/") == 1)
        or (name.startswith("deploy/") and name.count("/") == 1)
        or name in {"bin/agent-coordinator", "bin/agent-coordinator-server"}
        for name in files
    ), "The package contains an unexpected path."
    return destination / root


def verify_internal(root: Path) -> None:
    expected: dict[str, str] = {}
    for line in (root / "SHA256SUMS").read_text(encoding="ascii").splitlines():
        fields = line.split()
        assert len(fields) == 2 and len(fields[0]) == 64
        name = fields[1]
        path = PurePosixPath(name)
        assert not path.is_absolute() and ".." not in path.parts and name not in expected
        expected[name] = fields[0]
    actual = {
        str(path.relative_to(root)).replace(os.sep, "/")
        for path in root.rglob("*")
        if path.is_file() and path.name != "SHA256SUMS"
    }
    assert set(expected) == actual, "The internal checksum manifest does not cover the package exactly."
    for name, digest in expected.items():
        assert sha256(root.joinpath(*PurePosixPath(name).parts)) == digest, f"Checksum mismatch for {name}."
    assert os.access(root / "bin/agent-coordinator", os.X_OK)
    assert os.access(root / "bin/agent-coordinator-server", os.X_OK)
    assert (root / "deploy/service.env.example").stat().st_mode & 0o777 == 0o600
    service = (root / "deploy/agent-coordinator.service").read_text()
    backup = (root / "deploy/agent-coordinator-backup.service").read_text()
    timer = (root / "deploy/agent-coordinator-backup.timer").read_text()
    for setting in ["User=agent-coordinator", "UMask=0077", "NoNewPrivileges=true", "ProtectSystem=strict"]:
        assert setting in service, f"Service unit is missing {setting}."
    assert "PrivateNetwork=true" in backup and "Persistent=true" in timer and "OnCalendar=" in timer
    run([str(root / "bin/agent-coordinator"), "--version"])
    run([str(root / "bin/agent-coordinator-server"), "--version"])


def verify_markdown_links(root: Path) -> None:
    package_root = root.resolve()
    for source in root.rglob("*.md"):
        for match in MARKDOWN_LINK.finditer(source.read_text(encoding="utf-8")):
            raw = match.group(1) or match.group(2)
            parsed = urllib.parse.urlsplit(raw)
            if parsed.scheme or parsed.netloc or not parsed.path or parsed.path.startswith("/"):
                continue
            target = (source.parent / urllib.parse.unquote(parsed.path)).resolve()
            try:
                target.relative_to(package_root)
            except ValueError as error:
                raise AssertionError(f"Local Markdown link escapes the package: {source.relative_to(root)}") from error
            assert target.is_file(), f"Local Markdown link is missing from the package: {source.relative_to(root)}"


def unused_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class Browser:
    def __init__(self, origin: str, ca: Path):
        self.origin = origin
        self.csrf = ""
        context = ssl.create_default_context(cafile=str(ca))
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()),
            urllib.request.HTTPSHandler(context=context),
        )

    def call(self, path: str, body: dict | None = None) -> dict:
        request = urllib.request.Request(
            self.origin + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={
                "Content-Type": "application/json",
                "Origin": self.origin,
                "X-CSRF-Token": self.csrf,
                "Idempotency-Key": str(uuid.uuid4()),
            },
        )
        with self.opener.open(request, timeout=20) as response:
            return json.load(response)["data"]

    def login(self, password: str) -> None:
        self.csrf = self.call("/api/v1/auth/login", {"username": "package-admin", "password": password})["csrf_token"]


def wait_for(path: Path | None, url: str | None = None, context: ssl.SSLContext | None = None) -> None:
    for _ in range(200):
        if path is not None and path.is_file():
            return
        if url is not None:
            try:
                with urllib.request.urlopen(url, context=context, timeout=1):
                    return
            except (urllib.error.URLError, TimeoutError):
                pass
        time.sleep(0.1)
    raise AssertionError("The disposable service did not become ready.")


def ubuntu_2404() -> bool:
    values = {}
    for line in Path("/etc/os-release").read_text().splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value.strip('"')
    return values.get("ID") == "ubuntu" and values.get("VERSION_ID") == "24.04"


def systemd_acceptance(root: Path, caddy_source: Path) -> None:
    assert os.environ.get("CI") == "true", "The systemd exercise is restricted to disposable CI runners."
    assert os.geteuid() == 0 and Path("/run/systemd/system").is_dir(), "Run the CI systemd exercise as root under systemd."
    assert ubuntu_2404(), "The systemd exercise requires the Ubuntu 24.04 acceptance baseline."
    assert caddy_source.is_file() and not caddy_source.is_symlink()
    suffix = uuid.uuid4().hex[:10]
    name = f"acpkg{suffix}"
    service_unit = f"{name}.service"
    caddy_unit = f"{name}-caddy.service"
    data = Path("/var/lib") / name
    config = Path("/etc") / name
    install = Path("/opt") / name
    unit_dir = Path("/etc/systemd/system")
    service_path = unit_dir / service_unit
    caddy_path = unit_dir / caddy_unit
    server_port, https_port = unused_port(), unused_port()
    while https_port == server_port:
        https_port = unused_port()
    origin = f"https://localhost:{https_port}"
    created_user = False
    try:
        run(["useradd", "--system", "--user-group", "--home-dir", str(data), "--shell", "/usr/sbin/nologin", name])
        created_user = True
        for directory in [data, config, install, data / "caddy-data", data / "caddy-config"]:
            directory.mkdir(parents=True, exist_ok=True)
        shutil.copy2(root / "bin/agent-coordinator-server", install / "agent-coordinator-server")
        shutil.copy2(root / "bin/agent-coordinator", install / "agent-coordinator")
        shutil.copy2(caddy_source, install / "caddy")
        for binary in [install / "agent-coordinator-server", install / "agent-coordinator", install / "caddy"]:
            binary.chmod(0o755)
        environment = config / "service.env"
        environment.write_text(
            f"COORDINATOR_DATABASE={data}/coordinator.sqlite3\n"
            f"COORDINATOR_LISTEN=127.0.0.1:{server_port}\n"
            f"COORDINATOR_PUBLIC_ORIGIN={origin}\n"
            "COORDINATOR_ALLOW_INSECURE_LOOPBACK=false\n"
        )
        environment.chmod(0o600)
        caddyfile = config / "Caddyfile"
        caddyfile.write_text(
            "{\n    admin off\n    skip_install_trust\n}\n"
            f"{origin} {{\n    tls internal\n    request_body {{\n        max_size 256KiB\n    }}\n"
            f"    reverse_proxy 127.0.0.1:{server_port}\n}}\n"
        )
        caddyfile.chmod(0o600)
        run(["chown", "-R", f"{name}:{name}", str(data), str(config)])
        password = secrets.token_hex(24)
        run([
            "runuser", "-u", name, "--", str(install / "agent-coordinator-server"),
            "--database", str(data / "coordinator.sqlite3"), "--listen", f"127.0.0.1:{server_port}",
            "--public-origin", origin, "init-admin", "--username", "package-admin", "--password-stdin",
        ], input_text=password + "\n")
        production = (root / "deploy/agent-coordinator.service").read_text()
        production = production.replace("User=agent-coordinator", f"User={name}")
        production = production.replace("Group=agent-coordinator", f"Group={name}")
        production = production.replace("/var/lib/agent-coordinator", str(data))
        production = production.replace("/etc/agent-coordinator/service.env", str(environment))
        production = production.replace("/usr/local/bin/agent-coordinator-server", str(install / "agent-coordinator-server"))
        production = production.replace("StateDirectory=agent-coordinator", f"StateDirectory={name}")
        service_path.write_text(production)
        caddy_path.write_text(
            "[Unit]\nDescription=Disposable Agent Coordinator HTTPS acceptance proxy\nAfter=network.target\n"
            f"[Service]\nType=simple\nUser={name}\nGroup={name}\nWorkingDirectory={data}\n"
            f"Environment=HOME={data}\nEnvironment=XDG_DATA_HOME={data}/caddy-data\nEnvironment=XDG_CONFIG_HOME={data}/caddy-config\n"
            f"ExecStart={install}/caddy run --config {caddyfile} --adapter caddyfile\n"
            "NoNewPrivileges=true\nPrivateTmp=true\nProtectSystem=strict\nProtectHome=true\n"
            f"ReadWritePaths={data}\n[Install]\nWantedBy=multi-user.target\n"
        )
        service_path.chmod(0o644)
        caddy_path.chmod(0o644)
        run(["systemd-analyze", "verify", str(service_path), str(caddy_path)])
        run(["systemctl", "daemon-reload"])
        run(["systemctl", "start", service_unit])
        wait_for(None, f"http://127.0.0.1:{server_port}/healthz")
        run(["systemctl", "start", caddy_unit])
        ca = data / "caddy-data/caddy/pki/authorities/local/root.crt"
        wait_for(ca)
        tls = ssl.create_default_context(cafile=str(ca))
        wait_for(None, origin + "/healthz", tls)
        browser = Browser(origin, ca)
        browser.login(password)
        project = browser.call("/api/v1/projects", {
            "name": "Package acceptance", "repository_url": "https://example.invalid/package.git", "target_branch": "main",
        })
        credential = browser.call("/api/v1/admin/agents", {"name": "package-agent"})
        binding = data / ".agent-coordinator.toml"
        binding.write_text(f'service_url = "{origin}"\nproject_id = "{project["id"]}"\n')
        binding.chmod(0o600)
        client_home = data / "client"
        client_home.mkdir(mode=0o700)
        run(["chown", "-R", f"{name}:{name}", str(binding), str(client_home)])
        client_env = {
            key: value
            for key, value in os.environ.items()
            if not key.upper().startswith(("AGENT_COORDINATOR_", "COORDINATOR_"))
        }
        client_env.update({
            "AGENT_COORDINATOR_HOME": str(client_home),
            "AGENT_COORDINATOR_TOKEN": credential["token"],
            "SSL_CERT_FILE": str(ca),
        })
        connected = run([
            "runuser", "-u", name, "--", str(install / "agent-coordinator"), "--repo-config", str(binding),
            "--session", "package-acceptance", "--json", "connect",
        ], env=client_env)
        assert credential["token"] not in connected.stdout and credential["token"] not in connected.stderr
        assert json.loads(connected.stdout)["data"]["session"]["id"]
        run(["systemctl", "restart", service_unit])
        wait_for(None, origin + "/healthz", tls)
        listed = run([
            "runuser", "-u", name, "--", str(install / "agent-coordinator"), "--repo-config", str(binding),
            "--session", "package-acceptance", "--json", "tasks", "list",
        ], env=client_env)
        assert credential["token"] not in listed.stdout and credential["token"] not in listed.stderr
        assert json.loads(listed.stdout)["data"]["items"] == []
        print("PASS: package checksums/layout, systemd install/start/restart, trusted internal-CA HTTPS, and native CLI reconnect.")
    finally:
        for unit in [caddy_unit, service_unit]:
            subprocess.run(["systemctl", "stop", unit], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        service_path.unlink(missing_ok=True)
        caddy_path.unlink(missing_ok=True)
        subprocess.run(["systemctl", "daemon-reload"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        subprocess.run(["systemctl", "reset-failed"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for path in [data, config, install]:
            shutil.rmtree(path, ignore_errors=True)
        if created_user:
            subprocess.run(["userdel", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--systemd", action="store_true")
    parser.add_argument("--caddy", type=Path)
    args = parser.parse_args()
    assert args.package.is_file() and not args.package.is_symlink()
    verify_outer_checksum(args.package)
    with tempfile.TemporaryDirectory(prefix="agent-coordinator-package-") as temporary:
        root = extract_checked(args.package, Path(temporary))
        verify_internal(root)
        verify_markdown_links(root)
        if args.systemd:
            assert args.caddy is not None, "--caddy is required with --systemd."
            systemd_acceptance(root, args.caddy)
        else:
            print("PASS: package checksums, bounded extraction, layout, offline links, modes, unit safeguards, and binaries.")


if __name__ == "__main__":
    main()
