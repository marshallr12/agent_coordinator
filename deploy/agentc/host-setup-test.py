#!/usr/bin/env python3
"""Tests for the attention canary and digest parts of host-setup.sh.

Runs without root or systemd: it sources the script (which then defines its
functions and does nothing else) and checks the unit files it would write, the
environment file and the --uninstall list.
"""
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "host-setup.sh"


def bash(code, env=None):
    """Stdout of `code` run in bash after sourcing host-setup.sh."""
    result = subprocess.run(["bash", "-c", f'set -euo pipefail; source "$1"; {code}', "bash", str(SCRIPT)],
                            capture_output=True, text=True, env=env)
    assert result.returncode == 0, result.stderr
    return result.stdout


def parse_unit(text):
    """{section: {key: value}} for a simple systemd unit."""
    sections, current = {}, None
    for line in text.splitlines():
        if line.startswith("[") and line.endswith("]"):
            current = sections.setdefault(line[1:-1], {})
        elif "=" in line:
            key, _, value = line.partition("=")
            current[key] = value
    return sections


class AttentionUnits(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)

    def units(self):
        bash(f'write_attention_units "{self.dir}"')
        return {p.name: p.read_text() for p in sorted(self.dir.iterdir())}

    def test_four_units_are_written(self):
        self.assertEqual(sorted(self.units()), [
            "agentc-canary.service", "agentc-canary.timer",
            "agentc-digest.service", "agentc-digest.timer"])

    def test_services_run_attention_py_from_the_environment_file(self):
        units = self.units()
        for name, command in (("canary", "canary"), ("digest", "digest")):
            service = parse_unit(units[f"agentc-{name}.service"])
            self.assertEqual(service["Service"]["Type"], "oneshot")
            self.assertEqual(service["Service"]["EnvironmentFile"], "/etc/agentc/attention.env")
            self.assertEqual(service["Service"]["ExecStart"],
                             f"/usr/bin/python3 -I /opt/agentc/bin/attention.py {command}")
            self.assertEqual(service["Service"]["ProtectSystem"], "strict")
            self.assertEqual(service["Service"]["NoNewPrivileges"], "yes")
            self.assertEqual(service["Service"]["ReadWritePaths"], "/var/lib/agentc")

    def test_the_canary_runs_every_ten_minutes_and_the_digest_daily(self):
        units = self.units()
        canary = parse_unit(units["agentc-canary.timer"])
        self.assertEqual(canary["Timer"]["OnUnitActiveSec"], "10min")
        digest = parse_unit(units["agentc-digest.timer"])
        self.assertEqual(digest["Timer"]["OnCalendar"], "daily")
        self.assertEqual(digest["Timer"]["Persistent"], "true")
        for text in (units["agentc-canary.timer"], units["agentc-digest.timer"]):
            self.assertEqual(parse_unit(text)["Install"]["WantedBy"], "timers.target")

    def test_schedules_can_be_overridden_when_the_script_runs(self):
        env = {"PATH": "/usr/bin:/bin", "CANARY_INTERVAL": "5min", "DIGEST_CALENDAR": "*-*-* 07:30:00"}
        self.assertIn("OnUnitActiveSec=5min", bash("attention_canary_timer", env))
        self.assertIn("OnCalendar=*-*-* 07:30:00", bash("attention_digest_timer", env))

    def test_the_environment_file_comments_defaults_and_asks_only_for_owner_values(self):
        lines = bash("attention_env_file").splitlines()
        active = [line for line in lines if line and not line.startswith("#")]
        self.assertEqual(active, ["ATTENTION_NTFY_TOPIC=", "ATTENTION_SMTP_HOST=", "ATTENTION_MAIL_TO="])
        for name in ("URL", "PROJECT", "TOKEN_FILE", "HEARTBEAT", "HEARTBEAT_MAX_AGE", "NTFY_URL",
                     "STATE", "MAX_HRI", "HOURS", "SMTP_PORT", "MAIL_FROM", "SMTP_TLS", "SMTP_USER",
                     "SMTP_PASSWORD_FILE"):
            self.assertTrue(any(line.startswith(f"# ATTENTION_{name}=") for line in lines), name)
        self.assertIn("# ATTENTION_TOKEN_FILE=/etc/agentc/attention-token", lines)
        self.assertIn("# ATTENTION_SMTP_TLS=starttls", " ".join(lines))
        self.assertIn("-o root -g root -m 0400", "\n".join(lines))

    def test_every_environment_variable_names_a_default_in_attention_py(self):
        source = (HERE / "attention.py").read_text()
        for line in bash("attention_env_file").splitlines():
            name = line.lstrip("# ").split("=")[0]
            if name.startswith("ATTENTION_"):
                self.assertIn(f'"{name}"', source)

    def test_uninstall_removes_units_environment_file_and_script(self):
        paths = bash("attention_paths").splitlines()
        for path in ("/etc/systemd/system/agentc-canary.service", "/etc/systemd/system/agentc-canary.timer",
                     "/etc/systemd/system/agentc-digest.service", "/etc/systemd/system/agentc-digest.timer",
                     "/etc/agentc/attention.env", "/opt/agentc/bin/attention.py"):
            self.assertIn(path, paths)
        # The owner's token is never in the list; uninstall hands it back to root.
        self.assertNotIn("/etc/agentc/attention-token", paths)

    def test_remove_attention_deletes_what_was_installed_and_keeps_the_token(self):
        root = self.dir
        (root / "units").mkdir(); (root / "etc").mkdir(); (root / "bin").mkdir(); (root / "state").mkdir()
        # has_systemd is stubbed false so no systemctl runs; ownership changes need root, so the
        # token is checked only by its survival.
        out = bash(f"""
          UNIT_DIR={root}/units ATTENTION_ENV={root}/etc/attention.env ATTENTION_SCRIPT={root}/bin/attention.py
          STATE={root}/state ATTENTION_TOKEN={root}/etc/attention-token
          has_systemd() {{ false; }}
          write_attention_units "$UNIT_DIR"
          attention_env_file > "$ATTENTION_ENV"; touch "$ATTENTION_SCRIPT" "$ATTENTION_TOKEN" "$STATE/canary-state.json"
          chown() {{ :; }}
          remove_attention
        """)
        self.assertEqual(sorted(p.name for p in root.rglob("*") if p.is_file()), ["attention-token"])
        self.assertIn("kept", out)

    def test_uninstall_calls_remove_attention_and_it_removes_the_listed_paths(self):
        uninstall = bash("declare -f uninstall")
        self.assertIn("remove_attention", uninstall)
        self.assertLess(uninstall.index("remove_attention"), uninstall.index("retire_account"))
        removal = bash("declare -f remove_attention")
        self.assertIn("attention_paths", removal)
        self.assertIn("disable --now", removal)

    def test_install_runs_after_the_run_unit_and_only_enables_ready_timers(self):
        main = bash("declare -f main")
        self.assertLess(main.index("install_run_unit"), main.index("install_attention"))
        ready = bash("declare -f attention_ready")
        self.assertIn("ATTENTION_NTFY_TOPIC", ready)

    def secure_token(self, mode=None):
        """Runs secure_attention_token on a temporary token; returns (mode, chown calls)."""
        token = self.dir / "attention-token"
        token.unlink(missing_ok=True)
        if mode is not None:
            token.write_text("token\n")
            token.chmod(mode)
        # Ownership changes need root, so chown is recorded and owned_by_root is stubbed true.
        out = bash(f"""
          ATTENTION_TOKEN={token}
          owned_by_root() {{ true; }}
          chown() {{ echo "chown $*" >&2; }}
          secure_attention_token
          [ ! -e "$ATTENTION_TOKEN" ] || stat -c %a -- "$ATTENTION_TOKEN"
        """)
        return out.strip()

    def test_the_token_is_held_root_root_0400(self):
        secure = bash("declare -f secure_attention_token")
        self.assertIn("chown root:root", secure)
        self.assertNotIn("0440", secure)
        self.assertNotIn("agentc-impl", bash("declare -f secure_attention_token attention_token_note"))

    def test_a_fresh_install_keeps_the_token_0400_and_a_missing_one_is_left_alone(self):
        self.assertEqual(self.secure_token(None), "")
        self.assertFalse((self.dir / "attention-token").exists())
        self.assertEqual(self.secure_token(0o400), "400")

    def test_a_preexisting_0440_group_readable_token_is_tightened(self):
        for mode in (0o440, 0o444, 0o640):
            self.assertEqual(self.secure_token(mode), "400")

    def test_the_token_is_installed_with_chown_to_root_root(self):
        token = self.dir / "attention-token"
        token.write_text("token\n")
        result = subprocess.run(["bash", "-c", f"""set -euo pipefail; source "$1"
          ATTENTION_TOKEN={token}; owned_by_root() {{ true; }}
          chown() {{ echo "chown $*"; }}
          secure_attention_token""", "bash", str(SCRIPT)], capture_output=True, text=True, check=True)
        self.assertEqual(result.stdout.strip(), f"chown root:root -- {token}")

    def test_the_token_note_prints_the_root_root_0400_install_command(self):
        note = bash("attention_token_note")
        self.assertIn("sudo install -o root -g root -m 0400 /dev/stdin /etc/agentc/attention-token", note)
        self.assertNotIn("0440", note)
        self.assertNotIn("agentc-impl", note)

    def test_the_units_run_as_root_so_they_still_read_the_token(self):
        for name, text in self.units().items():
            if name.endswith(".service"):
                self.assertNotIn("User=", text)

    def test_the_containment_suite_checks_roles_cannot_read_the_token(self):
        suite = (HERE / "containment-suite.sh").read_text()
        self.assertIn("check_attention_token", suite)
        self.assertIn("agentc-impl: cannot read the attention token", suite)
        self.assertIn("agentc-rev: cannot read the attention token", suite)

    def test_the_script_parses(self):
        subprocess.run(["bash", "-n", str(SCRIPT)], check=True)


