#!/usr/bin/env python3
"""Tests of the containment probe's fail-closed result handling.

Most tests are portable and run without any Windows API, because the Linux
deploy-script job runs this file. The tests at the end need native Windows and
real NTFS objects; they are skipped elsewhere.
"""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest

spec = importlib.util.spec_from_file_location("probe", Path(__file__).with_name("windows-containment-probe.py"))
probe = importlib.util.module_from_spec(spec)
# The driver hands itself to its role-account module, so it must be importable by name.
sys.modules[spec.name] = probe
spec.loader.exec_module(probe)
native = probe.native
owner = probe.owner
policy = probe.policy
runner = probe.runner

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


MANIFEST = {"schema_version": 1, "owner_sid": "S-1-5-21-1-2-3-1001", "profile": "C:/Users/owner",
            "directory": "C:/owner/canaries", "file": "C:/owner/canaries/canary.txt",
            "credential": "C:/owner/canaries/credential-standin.txt", "registry_path": "Software/AgentcProbe-x",
            "pid": 4242, "pipe": "//./pipe/agentc-probe-x-owner", "event": "agentc-probe-x-owner",
            "toolchain": "C:/staging/toolchain", "msvc": "C:/msvc", "windows_kit": "C:/kit",
            "controls": {name: True for name in owner.OWNER_CONTROLS}}
ROLE_SID = "S-1-5-21-1-2-3-1002"


class RoleAccountTests(unittest.TestCase):
    def role_checks(self):
        return probe.launch_checks("implementer", PEERS["implementer"], {**facts(), "owner_rows": True})

    def test_owner_rows_exist_only_in_role_account_mode(self):
        single = {check.key for check in probe.launch_checks("implementer", PEERS["implementer"], facts())}
        role = {check.key: check for check in self.role_checks()}
        added = set(role) - single
        self.assertEqual(added, {key for key, _, _ in probe.OWNER_ATTACKS} | {"identity.registry_environment_open"})
        for key, _, _ in probe.OWNER_ATTACKS:
            self.assertEqual((role[key].scope, role[key].mandatory, role[key].expected),
                             ("worker_to_owner", True, probe.DENIED))
        self.assertFalse(role["identity.registry_environment_open"].mandatory)

    def test_every_owner_attack_names_an_owner_control(self):
        for key, _, _ in probe.OWNER_ATTACKS:
            self.assertEqual(probe.controls_for(key), ("control.owner.",))
        produced = {row["check"] for row in owner.control_rows(probe, MANIFEST)}
        self.assertTrue(any(check.startswith("control.owner.") for check in produced))
        self.assertIn("control.registry_read", produced)

    def test_a_failed_or_missing_owner_control_blocks(self):
        self.assertEqual({row["status"] for row in owner.control_rows(probe, MANIFEST)}, {"pass"})
        for controls in ({**MANIFEST["controls"], "pipe_connect": False},
                         {name: True for name in owner.OWNER_CONTROLS if name != "file_read"},
                         {**MANIFEST["controls"], "event_open": "true"}):
            rows = owner.control_rows(probe, {**MANIFEST, "controls": controls})
            self.assertEqual(probe.verdict(rows)["gate"], "blocked")

    def test_a_run_as_the_owner_is_plumbing_evidence_and_blocks(self):
        same = owner.identity_row(probe, MANIFEST["owner_sid"], MANIFEST["owner_sid"])
        self.assertEqual((same["status"], same["mandatory"]), ("fail", True))
        self.assertIn("plumbing evidence only", same["observed"])
        self.assertEqual(owner.identity_row(probe, MANIFEST["owner_sid"], ROLE_SID)["status"], "pass")

    def test_the_owner_step_can_block_a_passing_role_run(self):
        rows = all_passing_rows() + probe.tag([probe.host_row("control.canary_read", "control", True, "ok")], "-", "-")
        both = {"appcontainer": rows, "lpac": rows}
        passing = [dict(row, variant=name) for name, variant_rows in both.items() for row in variant_rows]
        self.assertEqual(probe.resummarize(passing)["prototype_gate"], "pass")
        tampered = probe.tag([probe.host_row("owner.integrity.files_unchanged", "file", False, "changed")], "-", "-")
        self.assertEqual(probe.resummarize(passing + tampered)["prototype_gate"], "blocked")
        self.assertEqual(probe.resummarize(tampered)["prototype_gate"], "blocked")

    def test_the_fixture_spec_names_the_owner_targets_and_the_launch_identity(self):
        values = owner.spec_values(MANIFEST, ROLE_SID)
        self.assertEqual(values["owner_file"], MANIFEST["file"])
        self.assertEqual(values["owner_pid"], 4242)
        self.assertTrue(values["identity_environment_key"].startswith(ROLE_SID))
        self.assertTrue(values["identity_environment_key"].endswith("Environment"))

    def test_an_incomplete_or_unknown_manifest_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "manifest.json"
            target.write_text(json.dumps(MANIFEST), encoding="utf-8")
            self.assertEqual(owner.load_manifest(target)["owner_sid"], MANIFEST["owner_sid"])
            broken = ({key: value for key, value in MANIFEST.items() if key != "pipe"},
                      {**MANIFEST, "schema_version": 2},
                      {**MANIFEST, "controls": {"invented": True}})
            for manifest in broken:
                target.write_text(json.dumps(manifest), encoding="utf-8")
                with self.assertRaises(ValueError):
                    owner.load_manifest(target)

    def test_the_role_command_runs_the_staged_interpreter_on_the_staged_probe(self):
        command = owner.role_command(Path("C:/staging"))
        self.assertTrue(command[0].endswith("python.exe"))
        self.assertIn("-I", command)
        self.assertEqual(Path(command[command.index("--owner-manifest") + 1]).name, "manifest.json")
        self.assertEqual(Path(command[command.index("--output") + 1]).parent.name, "role")


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


