#!/usr/bin/env python3
"""WSL admission and evidence regressions; no root, WSL, or service required."""
import configparser
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('wsl_host', HERE / 'wsl-host.py')
HOST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HOST)


class Admission(unittest.TestCase):
    def test_configured_nat_cannot_mask_a_fallback_runtime(self):
        self.assertEqual(HOST.networking_errors('nat\n'), [])
        for mode, code in (('consomme', 0), ('virtioproxy', 0), ('mirrored', 0), ('nat', 1)):
            with self.subTest(mode=mode, code=code):
                self.assertTrue(HOST.networking_errors(mode, code))

    def test_example_is_admissible_but_default_wsl_is_not(self):
        config = configparser.ConfigParser()
        config.read(HERE / 'wsl.conf.example')
        self.assertEqual(HOST.configuration_errors(config), [])
        config.remove_section('interop')
        self.assertTrue(any('interop' in error for error in HOST.configuration_errors(config)))
        config.set('boot', 'initTimeout', '10000')
        self.assertTrue(any('180000' in error for error in HOST.configuration_errors(config)))

    def test_unsafe_host_never_runs_installer(self):
        with patch.object(HOST, 'preflight', return_value=['Windows interop enabled']), \
                patch.object(HOST.subprocess, 'run') as run:
            with self.assertRaises(SystemExit):
                HOST.install(Path('/untrusted/source'))
            run.assert_not_called()

    def test_changed_host_and_symlink_receipts_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / 'receipt'
            receipt.write_text(json.dumps({'fingerprint': {'kernel': 'old'}}))
            with patch.object(HOST, 'RECEIPT', receipt), \
                    patch.object(HOST, 'fingerprint', return_value={'kernel': 'new'}), \
                    patch.object(HOST.os, 'geteuid', return_value=0):
                # Mock ownership only: the test runs without root.
                real_stat = receipt.lstat()
                class Owned:
                    st_uid = 0
                    st_mode = real_stat.st_mode & ~0o022
                with patch.object(Path, 'lstat', return_value=Owned()), \
                        patch.object(Path, 'is_symlink', return_value=False):
                    with self.assertRaisesRegex(SystemExit, 'host changed'):
                        HOST.require_receipt()
                with patch.object(Path, 'lstat', return_value=Owned()), \
                        patch.object(Path, 'is_symlink', return_value=True):
                    with self.assertRaisesRegex(SystemExit, 'unsafe containment'):
                        HOST.require_receipt()


class SuiteEvidence(unittest.TestCase):
    def verify_result(self, suite_output, exit_code):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source'
            suite = source / 'deploy/agentc/containment-suite.sh'
            suite.parent.mkdir(parents=True)
            suite.write_text(f"printf '%s\\n' '{suite_output}'\nexit {exit_code}\n")
            receipt = root / 'receipt'
            receipt.write_text('previous success')
            log = root / 'log'
            original_path = HOST.Path
            def path(value):
                return log if value == '/var/log/agentc-wsl-containment.log' else original_path(value)
            def git(argv):
                value = 'abc123\n' if argv[-1] == 'HEAD' else ''
                return subprocess.CompletedProcess(argv, 0, value, '')
            with patch.object(HOST, 'RECEIPT', receipt), patch.object(HOST, 'Path', side_effect=path), \
                    patch.object(HOST, 'require_preflight'), patch.object(HOST, 'command', side_effect=git), \
                    patch.object(HOST, 'fingerprint', return_value={'kernel': 'test'}):
                if exit_code or 'FAIL' in suite_output or 'SKIP' in suite_output:
                    with self.assertRaises(SystemExit):
                        HOST.verify(source)
                    self.assertFalse(receipt.exists(), 'a failed rerun retained admission authority')
                else:
                    HOST.verify(source)
                    self.assertEqual(json.loads(receipt.read_text())['log_sha256'], HOST.digest(log))

    def test_nonzero_invalidates_previous_success(self):
        self.verify_result('PASS first check', 1)

    def test_false_success_with_failure_or_skip_is_refused(self):
        for output in ('FAIL egress escaped', 'SKIP candidate isolation'):
            with self.subTest(output=output):
                self.verify_result(output, 0)

    def test_success_records_retrievable_log_digest(self):
        self.verify_result('PASS full containment', 0)


if __name__ == '__main__':
    unittest.main()