class E2EUnits(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)

    def units(self):
        bash(f'write_e2e_units "{self.dir}"')
        return {p.name: p.read_text() for p in sorted(self.dir.iterdir())}

    def test_two_unit_templates_are_written(self):
        self.assertEqual(sorted(self.units()), ["agentc-e2e-canary@.service", "agentc-e2e-canary@.timer"])

    def test_the_service_runs_one_harness_per_instance_under_a_shared_lock(self):
        service = parse_unit(self.units()["agentc-e2e-canary@.service"])["Service"]
        self.assertEqual(service["Type"], "oneshot")
        self.assertEqual(service["EnvironmentFile"], "/etc/agentc/e2e-canary.env")
        self.assertEqual(service["ExecStart"], "/usr/bin/flock /var/lib/agentc/e2e-canary.lock "
                         "/usr/bin/python3 -I /opt/agentc/bin/e2e-canary.py --harness %i")
        self.assertEqual(service["ProtectSystem"], "strict")
        self.assertEqual(service["NoNewPrivileges"], "yes")
        self.assertEqual(service["ReadWritePaths"], "/var/lib/agentc")

    def test_the_timer_is_daily_and_persistent_by_default_and_overridable(self):
        timer = parse_unit(self.units()["agentc-e2e-canary@.timer"])
        self.assertEqual(timer["Timer"]["OnCalendar"], "daily")
        self.assertEqual(timer["Timer"]["Persistent"], "true")
        self.assertEqual(timer["Install"]["WantedBy"], "timers.target")
        env = {"PATH": "/usr/bin:/bin", "E2E_CALENDAR": "*-*-* 03:00:00"}
        self.assertIn("OnCalendar=*-*-* 03:00:00", bash("e2e_timer", env))

    def test_the_environment_file_asks_only_for_owner_values_and_names_code_defaults(self):
        lines = bash("e2e_env_file").splitlines()
        active = [line for line in lines if line and not line.startswith("#")]
        self.assertEqual(active, ["E2E_PROJECT=", "E2E_NTFY_TOPIC="])
        for name in ("HARNESSES", "URL", "TOKEN_FILE", "NTFY_URL", "TIMEOUT_MINUTES", "POLL_SECONDS",
                     "RESULTS", "LEDGER", "HOST"):
            self.assertTrue(any(line.startswith(f"# E2E_{name}=") for line in lines), name)
        self.assertIn("# E2E_TOKEN_FILE=/etc/agentc/e2e-canary-token", lines)

    def test_every_environment_variable_names_a_default_in_e2e_canary_py(self):
        source = (HERE / "e2e-canary.py").read_text()
        for line in bash("e2e_env_file").splitlines():
            name = line.lstrip("# ").split("=")[0]
            # E2E_HARNESSES is read by host-setup.sh itself, to pick the timers.
            if name.startswith("E2E_") and name != "E2E_HARNESSES":
                self.assertIn(f'"{name}"', source)

    def test_harnesses_default_to_claude_and_unknown_ones_are_ignored(self):
        env_file = self.dir / "e2e.env"
        self.assertEqual(bash(f'E2E_ENV={env_file}; e2e_harnesses').split(), ["claude"])
        env_file.write_text("E2E_HARNESSES=\n")
        self.assertEqual(bash(f'E2E_ENV={env_file}; e2e_harnesses').split(), ["claude"])
        env_file.write_text("E2E_PROJECT=p\nE2E_HARNESSES=codex, claude gemini\n")
        self.assertEqual(bash(f'E2E_ENV={env_file}; e2e_harnesses 2>/dev/null').split(), ["codex", "claude"])

    def test_the_timers_are_ready_only_with_token_project_and_topic(self):
        def ready(env_text, token=True):
            env_file, token_file = self.dir / "e2e.env", self.dir / "token"
            env_file.write_text(env_text)
            token_file.unlink(missing_ok=True)
            if token:
                token_file.write_text("t")
            result = subprocess.run(["bash", "-c", f'source "$1"; E2E_ENV={env_file}; E2E_TOKEN={token_file}; e2e_ready',
                                     "bash", str(SCRIPT)], capture_output=True)
            return result.returncode == 0
        self.assertTrue(ready("E2E_PROJECT=p\nE2E_NTFY_TOPIC=t\n"))
        self.assertFalse(ready("E2E_PROJECT=p\nE2E_NTFY_TOPIC=t\n", token=False))
        self.assertFalse(ready("E2E_PROJECT=\nE2E_NTFY_TOPIC=t\n"))
        self.assertFalse(ready("E2E_PROJECT=p\nE2E_NTFY_TOPIC=\n"))

    def test_uninstall_removes_units_environment_file_and_script_but_keeps_token_and_results(self):
        paths = bash("e2e_paths").splitlines()
        for path in ("/etc/systemd/system/agentc-e2e-canary@.service", "/etc/systemd/system/agentc-e2e-canary@.timer",
                     "/etc/agentc/e2e-canary.env", "/opt/agentc/bin/e2e-canary.py"):
            self.assertIn(path, paths)
        self.assertNotIn("/etc/agentc/e2e-canary-token", paths)
        self.assertNotIn("/var/lib/agentc/e2e-canary.jsonl", paths)
        uninstall = bash("declare -f uninstall")
        self.assertLess(uninstall.index("remove_e2e"), uninstall.index("retire_account"))

    def test_remove_e2e_deletes_what_was_installed_and_keeps_token_and_results(self):
        root = self.dir
        for name in ("units", "etc", "bin", "state"):
            (root / name).mkdir()
        bash(f"""
          UNIT_DIR={root}/units E2E_ENV={root}/etc/e2e.env E2E_SCRIPT={root}/bin/e2e-canary.py
          STATE={root}/state E2E_TOKEN={root}/etc/token
          has_systemd() {{ false; }}
          write_e2e_units "$UNIT_DIR"
          e2e_env_file > "$E2E_ENV"; touch "$E2E_SCRIPT" "$E2E_TOKEN" "$STATE/e2e-canary.jsonl" "$STATE/e2e-canary.lock"
          remove_e2e
        """)
        self.assertEqual(sorted(p.name for p in root.rglob("*") if p.is_file()), ["e2e-canary.jsonl", "token"])

    def test_install_runs_after_attention_and_only_enables_configured_ready_harnesses(self):
        main = bash("declare -f main")
        self.assertLess(main.index("install_attention"), main.index("install_e2e"))
        install = bash("declare -f install_e2e")
        self.assertIn("e2e_ready", install)
        self.assertIn("e2e_harnesses", install)
        self.assertIn("enable --now", install)

    def test_the_token_is_adopted_only_when_root_owned_and_held_root_only(self):
        secure = bash("declare -f secure_e2e_token")
        self.assertIn("owned_by_root", secure)
        self.assertIn("chmod 0400", secure)


