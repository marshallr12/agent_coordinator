#!/usr/bin/env python3
"""Build deterministic Agent Coordinator binary release archives."""

import argparse
import datetime as dt
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import shutil
import stat
import tarfile
import tempfile
import urllib.parse
import zipfile


ROOT = Path(__file__).resolve().parents[1]
PLATFORMS = ("linux-x86_64", "windows-x86_64")
VERSION = re.compile(r"[0-9A-Za-z][0-9A-Za-z.+-]{0,63}\Z")
MARKDOWN_LINK = re.compile(r"!?\[[^\]]*\]\((?:<([^>]+)>|([^\s)]+))")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def atomic_bytes(path: Path, content: bytes, mode: int = 0o644) -> None:
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as destination:
            destination.write(content)
            destination.flush()
            os.fsync(destination.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def checked_file(path: Path, label: str) -> Path:
    if not path.is_file() or path.is_symlink():
        raise SystemExit(f"{label} must be a regular, non-symlink file: {path}")
    return path


def source_entries(platform: str, server: Path | None, cli: Path) -> dict[str, tuple[Path, int]]:
    docs = ROOT / "docs"
    deploy = ROOT / "deploy"
    entries: dict[str, tuple[Path, int]] = {}
    for source in sorted(ROOT.glob("*.md")):
        entries[source.name] = (checked_file(source, "top-level guide"), 0o644)
    for source in sorted(docs.glob("*.md")):
        entries[f"docs/{source.name}"] = (checked_file(source, "documentation"), 0o644)
    for source in sorted(deploy.iterdir()):
        if source.is_file() and not source.is_symlink():
            mode = 0o600 if source.name == "service.env.example" else 0o644
            entries[f"deploy/{source.name}"] = (checked_file(source, "deployment file"), mode)
    if platform == "linux-x86_64":
        if server is None:
            raise SystemExit("--server is required for the Linux package")
        entries.update({
            "bin/agent-coordinator-server": (checked_file(server, "server binary"), 0o755),
            "bin/agent-coordinator": (checked_file(cli, "CLI binary"), 0o755),
        })
    else:
        if server is not None:
            raise SystemExit("--server is not accepted for the Windows CLI package")
        entries["agent-coordinator.exe"] = (checked_file(cli, "Windows CLI binary"), 0o755)
    return entries


def validate_local_markdown_links(entries: dict[str, tuple[Path, int]]) -> None:
    for name, (source, _) in entries.items():
        if not name.endswith(".md"):
            continue
        for match in MARKDOWN_LINK.finditer(source.read_text(encoding="utf-8")):
            raw = match.group(1) or match.group(2)
            parsed = urllib.parse.urlsplit(raw)
            if parsed.scheme or parsed.netloc or not parsed.path or parsed.path.startswith("/"):
                continue
            target = posixpath.normpath(
                posixpath.join(str(PurePosixPath(name).parent), urllib.parse.unquote(parsed.path))
            )
            if target == ".." or target.startswith("../") or target not in entries:
                raise SystemExit(f"local Markdown link is missing from the package: {name}")


def checksum_manifest(entries: dict[str, tuple[Path, int]]) -> bytes:
    lines = [f"{sha256(source)}  {name}\n" for name, (source, _) in sorted(entries.items())]
    return "".join(lines).encode("utf-8")


def directories(entries: dict[str, tuple[Path, int]]) -> list[str]:
    values: set[str] = set()
    for name in [*entries, "SHA256SUMS"]:
        parent = PurePosixPath(name).parent
        while str(parent) != ".":
            values.add(str(parent))
            parent = parent.parent
    return sorted(values, key=lambda value: (value.count("/"), value))


def tar_info(name: str, size: int, mode: int, epoch: int, kind: bytes = tarfile.REGTYPE) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.size = size
    info.mode = mode
    info.mtime = epoch
    info.uid = 0
    info.gid = 0
    info.uname = "root"
    info.gname = "root"
    info.type = kind
    return info


def write_linux(path: Path, root_name: str, entries: dict[str, tuple[Path, int]], manifest: bytes, epoch: int) -> None:
    with tempfile.NamedTemporaryFile(prefix=".release-", suffix=".tar", dir=path.parent, delete=False) as raw:
        raw_path = Path(raw.name)
    with tempfile.NamedTemporaryFile(prefix=f".{path.name}.", dir=path.parent, delete=False) as compressed:
        compressed_path = Path(compressed.name)
    try:
        with tarfile.open(raw_path, "w", format=tarfile.GNU_FORMAT) as archive:
            archive.addfile(tar_info(root_name + "/", 0, 0o755, epoch, tarfile.DIRTYPE))
            for directory in directories(entries):
                archive.addfile(tar_info(f"{root_name}/{directory}/", 0, 0o755, epoch, tarfile.DIRTYPE))
            for name, (source, mode) in sorted(entries.items()):
                with source.open("rb") as content:
                    archive.addfile(tar_info(f"{root_name}/{name}", source.stat().st_size, mode, epoch), content)
            import io
            archive.addfile(tar_info(f"{root_name}/SHA256SUMS", len(manifest), 0o644, epoch), io.BytesIO(manifest))
        with raw_path.open("rb") as source, compressed_path.open("wb") as raw_destination:
            with gzip.GzipFile(filename="", mode="wb", compresslevel=9, mtime=0, fileobj=raw_destination) as destination:
                shutil.copyfileobj(source, destination, 1024 * 1024)
            raw_destination.flush()
            os.fsync(raw_destination.fileno())
        os.chmod(compressed_path, 0o644)
        os.replace(compressed_path, path)
    finally:
        raw_path.unlink(missing_ok=True)
        compressed_path.unlink(missing_ok=True)


def zip_info(name: str, mode: int, timestamp: tuple[int, int, int, int, int, int]) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name, timestamp)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.create_system = 3
    info.external_attr = (stat.S_IFREG | mode) << 16
    return info