class StationTests(unittest.TestCase):
    """One logon session has one unnamed window station, so a run shares it between launches."""

    def test_the_station_admits_the_owner_and_exactly_the_listed_packages(self):
        sddl = native.station_sddl("S-1-5-21-1", ["S-1-15-2-7", "S-1-15-2-8"])
        self.assertEqual(sddl, "D:(A;;GA;;;S-1-5-21-1)(A;;GA;;;S-1-15-2-7)(A;;GA;;;S-1-15-2-8)S:(ML;;NW;;;LW)")
        self.assertEqual(native.station_sddl("S-1-5-21-1", []), "D:(A;;GA;;;S-1-5-21-1)S:(ML;;NW;;;LW)")

    def test_a_station_the_run_did_not_make_blocks_the_gate(self):
        made = types.SimpleNamespace(created=True)
        self.assertEqual(probe.station_row(made)["status"], "pass")
        for station in (None, types.SimpleNamespace(created=False)):
            row = probe.tag([probe.station_row(station)], "-", "-")[0]
            self.assertEqual(row["status"], "fail")
            result = probe.summary({"appcontainer": all_passing_rows(), "lpac": all_passing_rows()}, [row])
            self.assertEqual(result["prototype_gate"], "blocked")
            self.assertIn("-:desktop.station_made_by_run", result["variants"]["lpac"]["blocking"])

    @unittest.skipUnless(ON_WINDOWS, "needs native Windows window stations")
    def test_two_requests_in_one_logon_session_reach_the_same_station(self):
        api = native.Api()
        owner_sid = native.current_user_sid(api)
        first, second = native.Station(api, owner_sid), native.Station(api, owner_sid)
        try:
            self.assertEqual(first.name, second.name)
            # The second request can never be the one that made the station.
            self.assertIs(second.created, False)
            for package in ("S-1-15-2-7", "S-1-15-2-8"):
                first.admit(package)
            first.dismiss("S-1-15-2-7")
            self.assertEqual(first.packages, ["S-1-15-2-8"])
        finally:
            second.close()
            first.close()


