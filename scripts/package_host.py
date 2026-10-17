#!/usr/bin/env python3
"""Package native Linux host binaries for agentc-update, without host credentials."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
BINARIES = ("agent-coordinator", "agentc-push", "agentc-supervisor")
MACHINES = {"x86_64": 62, "aarch64": 183}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def checked_entries(bin_dir, arch, version, commit):
    """Refuse foreign, stale, dirty, or differently versioned build inputs."""
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("source commit must be a full SHA")
    if subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip() != commit \
            or subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT):
        raise ValueError("host release source must be clean at the source commit")
    if tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"] != version:
        raise ValueError("host version differs from the workspace")
    entries = {}
    for name in BINARIES:
        path = bin_dir / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"{name} must be a regular file")
        data = path.read_bytes()
        if data[:6] != b"\x7fELF\x02\x01" or int.from_bytes(data[18:20], "little") != MACHINES[arch]:
            raise ValueError(f"{name} is not a Linux {arch} ELF binary")
        entries[f"bin/{name}"] = data
        output = subprocess.check_output([str(path.resolve()), "--version"], text=True).strip()
        if output != f"{name} {version}":
            raise ValueError(f"{name} has the wrong version")
    identity = json.loads(subprocess.check_output(
        [str((bin_dir / "agent-coordinator").resolve()), "client-info", "--json"], text=True))
    build = identity.get("build", {})
    expected = {"source_commit": commit, "dirty": False, "target_os": "linux",
                "target_arch": arch, "version": version,
                "source_repository": "https://github.com/marshallr12/agent_coordinator"}
    if identity.get("version") != version or any(build.get(k) != v for k, v in expected.items()):
        raise ValueError("CLI build identity differs from the host release")
    entries["BUILD-IDENTITY.json"] = (json.dumps(identity, sort_keys=True, indent=2) + "\n").encode()
    return entries


def package(entries, output, version, arch, epoch):
    """Write reproducible regular-file-only archives and the updater's outer manifest."""
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) or arch not in MACHINES or epoch < 0:
        raise ValueError("expected a stable version, supported architecture and nonnegative epoch")
    root = f"agentc-host-{version}-linux-{arch}"
    files = dict(entries)
    files["SHA256SUMS"] = "".join(f"{digest(data)}  {name}\n" for name, data in sorted(entries.items())).encode()
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"{root}.tar.gz"
    with archive.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as zipped:
        with tarfile.open(fileobj=zipped, mode="w", format=tarfile.USTAR_FORMAT) as tar:
            for name, data in sorted(files.items()):
                info = tarfile.TarInfo(f"{root}/{name}")
                info.size, info.mtime = len(data), epoch
                info.mode = 0o755 if name.startswith("bin/") else 0o644
                tar.addfile(info, io.BytesIO(data))
    (output / "SHA256SUMS").write_text(f"{digest(archive.read_bytes())}  {archive.name}\n", encoding="ascii")
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--arch", choices=MACHINES, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-date-epoch", type=int, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    entries = checked_entries(args.bin_dir, args.arch, args.version, args.source_commit)
    print(package(entries, args.output_dir, args.version, args.arch, args.source_date_epoch))


if __name__ == "__main__":
    main()
