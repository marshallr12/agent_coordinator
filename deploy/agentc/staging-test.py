#!/usr/bin/env python3
"""Tests for staging.py's --required-check parsing."""
import argparse
import importlib.util
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("staging", HERE / "staging.py")
staging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(staging)


class ParseCheck(unittest.TestCase):
    def test_parts_are_sent_stripped(self):
        self.assertEqual(staging.parse_check(" a : v1 : any "),
                         {"identity": "a", "version": "v1", "environment": "any"})

    def test_plain_check_is_unchanged(self):
        self.assertEqual(staging.parse_check("a:v1:any"),
                         {"identity": "a", "version": "v1", "environment": "any"})

    def test_malformed_checks_are_rejected(self):
        for text in ("a:v1", "a:v1:any:x", "a: :any", ":v1:any"):
            with self.assertRaises(argparse.ArgumentTypeError, msg=text):
                staging.parse_check(text)


if __name__ == "__main__":
    unittest.main()
