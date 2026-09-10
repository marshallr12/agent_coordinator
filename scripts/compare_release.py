#!/usr/bin/env python3
"""Compare two release ZIP files without printing packaged file contents."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import struct
import sys
import zipfile


MAX_MEMBERS = 512
MAX_MEMBER_SIZE = 256 * 1024 * 1024
MAX_TOTAL_SIZE = 512 * 1024 * 1024
PE_SIGNATURE = b"PE\0\0"
CODEVIEW_SIGNATURES = {b"RSDS": "rsds", b"NB10": "nb10"}
DEBUG_TYPES = {
    0: "unknown",
    1: "coff",
    2: "codeview",
    12: "vc_feature",
    13: "pogo",
    16: "reproducible",
    17: "embedded_portable_pdb",
}


class ComparisonError(Exception):
    """An archive or executable is malformed or exceeds diagnostic bounds."""


def sanitized_name(name: str, index: int) -> str:
    path = PurePosixPath(name)
    if (
        not name
        or "\\" in name
        or path.is_absolute()
        or any(part in ("", ".", "..") for part in path.parts)
        or any(ord(character) < 0x20 or ord(character) == 0x7F for character in name)
    ):
        return f"<invalid-member-{index}>"
    return str(path)


def digest(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def checked_slice(content: bytes, offset: int, size: int, label: str) -> bytes:
    if offset < 0 or size < 0 or offset > len(content) or size > len(content) - offset:
        raise ComparisonError(f"truncated {label}")
    return content[offset : offset + size]


def u16(content: bytes, offset: int, label: str) -> int:
    return struct.unpack("<H", checked_slice(content, offset, 2, label))[0]


def u32(content: bytes, offset: int, label: str) -> int:
    return struct.unpack("<I", checked_slice(content, offset, 4, label))[0]


def pe_summary(content: bytes) -> dict[str, object] | None:
    """Return structured PE timestamp/debug metadata, never embedded strings."""
    if len(content) < 64 or content[:2] != b"MZ":
        return None

    pe_offset = u32(content, 0x3C, "DOS header")
    if checked_slice(content, pe_offset, 4, "PE signature") != PE_SIGNATURE:
        raise ComparisonError("invalid PE signature")
    coff = pe_offset + 4
    section_count = u16(content, coff + 2, "COFF section count")
    coff_timestamp = u32(content, coff + 4, "COFF timestamp")
    if section_count > 96:
        raise ComparisonError("PE section count exceeds its diagnostic bound")
    optional_size = u16(content, coff + 16, "COFF optional-header size")
    optional = coff + 20
    checked_slice(content, optional, optional_size, "PE optional header")
    magic = u16(content, optional, "PE optional-header magic")
    if magic == 0x20B:
        data_directories = optional + 112
        directory_count_offset = optional + 108
        pe_kind = "pe32+"
    elif magic == 0x10B:
        data_directories = optional + 96
        directory_count_offset = optional + 92
        pe_kind = "pe32"
    else:
        raise ComparisonError("unsupported PE optional-header magic")

    if optional_size < directory_count_offset + 4 - optional:
        raise ComparisonError("PE optional header omits its data-directory count")
    checksum = u32(content, optional + 64, "PE checksum")
    directory_count = u32(content, directory_count_offset, "PE data-directory count")
    debug_directory = data_directories + 6 * 8
    if directory_count <= 6 or debug_directory + 8 > optional + optional_size:
        debug_rva = 0
        debug_size = 0
    else:
        debug_rva = u32(content, debug_directory, "PE debug RVA")
        debug_size = u32(content, debug_directory + 4, "PE debug size")

    sections_offset = optional + optional_size
    sections: list[tuple[int, int, int, int]] = []
    for index in range(section_count):
        section = sections_offset + index * 40
        checked_slice(content, section, 40, "PE section header")
        virtual_size = u32(content, section + 8, "PE virtual size")
        virtual_address = u32(content, section + 12, "PE virtual address")
        raw_size = u32(content, section + 16, "PE raw size")
        raw_offset = u32(content, section + 20, "PE raw offset")
        sections.append((virtual_address, max(virtual_size, raw_size), raw_offset, raw_size))

    def rva_to_offset(rva: int, size: int) -> int:
        for virtual_address, mapped_size, raw_offset, raw_size in sections:
            relative = rva - virtual_address
            if relative >= 0 and relative <= mapped_size and size <= mapped_size - relative:
                if relative > raw_size or size > raw_size - relative:
                    break
                return raw_offset + relative
        raise ComparisonError("PE debug directory RVA is not backed by a section")

    debug_entries: list[dict[str, object]] = []
    if debug_size:
        if debug_size // 28 > 128:
            raise ComparisonError("PE debug directory exceeds its diagnostic bound")
        if debug_size % 28:
            raise ComparisonError("PE debug directory has a partial entry")
        debug_offset = rva_to_offset(debug_rva, debug_size)
        for index in range(debug_size // 28):
            entry = debug_offset + index * 28
            timestamp = u32(content, entry + 4, "PE debug timestamp")
            entry_type = u32(content, entry + 12, "PE debug type")
            data_size = u32(content, entry + 16, "PE debug data size")
            data_offset = u32(content, entry + 24, "PE debug data offset")
            item: dict[str, object] = {
                "type": DEBUG_TYPES.get(entry_type, f"type-{entry_type}"),
                "timestamp": f"0x{timestamp:08x}",
                "size": data_size,
            }
            if data_size and data_offset:
                payload = checked_slice(content, data_offset, data_size, "PE debug payload")
                item["payload_sha256"] = digest(payload)
                if entry_type == 2 and len(payload) >= 4:
                    item["codeview_format"] = CODEVIEW_SIGNATURES.get(payload[:4], "other")
            debug_entries.append(item)

    return {
        "format": pe_kind,
        "coff_timestamp": f"0x{coff_timestamp:08x}",
        "checksum": f"0x{checksum:08x}",
        "debug_entries": debug_entries,
        "has_reproducible_debug_entry": any(item["type"] == "reproducible" for item in debug_entries),
    }


def byte_difference(first: bytes, second: bytes) -> dict[str, int | None]:
    shared = min(len(first), len(second))
    differing = 0
    first_offset = None
    last_offset = None
    for index in range(shared):
        if first[index] == second[index]:
            continue
        differing += 1
        if first_offset is None:
            first_offset = index
        last_offset = index
    if len(first) != len(second):
        differing += abs(len(first) - len(second))
        if first_offset is None:
            first_offset = shared
        last_offset = max(len(first), len(second)) - 1
    return {
        "differing_bytes": differing,
        "first_differing_offset": first_offset,
        "last_differing_offset": last_offset,
    }


def read_archive(path: Path) -> tuple[dict[str, bytes], dict[str, dict[str, object]]]:
    if not path.is_file() or path.is_symlink():
        raise ComparisonError(f"archive is not a regular file: {path}")
    if path.stat().st_size > MAX_TOTAL_SIZE:
        raise ComparisonError("archive file exceeds its diagnostic bound")
    contents: dict[str, bytes] = {}
    metadata: dict[str, dict[str, object]] = {}
    with zipfile.ZipFile(path) as archive:
        infos = archive.infolist()
        if len(infos) > MAX_MEMBERS:
            raise ComparisonError(f"archive has more than {MAX_MEMBERS} members")
        total = 0
        for index, info in enumerate(infos):
            name = sanitized_name(info.filename, index)
            if name in contents:
                raise ComparisonError(f"archive contains a duplicate member: {name}")
            if info.file_size > MAX_MEMBER_SIZE:
                raise ComparisonError(f"archive member exceeds {MAX_MEMBER_SIZE} bytes: {name}")
            total += info.file_size
            if total > MAX_TOTAL_SIZE:
                raise ComparisonError(f"archive expands beyond {MAX_TOTAL_SIZE} bytes")
            value = archive.read(info)
            if len(value) != info.file_size:
                raise ComparisonError(f"archive member size changed while reading: {name}")
            contents[name] = value
            metadata[name] = {
                "timestamp": list(info.date_time),
                "compression": info.compress_type,
                "external_attributes": f"0x{info.external_attr:08x}",
            }
    return contents, metadata


def archive_digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def compare(first_path: Path, second_path: Path) -> dict[str, object]:
    first, first_metadata = read_archive(first_path)
    second, second_metadata = read_archive(second_path)
    first_names = set(first)
    second_names = set(second)
    differences: list[dict[str, object]] = []
    for name in sorted(first_names & second_names):
        if first[name] == second[name] and first_metadata[name] == second_metadata[name]:
            continue
        item: dict[str, object] = {
            "member": name,
            "first_size": len(first[name]),
            "second_size": len(second[name]),
            "first_sha256": digest(first[name]),
            "second_sha256": digest(second[name]),
            **byte_difference(first[name], second[name]),
        }
        if first_metadata[name] != second_metadata[name]:
            item["first_zip_metadata"] = first_metadata[name]
            item["second_zip_metadata"] = second_metadata[name]
        if PurePosixPath(name).suffix.lower() in (".exe", ".dll"):
            first_pe = pe_summary(first[name])
            second_pe = pe_summary(second[name])
            item["first_pe"] = first_pe
            item["second_pe"] = second_pe
        differences.append(item)

    first_digest, second_digest = archive_digest(first_path), archive_digest(second_path)
    return {
        "equal": first_digest == second_digest,
        "member_contents_and_metadata_equal": not differences and first_names == second_names,
        "first_archive_sha256": first_digest,
        "second_archive_sha256": second_digest,
        "first_member_count": len(first),
        "second_member_count": len(second),
        "only_in_first": sorted(first_names - second_names),
        "only_in_second": sorted(second_names - first_names),
        "differences": differences,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("first", type=Path)
    parser.add_argument("second", type=Path)
    args = parser.parse_args()
    try:
        result = compare(args.first, args.second)
    except (ComparisonError, OSError, RuntimeError, zipfile.BadZipFile) as error:
        print(json.dumps({"error": "comparison_failed", "category": type(error).__name__}, sort_keys=True), file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["equal"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
