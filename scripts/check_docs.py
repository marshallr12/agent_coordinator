#!/usr/bin/env python3
"""Build the pinned mdBook and validate its maintained source and local links."""

import argparse
from collections import Counter
from html.parser import HTMLParser
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tomllib
import urllib.parse


ROOT = Path(__file__).resolve().parents[1]
PINNED_MDBOOK_VERSION = "0.5.4"
MAX_SOURCE_FILES = 256
MAX_SOURCE_BYTES = 16 * 1024 * 1024
SUMMARY_LINK = re.compile(r"\[[^\]]+\]\((?:<([^>]+)>|([^\s)]+))")
AUTHORITATIVE_WRAPPERS = ("AGENTS.md", "BACKLOG.md", "DURABLE-RECORD.md", "HANDOFF.md")
FORBIDDEN_NAMES = {
    ".env",
    "credentials",
    "credentials.toml",
    "password",
    "private-key",
    "secret",
    "secrets",
    "token",
}


class Links(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.ids: set[str] = set()
        self.references: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        for key in ("id", "name"):
            if values.get(key):
                self.ids.add(values[key] or "")
        for key in ("href", "src"):
            if values.get(key):
                self.references.append(values[key] or "")


def regular(path: Path, label: str) -> Path:
    if not path.is_file() or path.is_symlink():
        raise AssertionError(f"{label} must be a regular, non-symlink file.")
    return path


def configuration() -> tuple[Path, Path]:
    book = regular(ROOT / "book.toml", "book.toml")
    value = tomllib.loads(book.read_text(encoding="utf-8"))
    if value.get("book", {}).get("src") != "book/src":
        raise AssertionError("book.toml must use book/src as the maintained source directory.")
    if value.get("build", {}).get("build-dir") != "target/book":
        raise AssertionError("book.toml must keep generated HTML under target/book.")
    return ROOT / "book/src", ROOT / "target/book"


def source_files(source_root: Path) -> list[Path]:
    if not source_root.is_dir() or source_root.is_symlink():
        raise AssertionError("book/src must be a real directory.")
    files: list[Path] = []
    total = 0
    for path in sorted(source_root.rglob("*")):
        relative = path.relative_to(source_root)
        if path.is_symlink():
            raise AssertionError(f"Book source may not contain links: {relative}")
        if path.is_dir():
            if path.name.startswith(".") or path.name in {"target", "node_modules"}:
                raise AssertionError(f"Book source contains a forbidden directory: {relative}")
            continue
        regular(path, "Book source")
        if path.suffix.lower() != ".md" or path.name.startswith("."):
            raise AssertionError(f"Only maintained Markdown belongs in book/src: {relative}")
        if path.name.lower() in FORBIDDEN_NAMES or path.stem.lower() in FORBIDDEN_NAMES:
            raise AssertionError(f"Book source has a credential-like filename: {relative}")
        total += path.stat().st_size
        files.append(path)
    if not files or len(files) > MAX_SOURCE_FILES:
        raise AssertionError("Book source file count is empty or exceeds 256 files.")
    if total > MAX_SOURCE_BYTES:
        raise AssertionError("Book source exceeds the 16 MiB documentation bound.")
    return files


def summary_coverage(source_root: Path, files: list[Path]) -> None:
    summary = regular(source_root / "SUMMARY.md", "book/src/SUMMARY.md")
    linked: list[str] = []
    for match in SUMMARY_LINK.finditer(summary.read_text(encoding="utf-8")):
        raw = match.group(1) or match.group(2)
        parsed = urllib.parse.urlsplit(raw)
        if parsed.scheme or parsed.netloc or parsed.path.startswith("/"):
            raise AssertionError("SUMMARY.md may contain only local chapter links.")
        if not parsed.path:
            continue
        relative = PurePosixPath(urllib.parse.unquote(parsed.path))
        if relative.is_absolute() or ".." in relative.parts or relative.suffix.lower() != ".md":
            raise AssertionError("SUMMARY.md contains an invalid chapter path.")
        target = source_root.joinpath(*relative.parts)
        regular(target, "SUMMARY.md chapter")
        linked.append(relative.as_posix())
    duplicates = sorted(name for name, count in Counter(linked).items() if count != 1)
    if duplicates:
        raise AssertionError(f"SUMMARY.md repeats a chapter: {duplicates[0]}")
    expected = {
        path.relative_to(source_root).as_posix()
        for path in files
        if path != summary
    }
    missing = sorted(expected - set(linked))
    extra = sorted(set(linked) - expected)
    if missing or extra:
        detail = missing[0] if missing else extra[0]
        raise AssertionError(f"SUMMARY.md does not cover the maintained book exactly: {detail}")


def authoritative_wrappers(source_root: Path) -> None:
    for name in AUTHORITATIVE_WRAPPERS:
        source = regular(ROOT / name, f"root {name}")
        wrapper = regular(source_root / name, f"book wrapper for {name}")
        expected = f"{{{{#include ../../{name}}}}}"
        content = wrapper.read_text(encoding="utf-8")
        if content.count(expected) != 1 or content.count("{{#include") != 1 or len(content) > 1024:
            raise AssertionError(f"book/src/{name} must be a concise include wrapper for root {name}.")
        if source.stat().st_size == 0:
            raise AssertionError(f"Root authority file is empty: {name}")


def run_mdbook(executable: Path, output_root: Path) -> None:
    regular(executable, "mdBook executable")
    version = subprocess.run(
        [str(executable), "--version"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        timeout=30,
        check=False,
    )
    if version.returncode != 0 or version.stdout.strip() != f"mdbook v{PINNED_MDBOOK_VERSION}":
        raise AssertionError(f"Documentation checks require mdbook v{PINNED_MDBOOK_VERSION}.")
    if output_root.exists():
        resolved = output_root.resolve()
        target = (ROOT / "target").resolve()
        if resolved.parent != target or resolved.name != "book":
            raise AssertionError("Refusing to clean an unexpected documentation output directory.")
        shutil.rmtree(output_root)
    result = subprocess.run(
        [str(executable), "build"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        timeout=300,
        check=False,
    )
    if result.returncode != 0:
        output = (result.stdout + result.stderr).strip()
        if len(output) > 8_000:
            output = output[-8_000:]
        raise AssertionError(f"The pinned mdBook build failed:\n{output}")


def parsed_html(path: Path, cache: dict[Path, Links]) -> Links:
    if path not in cache:
        parser = Links()
        parser.feed(path.read_text(encoding="utf-8"))
        cache[path] = parser
    return cache[path]


def generated_links(output_root: Path) -> None:
    regular(output_root / "index.html", "generated book index")
    html = sorted(output_root.rglob("*.html"))
    if not html:
        raise AssertionError("mdBook produced no HTML pages.")
    cache: dict[Path, Links] = {}
    root = output_root.resolve()
    for source in html:
        for raw in parsed_html(source, cache).references:
            parsed = urllib.parse.urlsplit(raw)
            scheme = parsed.scheme.lower()
            if scheme in {"javascript", "file"}:
                raise AssertionError(f"Generated documentation contains an unsafe link: {source.relative_to(output_root)}")
            if scheme or parsed.netloc:
                continue
            if parsed.path.startswith("/"):
                if source.relative_to(output_root).as_posix() == "404.html" and raw == "/":
                    continue
                raise AssertionError(
                    f"Generated documentation contains an absolute local link: {source.relative_to(output_root)}"
                )
            relative_path = urllib.parse.unquote(parsed.path)
            target = (source.parent / relative_path).resolve() if relative_path else source.resolve()
            try:
                target.relative_to(root)
            except ValueError as error:
                raise AssertionError(f"Generated link escapes the book: {source.relative_to(output_root)}") from error
            if target.is_dir():
                target = target / "index.html"
            regular(target, f"generated link from {source.relative_to(output_root)}")
            if parsed.fragment and target.suffix.lower() == ".html":
                fragment = urllib.parse.unquote(parsed.fragment)
                if fragment not in parsed_html(target, cache).ids:
                    raise AssertionError(
                        f"Generated link has a missing fragment in {source.relative_to(output_root)}"
                    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--mdbook",
        type=Path,
        default=Path(os.environ.get("MDBOOK", "mdbook")),
        help="Path to the pinned mdBook executable.",
    )
    args = parser.parse_args()
    executable = args.mdbook
    if not executable.is_absolute():
        found = shutil.which(str(executable))
        if found is None:
            raise AssertionError("The pinned mdBook executable is unavailable.")
        executable = Path(found)
    source_root, output_root = configuration()
    files = source_files(source_root)
    summary_coverage(source_root, files)
    authoritative_wrappers(source_root)
    run_mdbook(executable, output_root)
    generated_links(output_root)
    print("PASS: pinned mdBook build, source coverage, authority wrappers, and generated local links.")


if __name__ == "__main__":
    main()