STATE_ON = {"profiles": {"Domain": True, "Private": True, "Public": True}, "service": "Running"}
STATE_OFF = {"profiles": {"Domain": False, "Private": False, "Public": False}, "service": "Running"}


class PolicyTests(unittest.TestCase):
    """Network rounds under firewall conditions: conditions, observed state and the restore record."""

    def test_conditions_start_with_the_untouched_state_and_are_known_and_distinct(self):
        self.assertEqual(policy.parse_conditions("baseline, profiles-off"), ["baseline", "profiles-off"])
        for text in ("profiles-off", "baseline,baseline", "baseline,invented", ""):
            with self.subTest(text=text), self.assertRaises(ValueError):
                policy.parse_conditions(text)

    def test_the_state_query_output_is_parsed_strictly(self):
        text = json.dumps({"profiles": [{"name": "Domain", "enabled": "True"}, {"name": "Private", "enabled": "False"},
                                        {"name": "Public", "enabled": "True"}], "service": "Running"})
        self.assertEqual(policy.parse_state(text),
                         {"profiles": {"Domain": True, "Private": False, "Public": True}, "service": "Running"})
        one_profile = json.dumps({"profiles": [{"name": "Domain", "enabled": "True"}], "service": "Running"})
        for broken in ("", "{}", "[]", one_profile):
            with self.subTest(broken=broken):
                self.assertIsNone(policy.parse_state(broken))

    def test_a_condition_is_reached_only_by_its_own_state(self):
        stopped = {**STATE_ON, "service": "Stopped"}
        mixed = {"profiles": {**STATE_OFF["profiles"], "Public": True}, "service": "Running"}
        reached = {("baseline", "on"): STATE_ON, ("profiles-off", "off"): STATE_OFF, ("service-stop", "stopped"): stopped}
        for (condition, _), state in reached.items():
            self.assertTrue(policy.reached(condition, state), condition)
        missed = (("baseline", STATE_OFF), ("baseline", stopped), ("baseline", None), ("profiles-off", mixed),
                  ("profiles-off", STATE_ON), ("service-stop", STATE_ON))
        for condition, state in missed:
            with self.subTest(condition=condition, state=state):
                self.assertFalse(policy.reached(condition, state))

    def test_a_round_needs_the_owners_record_and_the_state_the_role_run_itself_reads(self):
        applied = {"condition": "profiles-off", "outcome": "applied", "detail": "every profile disabled"}
        def statuses(record, state):
            """Row statuses for the profiles-off condition."""
            return [row["status"] for row in policy.condition_rows(probe, "profiles-off", record, state)]

        self.assertEqual(statuses(applied, STATE_OFF), ["pass", "pass"])
        # The record claims the change, but the role run still sees the firewall on.
        self.assertEqual(statuses(applied, STATE_ON), ["pass", "fail"])
        self.assertEqual(statuses(None, STATE_OFF)[0], "fail")
        self.assertEqual(statuses({"outcome": "not-applied"}, STATE_OFF)[0], "fail")

    def test_only_the_service_stop_may_be_refused_without_blocking(self):
        refused = {"outcome": "refused", "detail": "access denied"}
        rows = policy.condition_rows(probe, "service-stop", refused, STATE_ON)
        self.assertEqual([row["status"] for row in rows], ["info"])
        rows = policy.condition_rows(probe, "profiles-off", refused, STATE_ON)
        self.assertEqual(rows[0]["status"], "fail")

    def test_a_missing_fixture_fails_every_condition_and_blocks_the_gate(self):
        host = types.SimpleNamespace(manifest={"policy_conditions": ["baseline", "profiles-off"]})
        rows = policy.role_rounds(probe, host, None)
        self.assertEqual([row["launch"] for row in rows], ["policy-baseline", "policy-profiles-off"])
        result = probe.summary({"appcontainer": all_passing_rows(), "lpac": all_passing_rows() + rows}, [])
        self.assertEqual(result["prototype_gate"], "blocked")
        self.assertEqual(policy.role_rounds(probe, types.SimpleNamespace(manifest={}), "fixture.exe"), [])

    def test_a_round_expects_what_the_matrix_expects(self):
        targets = {"tcp.loopback4": "127.0.0.1:1", "tcp.external": "192.0.2.1:443",
                   "udp.loopback4": "127.0.0.1:2", "dns_name": "example.com"}
        facts = policy.network_facts(targets)
        self.assertEqual(facts, {"tcp": ["loopback4", "external"], "udp": ["loopback4"], "dns": True})
        observed = {"net.tcp.loopback4": "error:10013", "net.tcp.external": "allowed",
                    "net.udp.loopback4": "allowed", "net.dns": "error:11001"}
        statuses = {row["check"]: row["status"] for row in probe.judge(probe.network_checks(facts), observed)}
        self.assertEqual(statuses, {"net.tcp.loopback4": "pass", "net.tcp.external": "fail",
                                    "net.udp.loopback4": "info", "net.dns": "pass"})
        self.assertEqual(policy.observations(probe, None), {})
        self.assertEqual(policy.observations(probe, b"net.dns=allowed\nnet.dns=allowed\n"), {})

    def test_an_applied_condition_counts_only_when_the_state_read_back_matches(self):
        self.assertEqual(policy.confirm("profiles-off", "applied", "done", STATE_OFF)[0], "applied")
        self.assertEqual(policy.confirm("profiles-off", "applied", "done", STATE_ON)[0], "failed")
        self.assertEqual(policy.confirm("service-stop", "refused", "denied", STATE_ON)[0], "refused")
        self.assertEqual(policy.apply_condition(Path("C:/Windows"), "profiles-off", untouched=True)[0],
                         "not-applied")
        self.assertEqual(policy.apply_condition(Path("C:/Windows"), "baseline")[0], "applied")

    def test_the_report_fails_unless_the_original_firewall_state_is_confirmed_back(self):
        back = {"restored": True, "original": STATE_ON, "final": STATE_ON, "changed": ["profiles-off"]}
        self.assertEqual(policy.restored_row(probe, back)["status"], "pass")
        for record in (None, {**back, "restored": False}, {**back, "final": STATE_OFF}, {**back, "final": None}):
            with self.subTest(record=record):
                self.assertEqual(policy.restored_row(probe, record)["status"], "fail")

    def test_the_two_sides_write_in_separate_directories_and_the_original_state_is_saved_once(self):
        with tempfile.TemporaryDirectory() as directory:
            staging = Path(directory)
            self.assertEqual(policy.marker(staging, "baseline", "ready"),
                             staging / "role" / "policy" / "baseline.ready")
            self.assertEqual(policy.record_file(staging, "baseline"), staging / "policy" / "baseline.json")
            policy.owner_directory(staging).mkdir()
            self.assertEqual(policy.save_original(staging, STATE_ON), STATE_ON)
            # A second run that starts from a changed state must not replace the saved original.
            self.assertEqual(policy.save_original(staging, STATE_OFF), STATE_ON)
            self.assertIsNone(policy.read_json(staging / "policy" / "absent.json"))
            self.assertTrue(policy.wait_for_file(staging / "policy" / "original.json", 0))


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


