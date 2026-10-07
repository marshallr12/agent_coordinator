#!/usr/bin/env python3
"""Tests for the staging pilot's argument parsing."""
import argparse
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("staging", Path(__file__).with_name("staging.py"))
staging = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(staging)


class ParseCheckTests(unittest.TestCase):
    def test_parts_are_sent_stripped(self):
        self.assertEqual(staging.parse_check(" a : v1 : any "),
                         {"identity": "a", "version": "v1", "environment": "any"})

    def test_default_check(self):
        self.assertEqual(staging.parse_check(staging.DEFAULT_CHECK),
                         {"identity": "staging-diff-check", "version": "v1",
                          "environment": "any"})

    def test_malformed_checks_are_rejected(self):
        for text in ("a:v1", "a:v1:any:extra", "a: :any", ":v1:any", "a:v1:  "):
            with self.subTest(text=text), self.assertRaises(argparse.ArgumentTypeError):
                staging.parse_check(text)


if __name__ == "__main__":
    unittest.main()
