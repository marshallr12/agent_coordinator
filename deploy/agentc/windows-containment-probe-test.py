#!/usr/bin/env python3
"""Tests of the containment probe's fail-closed result handling.

Most tests are portable and run without any Windows API, because the Linux
deploy-script job runs this file. The tests at the end need native Windows and
real NTFS objects; they are skipped elsewhere.
"""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest

spec = importlib.util.spec_from_file_location("probe", Path(__file__).with_name("windows-containment-probe.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)
native = probe.native

PEERS = {"implementer": [("impl-2", "implementer"), ("rev-1", "reviewer")],
         "reviewer": [("impl-1", "implementer"), ("impl-2", "implementer")]}
ON_WINDOWS = sys.platform == "win32"


def facts(lpac=True):
    """Launch facts as the driver would build them for one launch."""
    environment = probe.environment_values(Path("C:/Windows"), Path("C:/work/tmp"), Path("C:/profile/Local"))
    return {"package_sid": "S-1-15-2-1-2-3", "lpac": lpac, "station": "Station\\Desktop",
            "environment": environment, "aliases": {"upper": "C:/HOST/CANARY.TXT"},
            "tcp": ["loopback4", "external"], "udp": ["loopback4"], "dns": True,
            "limits": probe.FIXTURE_LIMITS}


def passing_observations(checks):
    """One accepted observation for every mandatory check."""
    observed = {}
    for check in checks:
        accepted = check.expected[0] if check.expected else "value:anything"
        observed[check.key] = {"value:#": "value:7"}.get(accepted, accepted).replace("value:<=", "value:")
    return observed


def all_passing_rows():
    """Rows for one implementer launch in which nothing failed."""
    checks = probe.launch_checks("implementer", PEERS["implementer"], facts())
    return probe.tag(probe.judge(checks, passing_observations(checks)), "lpac", "impl-1")


class VerdictTests(unittest.TestCase):
    def test_a_complete_passing_matrix_passes_the_gate_but_never_admits_a_host(self):
        result = probe.summary({"appcontainer": all_passing_rows(), "lpac": all_passing_rows()}, [])
        self.assertEqual(result["prototype_gate"], "pass")
        self.assertEqual(result["variants"]["lpac"], {"gate": "pass", "blocking": []})
        self.assertIs(result["host_admitted"], False)
        self.assertTrue(result["unverified"])

    def test_no_outcome_admits_a_host(self):
        for rows in ([], all_passing_rows(), [probe.host_row("setup", "control", False, "failed")]):
            with self.subTest(rows=len(rows)):
                self.assertIs(probe.summary({"lpac": rows}, [])["host_admitted"], False)

    def test_a_variant_that_was_not_evaluated_is_blocked(self):
        result = probe.summary({"appcontainer": [], "lpac": []}, [])
        self.assertEqual(result["prototype_gate"], "blocked")
        self.assertIn("-:variant was not evaluated", result["variants"]["lpac"]["blocking"])

    def test_the_gate_follows_only_the_candidate_boundary(self):
        failing = probe.tag([probe.host_row("workload.cargo_test", "workload", False, "exit=1")], "x", "builder")
        blocked_comparison = probe.summary({"appcontainer": failing, "lpac": all_passing_rows()}, [])
        self.assertEqual(blocked_comparison["prototype_gate"], "pass")
        blocked_candidate = probe.summary({"appcontainer": all_passing_rows(), "lpac": failing}, [])
        self.assertEqual(blocked_candidate["prototype_gate"], "blocked")

    def test_a_failed_shared_control_blocks_every_variant(self):
        shared = probe.tag([probe.host_row("control.canary_read", "control", False, "host token")], "-", "-")
        result = probe.summary({"appcontainer": all_passing_rows(), "lpac": all_passing_rows()}, shared)
        for name in ("appcontainer", "lpac"):
            self.assertEqual(result["variants"][name]["blocking"], ["-:control.canary_read"])

    def test_one_failed_mandatory_row_blocks_and_is_named(self):
        rows = all_passing_rows()
        target = next(row for row in rows if row["check"] == "host.registry_environment_open")
        target.update(observed="allowed", status="fail")
        result = probe.verdict(rows)
        self.assertEqual(result, {"gate": "blocked", "blocking": ["impl-1:host.registry_environment_open"]})


class JudgingTests(unittest.TestCase):
    def setUp(self):
        self.checks = [probe.Check("host.file_read", "worker_to_host", "file", probe.DENIED),
                       probe.Check("path.planted", "own", "info", (), False)]

    def statuses(self, observed):
        return {row["check"]: row["status"] for row in probe.judge(self.checks, observed)}

    def test_a_missing_observation_fails(self):
        rows = probe.judge(self.checks, {})
        self.assertEqual((rows[0]["status"], rows[0]["observed"]), ("fail", "missing"))

    def test_an_allowed_attack_and_an_unexpected_error_both_fail(self):
        for value in ("allowed", "error:32", "error:", "ok", ""):
            with self.subTest(value=value):
                self.assertEqual(self.statuses({"host.file_read": value})["host.file_read"], "fail")
        self.assertEqual(self.statuses({"host.file_read": "error:5"})["host.file_read"], "pass")

    def test_an_unexpected_observation_is_a_mandatory_failure(self):
        rows = probe.judge(self.checks, {"host.file_read": "error:5", "host.surprise": "ok"})
        surprise = rows[-1]
        self.assertEqual((surprise["check"], surprise["status"], surprise["mandatory"]),
                         ("host.surprise", "fail", True))

    def test_informational_rows_neither_pass_nor_block(self):
        rows = probe.judge(self.checks, {"host.file_read": "error:5", "path.planted": "value:plain"})
        self.assertEqual(rows[1]["status"], "info")
        self.assertEqual(probe.verdict(rows)["gate"], "pass")

    def test_a_skipped_host_row_is_not_a_pass(self):
        skipped = probe.host_row("control.dns", "control", False, "skipped: offline")
        self.assertEqual(probe.verdict([skipped])["gate"], "blocked")

    def test_numeric_expectations(self):
        self.assertTrue(probe.accepts("value:<=23", "value:23"))
        self.assertFalse(probe.accepts("value:<=23", "value:24"))
        self.assertFalse(probe.accepts("value:<=23", "value:unbounded"))
        self.assertTrue(probe.accepts("value:#", "value:4242"))
        self.assertFalse(probe.accepts("value:#", "value:spawn-failed:5"))
        self.assertFalse(probe.accepts("error:5", "error:50"))

    def test_fixture_output_is_parsed_strictly(self):
        self.assertEqual(probe.parse_rows("a=ok\nb=value:x=y\n"), {"a": "ok", "b": "value:x=y"})
        for text in ("a=ok\na=ok", "no separator", "=value", "a=ok\n\nb=ok"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                probe.parse_rows(text)


class MatrixTests(unittest.TestCase):
    def mandatory(self, role, lpac=True):
        return [check for check in probe.launch_checks(role, PEERS[role], facts(lpac)) if check.mandatory]

    def test_every_isolation_scope_has_mandatory_rows(self):
        scopes = {check.scope for role in PEERS for check in self.mandatory(role)}
        for scope in ("worker_to_host", "implementer_to_reviewer", "reviewer_to_implementer",
                      "same_role_cross_launch", "own"):
            self.assertIn(scope, scopes)

    def test_every_protected_resource_has_mandatory_rows(self):
        resources = {check.resource for check in self.mandatory("implementer")}
        for resource in ("credential", "file", "process", "ipc", "child_escape", "path", "egress", "control"):
            self.assertIn(resource, resources)

    def test_no_check_is_listed_twice(self):
        for role in PEERS:
            keys = [check.key for check in probe.launch_checks(role, PEERS[role], facts())]
            self.assertEqual(len(keys), len(set(keys)))

    def test_every_mandatory_attack_names_a_positive_control(self):
        for role in PEERS:
            for check in self.mandatory(role):
                if check.resource != "control":
                    self.assertTrue(probe.controls_for(check.key), check.key)
        for key in ("escape.unlisted_handle", "net.host_received.loopback4", "hostpath.worker.hardlink_refused",
                    "hostpath.self.rename_race_refused", "workload.cargo_build", "broker.identity_from_os"):
            self.assertTrue(probe.controls_for(key), key)

    def test_launch_controls_named_by_attacks_are_in_the_matrix(self):
        keys = {check.key for check in self.mandatory("implementer")}
        named = {control for controls in probe.CONTROLS.values() for control in controls}
        for control in named:
            if control.startswith(("own.", "desktop.")):
                self.assertIn(control, keys)

    def test_the_two_token_kinds_expect_opposite_results_for_the_shared_file(self):
        row = {lpac: next(check for check in self.mandatory("implementer", lpac)
                          if check.key == "boundary.all_packages_read") for lpac in (True, False)}
        self.assertEqual((row[True].expected, row[True].scope), (probe.DENIED, "worker_to_host"))
        self.assertEqual((row[False].expected, row[False].resource), (("allowed",), "control"))

    def test_a_share_path_may_be_refused_as_network_access(self):
        self.assertEqual(probe.alias_refusals("unc_share"), ("error:5", "error:65"))
        self.assertEqual(probe.alias_refusals("verbatim"), probe.DENIED)

    def test_role_pairs_are_named_by_direction(self):
        self.assertEqual(probe.relation("implementer", "reviewer"), "implementer_to_reviewer")
        self.assertEqual(probe.relation("reviewer", "implementer"), "reviewer_to_implementer")
        self.assertEqual(probe.relation("implementer", "implementer"), "same_role_cross_launch")

    def test_the_candidate_is_one_of_the_evaluated_boundaries(self):
        self.assertIn(probe.CANDIDATE, probe.VARIANTS)
        self.assertTrue(probe.VARIANTS[probe.CANDIDATE].lpac)


class LaunchTests(unittest.TestCase):
    def test_the_environment_is_an_exact_allowlist(self):
        values = probe.environment_values(Path("C:/Windows"), Path("C:/work/tmp"), Path("C:/profile/Local"))
        self.assertEqual(set(values), {"PATH", "SystemRoot", "WINDIR", "TEMP", "TMP", "LOCALAPPDATA"})
        block = native.environment_block(values)
        names = {entry.partition("=")[0] for entry in block[:].split(chr(0)) if entry}
        self.assertEqual(names, set(values))
        self.assertTrue(block[:].endswith(chr(0) * 2))

    def test_there_is_no_launch_without_a_package_identity(self):
        with self.assertRaisesRegex(RuntimeError, "package identity"):
            native.Launch(None, None, probe.VARIANTS["lpac"], "Station\\Desktop", probe.FIXTURE_LIMITS)
        with self.assertRaisesRegex(RuntimeError, "package identity"):
            native.Launch(None, object(), None, "Station\\Desktop", probe.FIXTURE_LIMITS)

    def test_a_partly_applied_boundary_is_refused_before_resume(self):
        complete = {"in_job": True, "appcontainer": True, "package": True, "lpac": True}
        native.refuse_unless_contained(complete)
        for name in complete:
            with self.subTest(missing=name), self.assertRaisesRegex(RuntimeError, name):
                native.refuse_unless_contained({**complete, name: False})
        with self.assertRaises(RuntimeError):
            native.refuse_unless_contained({})

    def test_a_cargo_step_needs_a_zero_exit_an_empty_job_and_passing_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "log.txt"
            log.write_text("test result: ok. 9 passed; 0 failed", encoding="utf-8")
            self.assertEqual(probe.workload_row("test", 0, True, log)["status"], "pass")
            self.assertEqual(probe.workload_row("test", 101, True, log)["status"], "fail")
            self.assertEqual(probe.workload_row("test", None, True, log)["status"], "fail")
            self.assertEqual(probe.workload_row("test", 0, False, log)["status"], "fail")
            log.write_text("test result: FAILED. 8 passed; 1 failed", encoding="utf-8")
            self.assertEqual(probe.workload_row("test", 0, True, log)["status"], "fail")
            self.assertEqual(probe.workload_row("build", 0, True, log)["status"], "pass")

    @unittest.skipIf(ON_WINDOWS, "the refusal is for other platforms")
    def test_the_windows_layer_refuses_other_platforms(self):
        with self.assertRaisesRegex(RuntimeError, "native Windows"):
            native.Api()


@unittest.skipUnless(ON_WINDOWS, "needs native Windows and NTFS links")
class ReaderTests(unittest.TestCase):
    """The handle-based reader against real links, as a host broker would meet them."""

    def setUp(self):
        self.api = native.Api()
        self.area = tempfile.TemporaryDirectory()
        self.root, self.outside = Path(self.area.name) / "root", Path(self.area.name) / "outside"
        self.root.mkdir()
        self.outside.mkdir()
        (self.outside / "canary.txt").write_bytes(b"outside")
        (self.root / "plain.txt").write_bytes(b"plain")

    def tearDown(self):
        self.area.cleanup()

    def junction(self, link, target):
        subprocess.run(["cmd.exe", "/d", "/c", "mklink", "/J", str(link), str(target)], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def test_a_plain_file_inside_the_root_is_read(self):
        self.assertEqual(native.validated_read(self.api, self.root, "plain.txt"), b"plain")

    def test_a_file_with_two_names_is_refused(self):
        os.link(self.root / "plain.txt", self.root / "second.txt")
        with self.assertRaisesRegex(native.PathRefused, "hard links"):
            native.validated_read(self.api, self.root, "plain.txt")

    def test_a_junction_and_a_file_behind_it_are_refused(self):
        self.junction(self.root / "redirect", self.outside)
        self.assertEqual((self.root / "redirect" / "canary.txt").read_bytes(), b"outside")
        with self.assertRaises(native.PathRefused):
            native.validated_read(self.api, self.root, "redirect")
        with self.assertRaisesRegex(native.PathRefused, "outside the launch root"):
            native.validated_read(self.api, self.root, "redirect\\canary.txt")

    def test_a_path_swapped_to_an_outside_junction_is_never_read(self):
        host = types.SimpleNamespace(api=self.api, windows=Path(os.environ["SystemRoot"]),
                                     canary=self.outside / "canary.txt")
        self.junction(self.root / "redirect", self.outside)
        row = probe.rename_race_row(host, self.root)
        self.assertEqual(row["status"], "pass", row["observed"])
        self.assertIn('"leaked": 0', row["observed"])

    def test_a_root_that_is_itself_a_junction_is_refused(self):
        self.junction(Path(self.area.name) / "linked-root", self.root)
        with self.assertRaises(native.PathRefused):
            native.validated_read(self.api, Path(self.area.name) / "linked-root", "plain.txt")

    def test_a_directory_a_missing_file_and_an_oversized_file_are_refused(self):
        (self.root / "folder").mkdir()
        for name in ("folder", "absent.txt"):
            with self.subTest(name=name), self.assertRaises(native.PathRefused):
                native.validated_read(self.api, self.root, name)
        with self.assertRaisesRegex(native.PathRefused, "read limit"):
            native.validated_read(self.api, self.root, "plain.txt", limit=2)


if __name__ == "__main__":
    unittest.main()
