#!/usr/bin/env python3
"""Focused safety tests for the restartable client installer."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock


SCRIPT = Path(__file__).with_name("upgrade_client.py")
SPEC = importlib.util.spec_from_file_location("upgrade_client", SCRIPT)
UPDATER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(UPDATER)


class UpgradeClientTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.commit = "a" * 40
        self.repository = "https://github.com/example/agent-coordinator"
        self.info = {
            "build": {"source_commit": self.commit, "source_repository": self.repository,
                      "target_os": "linux", "target_arch": "x86_64", "dirty": False},
            "supported_protocol_versions": ["v1"],
            "capabilities": ["durable_candidate_submission_fields"],
        }

    def executable(self, path, info):
        path.write_text("#!/usr/bin/env python3\nimport json\nprint(json.dumps(" + repr(info) + "))\n")
        path.chmod(0o755)
        return path

    def test_wrong_architecture_is_rejected_before_replacement(self):
        candidate_info = json.loads(json.dumps(self.info))
        candidate_info["build"]["target_arch"] = "aarch64"
        candidate = self.executable(self.root / "candidate", candidate_info)
        with self.assertRaises(SystemExit):
            UPDATER.verify_candidate(candidate, self.commit, self.repository,
                                     "linux-x86_64", {"v1"}, {"durable_candidate_submission_fields"})

    def test_bad_download_checksum_leaves_installed_client_unchanged(self):
        active = self.executable(self.root / "agent-coordinator", {"legacy": True})
        candidate = self.executable(self.root / "download", self.info)
        service_info = self.root / "service-info.json"
        service_info.write_text(json.dumps({"data": {"client_compatibility": {
            "required_protocol_versions": ["v1"],
            "required_capabilities": ["durable_candidate_submission_fields"],
            "compatible_client": {"source_commit": self.commit,
                                   "source_repository": self.repository,
                                   "supported_targets": [UPDATER.target_name()]},
        }}}))
        original = active.read_bytes()
        result = subprocess.run([
            sys.executable, str(SCRIPT), "--binary", str(active),
            "--service-info", str(service_info), "--candidate", str(candidate),
            "--sha256", "0" * 64,
        ], text=True, capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SHA-256 mismatch", result.stderr)
        self.assertEqual(active.read_bytes(), original)
        self.assertFalse((self.root / "agent-coordinator.rollback").exists())

    def test_missing_required_protocol_is_rejected_before_replacement(self):
        active = self.executable(self.root / "agent-coordinator", {"legacy": True})
        original = active.read_bytes()
        candidate_info = json.loads(json.dumps(self.info))
        candidate_info["supported_protocol_versions"] = []
        candidate = self.executable(self.root / "candidate", candidate_info)
        service_info = self.root / "service-info.json"
        service_info.write_text(json.dumps({"data": {"client_compatibility": {
            "required_protocol_versions": ["v1"],
            "required_capabilities": ["durable_candidate_submission_fields"],
            "compatible_client": {
                "source_commit": self.commit,
                "source_repository": self.repository,
                "supported_targets": [UPDATER.target_name()],
            },
        }}}))
        result = subprocess.run([
            sys.executable, str(SCRIPT), "--binary", str(active),
            "--service-info", str(service_info), "--candidate", str(candidate),
            "--sha256", UPDATER.sha256(candidate),
        ], text=True, capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("service-required protocol version", result.stderr)
        self.assertEqual(active.read_bytes(), original)
        self.assertFalse((self.root / "agent-coordinator.rollback").exists())

    def test_failed_post_install_verification_restores_rollback_and_state(self):
        active = self.executable(self.root / "agent-coordinator", {"legacy": True})
        candidate = self.executable(self.root / "candidate", self.info)
        rollback = self.root / "agent-coordinator.rollback"
        protected = self.root / "protected-session.json"
        protected.write_text('{"origin":"https://service.example","pending_key":"saved"}')
        original_active = active.read_bytes()
        original_protected = protected.read_bytes()
        calls = 0

        def report(executable):
            nonlocal calls
            calls += 1
            if executable == active:
                raise SystemExit("simulated installed-client verification failure")
            return self.info

        with mock.patch.object(UPDATER, "client_info", side_effect=report):
            with self.assertRaisesRegex(SystemExit, "restored rollback"):
                UPDATER.install(active, candidate, rollback, self.commit)
        self.assertEqual(calls, 3)
        self.assertEqual(active.read_bytes(), original_active)
        self.assertEqual(rollback.read_bytes(), original_active)
        self.assertEqual(protected.read_bytes(), original_protected)

    def test_source_commit_and_repository_must_match(self):
        candidate = self.executable(self.root / "candidate", self.info)
        with self.assertRaisesRegex(SystemExit, "source commit"):
            UPDATER.verify_candidate(candidate, "b" * 40, self.repository,
                                     "linux-x86_64", {"v1"}, {"durable_candidate_submission_fields"})
        with self.assertRaisesRegex(SystemExit, "source repository"):
            UPDATER.verify_candidate(candidate, self.commit, "https://github.com/other/repo",
                                     "linux-x86_64", {"v1"}, {"durable_candidate_submission_fields"})

    def test_locked_source_build_fallback_uses_exact_clean_origin(self):
        source = self.root / "source"
        source.mkdir()

        def run(command, **kwargs):
            if command[-2:] == ["rev-parse", "HEAD"]:
                return SimpleNamespace(returncode=0, stdout=self.commit + "\n")
            if command[-2:] == ["status", "--porcelain"]:
                return SimpleNamespace(returncode=0, stdout="")
            if command[-3:] == ["remote", "get-url", "origin"]:
                return SimpleNamespace(returncode=0, stdout=self.repository + ".git\n")
            self.assertEqual(command[-5:], ["build", "--release", "--locked", "-p", "coordinator-cli"])
            output = Path(kwargs["env"]["CARGO_TARGET_DIR"]) / "release"
            output.mkdir()
            (output / "agent-coordinator").write_text("mock built client")
            return SimpleNamespace(returncode=0, stdout="")

        with mock.patch.object(UPDATER.shutil, "which", side_effect=lambda name: f"/tools/{name}"), \
             mock.patch.object(UPDATER.subprocess, "run", side_effect=run):
            executable = UPDATER.build_from_source(source, self.commit, self.repository)
        self.assertTrue(executable.is_file())
        self.assertEqual(executable.parent.name, "release")


if __name__ == "__main__":
    unittest.main()
