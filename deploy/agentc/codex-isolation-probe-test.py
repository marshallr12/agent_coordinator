#!/usr/bin/env python3
"""Tests for codex-isolation-probe.py's sandbox verdicts, with stub `codex` binaries."""
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

PROBE = Path(__file__).resolve().parent / "codex-isolation-probe.py"

# Runs the command with no isolation, so every hostile probe must be reported.
UNSANDBOXED = """#!/bin/sh
case "$1" in
  --version) echo "codex-cli stub";;
  sandbox) shift; while [ "$1" != "--" ]; do shift; done; shift; exec "$@";;
esac
"""

# Cannot build its sandbox, as inside a nested sandbox: nothing may be claimed.
NO_BUBBLEWRAP = """#!/bin/sh
case "$1" in
  --version) echo "codex-cli stub";;
  sandbox) echo "error building bubblewrap command: Read-only file system (os error 30)" >&2; exit 1;;
esac
"""


class Probe(unittest.TestCase):
    def run_probe(self, script, *flags):
        with tempfile.TemporaryDirectory() as root:
            stub = Path(root, "codex")
            stub.write_text(script)
            stub.chmod(0o755)
            work = Path(root, "work")
            result = subprocess.run(
                [sys.executable, "-I", str(PROBE), "sandbox", "--codex", str(stub), "--work", str(work), *flags],
                capture_output=True, text=True, env={"PATH": os.environ["PATH"]}, timeout=120)
            self.assertEqual(list(work.iterdir()), [], "the work directory is removed")
            return result

    def test_an_unisolated_candidate_fails_the_boundary_probes(self):
        result = self.run_probe(UNSANDBOXED)
        self.assertEqual(result.returncode, 1, result.stdout)
        for check in ("read-provider-secret", "read-coordinator-secret", "symlink-alias", "inherited-descriptor",
                      "write-git-config", "write-harness-state", "background-process", "guarded-files-unchanged"):
            self.assertIn(f"[fail] {check}:", result.stdout)
        self.assertIn("[pass] read-normal-file:", result.stdout)
        self.assertIn("[pass] write-workspace-file:", result.stdout)

    def test_a_sandbox_that_cannot_start_skips_every_probe_and_claims_nothing(self):
        result = self.run_probe(NO_BUBBLEWRAP)
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertNotIn("[pass]", result.stdout)
        self.assertNotIn("[fail]", result.stdout)
        self.assertIn("[skip] read-provider-secret: codex sandbox cannot run here", result.stdout)

    def test_require_complete_fails_when_anything_was_skipped(self):
        self.assertEqual(self.run_probe(NO_BUBBLEWRAP, "--require-complete").returncode, 2)


if __name__ == "__main__":
    unittest.main()