class UpdaterUnits(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)

    def units(self):
        bash(f'write_update_units "{self.dir}"')
        return {p.name: p.read_text() for p in sorted(self.dir.iterdir())}

    def test_a_service_and_a_timer_are_written(self):
        self.assertEqual(sorted(self.units()), ["agentc-update.service", "agentc-update.timer"])

    def test_the_service_runs_the_updater_as_root_with_both_environment_files(self):
        service = parse_unit(self.units()["agentc-update.service"])["Service"]
        self.assertEqual(service["Type"], "oneshot")
        self.assertEqual(service["ExecStart"], "/usr/bin/python3 -I /opt/agentc/bin/agentc-update")
        self.assertNotIn("User", service)
        # Root-owned, but not sandboxed: it replaces files under /opt and /etc and runs the suite.
        self.assertNotIn("ProtectSystem", service)
        self.assertNotIn("NoNewPrivileges", service)
        text = self.units()["agentc-update.service"]
        self.assertIn("EnvironmentFile=-/etc/agentc/e2e-canary.env", text)
        self.assertIn("EnvironmentFile=-/etc/agentc/update.env", text)

    def test_the_timer_is_daily_persistent_and_overridable(self):
        timer = parse_unit(self.units()["agentc-update.timer"])
        self.assertEqual(timer["Timer"]["OnCalendar"], "daily")
        self.assertEqual(timer["Timer"]["Persistent"], "true")
        self.assertEqual(timer["Install"]["WantedBy"], "timers.target")
        env = {"PATH": "/usr/bin:/bin", "UPDATE_CALENDAR": "Sun *-*-* 04:00:00"}
        self.assertIn("OnCalendar=Sun *-*-* 04:00:00", bash("update_timer", env))

    def test_every_environment_variable_names_a_default_in_agentc_update_py(self):
        source = (HERE / "agentc-update.py").read_text()
        names = []
        for line in bash("update_env_file").splitlines():
            name = line.lstrip("# ").split("=")[0]
            if name.startswith("UPDATE_"):
                names.append(name)
                self.assertIn(f'"{name}"', source)
        self.assertIn("UPDATE_REPO", names)
        # Nothing is active: every setting has a code default.
        self.assertEqual([l for l in bash("update_env_file").splitlines() if l and not l.startswith("#")], [])

    def test_the_updater_shares_the_canary_lock_and_paths_of_the_host(self):
        source = (HERE / "agentc-update.py").read_text()
        self.assertIn('self.state_dir / "e2e-canary.lock"', source)
        self.assertIn("/var/lib/agentc/e2e-canary.lock", bash("e2e_service"))
        self.assertIn('DEFAULT_PREFIX = "/opt/agentc"', source)
        self.assertIn('DEFAULT_ETC = "/etc/agentc"', source)

    def test_uninstall_removes_the_updater_but_keeps_its_results(self):
        paths = bash("update_paths").splitlines()
        for path in ("/etc/systemd/system/agentc-update.service", "/etc/systemd/system/agentc-update.timer",
                     "/etc/agentc/update.env", "/opt/agentc/bin/agentc-update", "/var/lib/agentc/update-state.json",
                     "/var/lib/agentc/update.lock"):
            self.assertIn(path, paths)
        self.assertNotIn("/var/lib/agentc/update.jsonl", paths)
        uninstall = bash("declare -f uninstall")
        self.assertLess(uninstall.index("remove_update"), uninstall.index("retire_account"))
        self.assertLess(uninstall.index("remove_service agentc-run"), uninstall.index("remove_update"))

    def test_remove_update_deletes_what_was_installed_and_keeps_the_results(self):
        root = self.dir
        for name in ("units", "etc", "bin", "state", "prefix/releases/1.0.0"):
            (root / name).mkdir(parents=True)
        out = bash(f"""
          UNIT_DIR={root}/units UPDATE_ENV={root}/etc/update.env UPDATE_SCRIPT={root}/bin/agentc-update
          STATE={root}/state PREFIX={root}/prefix
          has_systemd() {{ false; }}
          write_update_units "$UNIT_DIR"
          update_env_file > "$UPDATE_ENV"; touch "$UPDATE_SCRIPT" "$STATE/update-state.json" "$STATE/update.lock" "$STATE/update.jsonl"
          touch "$PREFIX/releases/1.0.0/marker"
          remove_update
        """)
        self.assertEqual(sorted(p.name for p in root.rglob("*") if p.is_file()), ["update.jsonl"])
        self.assertFalse((root / "prefix/releases").exists())
        self.assertIn("kept", out)

    def test_install_runs_after_the_canary_and_enables_the_timer_only_when_the_canary_is_ready(self):
        main = bash("declare -f main")
        self.assertLess(main.index("install_e2e"), main.index("install_update"))
        install = bash("declare -f install_update")
        self.assertIn("e2e_ready", install)
        self.assertIn("UPDATE_TIMER", install)
        self.assertIn("enable --now", install)

    def test_next_steps_describe_the_updater(self):
        self.assertIn("update_note", bash("declare -f next_steps"))
        note = bash("update_note")
        self.assertIn("--rollback core", note)
        self.assertIn("gh auth login", note)

    def test_the_script_parses(self):
        subprocess.run(["bash", "-n", str(SCRIPT)], check=True)


if __name__ == "__main__":
    unittest.main()