def write_windows(path: Path, root_name: str, entries: dict[str, tuple[Path, int]], manifest: bytes, epoch: int) -> None:
    instant = dt.datetime.fromtimestamp(max(epoch, 315532800), tz=dt.timezone.utc)
    timestamp = (instant.year, instant.month, instant.day, instant.hour, instant.minute, instant.second - instant.second % 2)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            for name, (source, mode) in sorted(entries.items()):
                archive.writestr(zip_info(f"{root_name}/{name}", mode, timestamp), source.read_bytes())
            archive.writestr(zip_info(f"{root_name}/SHA256SUMS", 0o644, timestamp), manifest)
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=PLATFORMS, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--server", type=Path)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--source-date-epoch", type=int, default=int(os.environ.get("SOURCE_DATE_EPOCH", "0")))
    args = parser.parse_args()
    if not VERSION.fullmatch(args.version):
        raise SystemExit("--version must be 1-64 portable version characters")
    if args.source_date_epoch < 0 or args.source_date_epoch > 4_354_819_199:
        raise SystemExit("--source-date-epoch is outside the supported range")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    entries = source_entries(args.platform, args.server, args.cli)
    validate_local_markdown_links(entries)
    manifest = checksum_manifest(entries)
    root_name = f"agent-coordinator-{args.version}-{args.platform}"
    suffix = ".tar.gz" if args.platform == "linux-x86_64" else ".zip"
    archive = args.output_dir / f"{root_name}{suffix}"
    if args.platform == "linux-x86_64":
        write_linux(archive, root_name, entries, manifest, args.source_date_epoch)
    else:
        write_windows(archive, root_name, entries, manifest, args.source_date_epoch)
    digest = sha256(archive)
    checksum = archive.with_name(archive.name + ".sha256")
    atomic_bytes(checksum, f"{digest}  {archive.name}\n".encode("ascii"))
    print(json.dumps({"archive": str(archive.resolve()), "checksum": str(checksum.resolve()), "sha256": digest}, sort_keys=True))


if __name__ == "__main__":
    main()
