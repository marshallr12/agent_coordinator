#!/usr/bin/env python3
"""Portable tests of fail-closed probe result handling; no Windows API calls."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("probe", Path(__file__).with_name("windows-containment-probe.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class ResultTests(unittest.TestCase):
    def test_no_uncontained_fallback(self):
        native = object.__new__(probe.Native)
        with self.assertRaisesRegex(RuntimeError, "package SID"):
            native.run(None, [], Path("C:/probe"), Path("C:/Windows"), False)

    def test_missing_and_malformed_observations_fail(self):
        for body in ("", "a=true\na=false", "a=timeout", "other=true", "a=true\nnoise"):
            with self.subTest(body=body), self.assertRaises(RuntimeError):
                probe.parse_results(body, {"a", "b"})

    def test_false_is_a_failed_observation(self):
        self.assertEqual(probe.parse_results("a=false\nb=true\n", {"a", "b"}),
                         {"a": False, "b": True})

    def test_no_experiment_admits_a_host(self):
        for rows in ([], [{"status": "pass"}], [{"status": "fail"}], [{"status": "skip"}]):
            self.assertFalse(probe.verdict(rows)["host_admitted"])
        self.assertFalse(probe.verdict([])["experiment_passed"])
        self.assertFalse(probe.verdict([{"status": "skip"}])["experiment_passed"])
        self.assertFalse(probe.verdict([{"status": "fail"}])["experiment_passed"])

    def test_environment_does_not_inherit_caller_values(self):
        block = probe.environment_block(Path("C:/probe"), Path("C:/Windows"), Path("C:/profile/Local"))
        self.assertTrue(block.endswith("\0\0"))
        self.assertEqual({entry.partition("=")[0] for entry in block.split("\0") if entry},
                         {"PATH", "SystemRoot", "WINDIR", "TEMP", "TMP", "LOCALAPPDATA"})


if __name__ == "__main__":
    unittest.main()
