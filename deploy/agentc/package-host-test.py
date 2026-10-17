#!/usr/bin/env python3
"""Exercise the producer's real archives through the host updater's consumer."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


producer = load("package_host", ROOT / "scripts/package_host.py")
consumer = load("host_update", ROOT / "deploy/agentc/agentc-update.py")


class HostPackage(unittest.TestCase):
    def test_both_archives_are_reproducible_and_accepted_by_the_updater(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = {f"bin/{name}": b"binary fixture" for name in producer.BINARIES}
            for arch in producer.MACHINES:
                with self.subTest(arch=arch):
                    first = producer.package(files, root / arch / "first", "1.2.3", arch, 12345)
                    second = producer.package(files, root / arch / "second", "1.2.3", arch, 12345)
                    self.assertEqual(first.read_bytes(), second.read_bytes())
                    consumer.verify_sums((first.parent / "SHA256SUMS").read_text(), first.name, first)
                    destination = root / arch / "unpacked"
                    consumer.unpack(first, destination)
                    self.assertEqual({p.name for p in (destination / "bin").iterdir()}, set(producer.BINARIES))
                    for name in producer.BINARIES:
                        self.assertEqual((destination / "bin" / name).stat().st_mode & 0o777, 0o755)
                    (destination / "bin" / "agentc-push").write_bytes(b"tampered")
                    with self.assertRaises(consumer.Failed):
                        consumer.verify_bundle(destination)

    def test_rejects_unsafe_versions_and_foreign_binaries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(ValueError):
                producer.package({}, root, "../../escape", "x86_64", 0)
            with self.assertRaises(ValueError):
                producer.checked_entries(root, "aarch64", "1.2.3", "not-a-sha")


if __name__ == "__main__":
    unittest.main()