# A stand-in for `agentc-supervisor windows-runner`: it answers each request
# line the way the runner does, and records what it was asked in its state
# directory. The interpreter runs it with the same arguments the client passes.
FAKE_RUNNER = r'''
import json, sys
from pathlib import Path
state = Path(sys.argv[sys.argv.index("--state-dir") + 1])
log = (state / "asked.jsonl").open("a", encoding="utf-8")
for line in sys.stdin:
    request = json.loads(line)
    log.write(line)
    log.flush()
    op = request["op"]
    if op == "identity":
        reply = {"ok": True, "account_sid": "S-1-5-21-9", "job_owner": "runner:1:2", "pid": 1}
    elif op == "prepare" and request["launch"] == "refused":
        reply = {"ok": False, "error": "station made by another program", "refusal": "station_not_ours"}
    elif op == "prepare":
        reply = {"ok": True, "package_sid": "S-1-15-2-7", "desktop": "S\\D", "job_owner": "runner:1:2"}
    elif op == "hold_file":
        reply = {"ok": True, "handle": 432}
    elif op == "start":
        reply = {"ok": True, "pid": 77, "created": 5}
    elif op == "observe":
        reply = {"ok": True, "state": "exited", "code": 3} if request["wait_ms"] else \
                {"ok": True, "state": "running"}
    elif op == "watch":
        reply = {"ok": True, "alive_in_job": True}
    elif op == "terminate":
        reply = {"ok": True, "quiescent": True}
    elif op == "watched_ended":
        reply = {"ok": True, "ended": True}
    elif op == "cleanup":
        reply = {"ok": True, "profile_deleted": True, "detail": None}
    elif op == "read" and request["relative"] == "link":
        reply = {"ok": False, "error": "the path is refused: reparse point", "refusal": "path_refused"}
    elif op == "read":
        reply = {"ok": True, "hex": b"plain".hex()}
    else:
        reply = {"ok": False, "error": "unknown", "refusal": None}
    print(json.dumps(reply), flush=True)
'''


class RunnerClientTests(unittest.TestCase):
    """The driver's side of the runner protocol, against a stand-in runner."""

    def setUp(self):
        self.area = tempfile.TemporaryDirectory()
        self.root = Path(self.area.name)
        script = self.root / "fake-runner.py"
        script.write_text(FAKE_RUNNER, encoding="utf-8")
        (self.root / "state").mkdir()
        # The client runs `<executable> windows-runner --state-dir <dir>`; a
        # small launcher script lets the interpreter stand in for the executable.
        self.client = runner.Client.__new__(runner.Client)
        self.client.executable = script
        self.client.log = (self.root / "runner.txt").open("wb")
        self.client.process = subprocess.Popen(
            [sys.executable, "-I", str(script), "windows-runner", "--state-dir", str(self.root / "state")],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.client.log)
        self.client.identity = self.client.ask("identity")
        self.client.station_made = False
        self.addCleanup(self.area.cleanup)
        self.addCleanup(self.client.close)

    def asked(self):
        """Every request the stand-in runner received, in order."""
        text = (self.root / "state" / "asked.jsonl").read_text(encoding="utf-8")
        return [json.loads(line) for line in text.splitlines()]

    def test_a_launch_is_prepared_with_its_whole_boundary_and_no_command_string(self):
        boundary, limits = probe.VARIANTS["lpac"], probe.FIXTURE_LIMITS
        made = self.client.prepare("lpac.impl-1", "agentc.probe.x", boundary, limits, Path("C:/c"), Path("C:/w"))
        self.assertEqual((made["package_sid"], made["desktop"]), ("S-1-15-2-7", "S\\D"))
        self.assertTrue(self.client.station_made)
        spec = self.asked()[-1]["spec"]
        self.assertEqual(spec["less_privileged"], True)
        self.assertEqual(spec["capabilities"], ["registryRead", probe.TOOLS_CAPABILITY])
        self.assertEqual(spec["limits"], {"processes": 24, "process_memory_mb": 512, "job_memory_mb": 1024})
        launch = runner.Launch(self.client, "lpac.impl-1")
        launch.start([Path("C:/bin/fixture.exe"), "launch", "impl-1"], Path("C:/w"), {"A": Path("C:/x")},
                     Path("C:/logs/1.txt"))
        started = self.asked()[-1]
        self.assertEqual(started["argv"], [str(Path("C:/bin/fixture.exe")), "launch", "impl-1"])
        self.assertEqual(started["environment"], {"A": str(Path("C:/x"))})
        self.assertEqual((launch.pid, started["listed"]), (77, []))

    def test_a_refused_station_stops_the_run_and_is_not_recorded_as_made(self):
        boundary, limits = probe.VARIANTS["lpac"], probe.FIXTURE_LIMITS
        with self.assertRaises(RuntimeError) as raised:
            self.client.prepare("refused", "agentc.probe.x", boundary, limits, Path("C:/c"), Path("C:/w"))
        self.assertEqual(raised.exception.refusal, "station_not_ours")
        self.assertFalse(self.client.station_made)
        host = types.SimpleNamespace(runner=self.client, station=None)
        self.assertEqual(probe.station_row(probe.station_of(host))["status"], "fail")

    def test_waiting_terminating_and_cleaning_up_report_what_the_runner_saw(self):
        launch = runner.Launch(self.client, "lpac.impl-1")
        self.assertIsNone(launch.wait(0))
        self.assertEqual(launch.wait(1.5), 3)
        self.assertEqual(self.asked()[-1]["wait_ms"], 1500)
        self.assertFalse(launch.reaped)
        self.assertTrue(launch.watch(4242))
        self.assertTrue(launch.terminate())
        self.assertTrue(launch.reaped, "the watched descendant's end is recorded when the job is emptied")
        self.assertTrue(launch.cleanup())
        self.assertEqual([request["op"] for request in self.asked()[-4:]],
                         ["watch", "terminate", "watched_ended", "cleanup"])

    def test_only_a_listed_held_handle_is_named_in_a_start(self):
        launch = runner.Launch(self.client, "lpac.handles")
        handle = launch.hold_file(Path("C:/host/canary.txt"))
        launch.start(["fixture.exe", "read-handle", str(handle)], Path("C:/w"), {}, Path("C:/l.txt"), (handle,))
        self.assertEqual(self.asked()[-1]["listed"], [432])
        launch.start(["fixture.exe", "read-handle", str(handle)], Path("C:/w"), {}, Path("C:/l.txt"))
        self.assertEqual(self.asked()[-1]["listed"], [])

    def test_the_reader_goes_through_the_runner_and_keeps_its_refusals(self):
        host = types.SimpleNamespace(runner=self.client)
        self.assertEqual(probe.read_validated(host, Path("C:/w"), "plain.txt"), b"plain")
        with self.assertRaises(native.PathRefused):
            probe.read_validated(host, Path("C:/w"), "link")
        self.assertEqual(probe.launcher_record(types.SimpleNamespace(runner=None)), {"kind": "python-prototype"})
        record = probe.launcher_record(host)
        self.assertEqual((record["kind"], record["account_sid"]), ("rust-runner", "S-1-5-21-9"))
        self.assertEqual(len(record["sha256"]), 64)

    def test_a_runner_that_stops_answering_is_an_error_not_a_pass(self):
        self.client.process.kill()
        self.client.process.wait()
        with self.assertRaises(runner.RunnerError):
            self.client.ask("identity")


class StagedLauncherTests(unittest.TestCase):
    def test_the_role_command_names_the_staged_launcher_only_when_asked(self):
        plain = owner.role_command(Path("C:/staging"))
        self.assertNotIn("--launcher", plain)
        command = owner.role_command(Path("C:/staging"), launcher=True)
        staged = Path(command[command.index("--launcher") + 1])
        self.assertEqual((staged.name, staged.parent.name), ("agentc-supervisor.exe", "launcher"))
        self.assertIn("windows-probe-runner.py", owner.STAGED_SOURCES)


if __name__ == "__main__":
    unittest.main()
