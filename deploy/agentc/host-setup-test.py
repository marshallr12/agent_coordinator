#!/usr/bin/env python3
"""Tests for host-setup.sh: the attention, end-to-end canary and updater
timers, and the sysvinit parts (agentc-run init script and its restart
wrapper, cron jobs run by agentc-cron, quiet hours).

Runs without root or systemd: it sources the script (which then defines its
functions and does nothing else) and checks the unit files it would write, the
environment file and the --uninstall list.
"""
import os
import subprocess
import tempfile
import time
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
        self.assertIn("designate-digest-sender", note)
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

    def test_each_role_gets_generated_instructions_not_an_empty_claude_md(self):
        body = SCRIPT.read_text().split("install_seeds() {", 1)[1].split("\n}\n", 1)[0]
        generated = 'agentc-supervisor" instructions --role "$name" > "$temp"'
        install = 'mv -fT -- "$temp" "$path/CLAUDE.md"'
        self.assertIn(generated, body)
        self.assertLess(body.index(generated), body.index(install))
        self.assertEqual(body.count("mktemp"), 3)  # settings, CLAUDE.md, Cargo seed


class CanaryProject(unittest.TestCase):
    """The second project binding's host side: its mirror and its config template."""

    def test_the_mirror_is_only_kept_for_a_host_that_names_a_canary_repository(self):
        with tempfile.TemporaryDirectory() as state:
            env = {"PATH": "/usr/bin:/bin", "STATE": state}
            self.assertEqual(bash("STATE=" + state + "; refresh_canary_mirror", env), "")
            self.assertEqual(list(Path(state).iterdir()), [])

    def test_the_config_template_shows_the_second_binding_and_its_push_configuration(self):
        text = SCRIPT.read_text()
        for line in ("# [run.canary_binding]", "# service_url = ", "# project_id = ", "# mirror = ",
                     "# [push_helper.project_configs]"):
            self.assertIn(line, text)
        self.assertIn("mirror-canary.git", text.split("remove_own_paths() {")[1])  # uninstall removes it


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
        for name in ("HARNESSES", "URL", "TOKEN_FILE", "NTFY_URL", "PRIORITY", "TIMEOUT_MINUTES",
                     "POLL_SECONDS", "RESULTS", "LEDGER", "HOST"):
            self.assertTrue(any(line.startswith(f"# E2E_{name}=") for line in lines), name)
        self.assertIn("# E2E_TOKEN_FILE=/etc/agentc/e2e-canary-token", lines)
        self.assertTrue(any(line.startswith("# E2E_PRIORITY=0 ") for line in lines))  # the in-code default

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


WRAPPER = HERE / "agentc-run-sysv.sh"
CRON = HERE / "agentc-cron.sh"
QUIET = HERE / "quiet-hours.sh"


def run(args, env=None, timeout=20):
    """CompletedProcess of `args` with captured text output."""
    return subprocess.run(args, capture_output=True, text=True, env=env, timeout=timeout)


class SysvRunScript(unittest.TestCase):
    """The agentc-run LSB script and its install on hosts without systemd."""

    def test_the_script_starts_after_the_firewall_and_proxy_and_parses(self):
        script = bash("run_sysv_script")
        self.assertIn("# Required-Start:    $network $remote_fs agentc-firewall agentc-egress", script)
        self.assertIn("# Required-Stop:     $network $remote_fs agentc-firewall agentc-egress", script)
        with tempfile.NamedTemporaryFile("w", suffix=".sh") as f:
            f.write(script); f.flush()
            subprocess.run(["sh", "-n", f.name], check=True)

    def test_start_runs_the_wrapper_and_every_call_matches_its_process_name(self):
        script = bash("run_sysv_script")
        self.assertIn("exec /opt/agentc/bin/agentc-run-sysv /opt/agentc/bin/agentc-supervisor 30 >>/var/log/agentc-run.log", script)
        self.assertIn("--retry TERM/120/KILL/5", script)
        for action in ("--start --oknodo", "--stop", "--status"):
            line = next(l for l in script.splitlines() if f"start-stop-daemon {action}" in l)
            self.assertIn("--name agentc-run-sysv", line, action)
        # dash's kill rejects "--"; the group is named by its negative pid.
        self.assertIn('kill -KILL "-$group"', script)
        self.assertNotIn("kill -KILL --", script)

    @unittest.skipUnless(Path("/sbin/start-stop-daemon").exists(), "needs start-stop-daemon")
    def test_the_script_drains_kills_the_leftover_group_and_ignores_a_reused_pid(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            initd = self.simulated_init_script(d)
            self.assertEqual(run(["sh", str(initd), "start"]).returncode, 0)
            RunWrapper.wait_for(self, d / "launch")
            self.assertEqual(run(["sh", str(initd), "status"]).returncode, 0)
            self.assertEqual(run(["sh", str(initd), "start"]).returncode, 0, "a second start is not an error")
            self.assertEqual(run(["sh", str(initd), "stop"]).returncode, 0)
            self.assertEqual((d / "drained").read_text(), "drained\n")
            launch = int((d / "launch").read_text())
            RunWrapper.wait_until(self, lambda: not Path(f"/proc/{launch}").exists())
            self.assertNotEqual(run(["sh", str(initd), "status"]).returncode, 0)
            (d / "pid").write_text(f"{os.getpid()}\n")  # a stale pidfile naming another process
            self.assertNotEqual(run(["sh", str(initd), "status"]).returncode, 0)
            run(["sh", str(initd), "stop"])
            self.assertTrue(Path(f"/proc/{os.getpid()}").exists())

    def simulated_init_script(self, d):
        """The real init script with its pidfile and log moved into `d`, running
        the wrapper against a fake loop that drains on SIGTERM and leaves a
        TERM-ignoring child in its process group."""
        (d / "bin").mkdir()
        (d / "agentc-run-sysv").write_text(WRAPPER.read_text())
        (d / "agentc-run-sysv").chmod(0o755)
        fake = d / "bin" / "agentc-supervisor"
        fake.write_text(f"#!/bin/sh\nsh -c 'trap \"\" TERM; echo $$ > {d}/launch; exec sleep 600' &\n"
                        f"trap 'sleep 1; echo drained > {d}/drained; exit 0' TERM\nwhile :; do sleep 0.1; done\n")
        fake.chmod(0o755)
        script = bash(f'PREFIX="{d}"; RUN_WRAPPER="{d}/agentc-run-sysv"; run_sysv_script')
        script = (script.replace("/run/agentc-run.pid", f"{d}/pid").replace("/var/log/agentc-run.log", f"{d}/log")
                  .replace("start-stop-daemon", "/sbin/start-stop-daemon"))
        (d / "initd").write_text(script)
        return d / "initd"

    def test_without_systemd_the_script_is_installed_but_never_enabled_or_started(self):
        self.assertIn("install_run_sysv", bash("declare -f install_run_unit"))
        body = bash("declare -f install_run_sysv")
        self.assertNotIn("update-rc.d", body)
        self.assertNotIn("/etc/init.d/agentc-run start", body)
        self.assertNotIn("service agentc-run", body)
        self.assertIn("update-rc.d agentc-run defaults", bash("has_systemd() { false; }; run_opt_in"))
        self.assertIn("systemctl enable --now agentc-run", bash("has_systemd() { true; }; run_opt_in"))

    def test_uninstall_removes_the_wrapper_and_the_script(self):
        self.assertIn("agentc-run-sysv", bash("declare -f remove_own_paths"))
        self.assertIn("remove_service agentc-run", bash("declare -f uninstall"))


class RunWrapper(unittest.TestCase):
    """agentc-run-sysv against a fake supervisor."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)

    def fake(self, body):
        """A fake supervisor whose `run` executes shell `body` in self.dir."""
        path = self.dir / "supervisor"
        path.write_text(f"#!/bin/sh\ncd '{self.dir}'\n{body}\n")
        path.chmod(0o755)
        return str(path)

    def wait_for(self, path, seconds=5):
        """Waits until `path` exists, failing after `seconds`."""
        RunWrapper.wait_until(self, path.exists, seconds)

    def wait_until(self, condition, seconds=5):
        """Waits until `condition()` is true, failing after `seconds`."""
        deadline = time.monotonic() + seconds
        while not condition():
            self.assertLess(time.monotonic(), deadline, "condition never held")
            time.sleep(0.05)

    def test_a_crash_restarts_the_loop_and_a_clean_exit_ends_the_wrapper(self):
        sup = self.fake("n=$(cat runs 2>/dev/null || echo 0); echo $((n+1)) > runs; [ $n -ge 1 ] || exit 3")
        result = run(["sh", str(WRAPPER), sup, "0"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.dir / "runs").read_text().strip(), "2")
        self.assertIn("exited 3; restarting in 0s", result.stdout)
        self.assertIn("exited cleanly", result.stdout)

    def test_sigterm_reaches_the_loop_and_the_wrapper_waits_for_its_drain(self):
        sup = self.fake("trap 'sleep 1; echo drained > drained; exit 0' TERM; echo up > up; while :; do sleep 0.1; done")
        proc = subprocess.Popen(["sh", str(WRAPPER), sup, "0"], stdout=subprocess.PIPE, text=True)
        self.wait_for(self.dir / "up")
        proc.terminate()
        out, _ = proc.communicate(timeout=10)
        self.assertEqual(proc.returncode, 0)
        self.assertTrue((self.dir / "drained").exists(), "the wrapper exited before the loop drained")
        self.assertIn("stopped (exit 0)", out)

    def test_sigterm_during_the_restart_delay_stops_without_restarting(self):
        sup = self.fake("echo run >> runs; exit 1")
        proc = subprocess.Popen(["sh", str(WRAPPER), sup, "30"], stdout=subprocess.PIPE, text=True)
        self.wait_for(self.dir / "runs")
        time.sleep(0.3)
        proc.terminate()
        out, _ = proc.communicate(timeout=5)
        self.assertEqual(proc.returncode, 0)
        self.assertEqual((self.dir / "runs").read_text().count("run"), 1)
        self.assertIn("stopped while waiting to restart", out)


class CronJobs(unittest.TestCase):
    """Timers as cron entries on hosts without systemd."""

    READY = "attention_ready() { true; }; e2e_ready() { true; }; e2e_harnesses() { echo claude; echo codex; }; "

    def test_systemd_schedules_translate_to_cron(self):
        cases = {"10min": "*/10 * * * *", "5min": "*/5 * * * *", "2h": "0 */2 * * *", "hourly": "0 * * * *",
                 "daily": "0 0 * * *", "weekly": "0 0 * * 1", "*-*-* 07:30:00": "30 7 * * *", "*-*-* 23:05": "5 23 * * *"}
        for value, cron in cases.items():
            self.assertEqual(bash(f"cron_schedule '{value}'").strip(), cron, value)

    def test_schedules_without_a_cron_form_fail_setup(self):
        for value in ("7min", "0min", "5h", "Mon *-*-* 10:00", "monthly", "soon",
                      "*-*-* 25:00", "*-*-* 02:00 UTC", "*-*-* 02:00,14:00", "*-*-* 7:00"):
            result = run(["bash", "-c", f'source "$1"; cron_schedule "{value}"', "bash", str(SCRIPT)])
            self.assertNotEqual(result.returncode, 0, value)
        result = run(["bash", "-c", 'source "$1"; CANARY_INTERVAL=7min; check_cron_schedules', "bash", str(SCRIPT)])
        self.assertEqual(result.returncode, 1)
        self.assertIn("CANARY_INTERVAL='7min' has no cron form", result.stderr)

    def test_ready_jobs_become_entries_run_by_agentc_cron(self):
        lines = bash(self.READY + "cron_entries").splitlines()
        runner = "root /opt/agentc/bin/agentc-cron --name"
        e2e = "--env /etc/agentc/e2e-canary.env -- /usr/bin/flock /var/lib/agentc/e2e-canary.lock /usr/bin/python3 -I /opt/agentc/bin/e2e-canary.py --harness"
        self.assertEqual(lines, [
            f"*/10 * * * * {runner} canary --env /etc/agentc/attention.env -- /usr/bin/python3 -I /opt/agentc/bin/attention.py canary",
            f"0 0 * * * {runner} digest --env /etc/agentc/attention.env -- /usr/bin/python3 -I /opt/agentc/bin/attention.py digest",
            f"0 0 * * * {runner} e2e-canary-claude {e2e} claude",
            f"0 0 * * * {runner} e2e-canary-codex {e2e} codex",
        ])
        self.assertNotIn("agentc-update", bash(self.READY + "UPDATE_TIMER=1; cron_entries"))

    def test_jobs_that_are_not_ready_get_no_entry(self):
        self.assertEqual(bash("attention_ready() { false; }; e2e_ready() { false; }; cron_entries"), "")
        only_digest = bash('attention_ready() { [ "$1" = agentc-digest ]; }; e2e_ready() { false; }; cron_entries')
        self.assertEqual([line.split(" --name ")[1].split()[0] for line in only_digest.splitlines()], ["digest"])

    def test_systemd_hosts_get_no_cron_file(self):
        out = bash("has_systemd() { true; }; install() { echo INSTALL; }; rm() { echo RM; }; install_cron_jobs")
        self.assertEqual(out, "")

    def test_the_cron_file_sets_a_shell_and_path_and_setup_and_uninstall_wire_it_in(self):
        self.assertIn("SHELL=/bin/sh\nPATH=/usr/sbin:/usr/bin:/sbin:/bin\nLINE\n", bash("cron_file 'LINE'"))
        main = bash("declare -f main")
        self.assertLess(main.index("install_update"), main.index("install_cron_jobs"))
        self.assertLess(main.index("install_cron_jobs"), main.index("install_quiet_hours"))
        self.assertLess(main.index("install_quiet_hours"), main.index("next_steps"))
        uninstall = bash("declare -f uninstall")
        for step in ("remove_cron_jobs", "quiet_hours_off"):
            self.assertIn(step, uninstall)
        self.assertIn('"$CRON_FILE" "$CRON_RUNNER"', bash("declare -f remove_cron_jobs"))


class CronRunner(unittest.TestCase):
    """agentc-cron: environment files, logging and exit status."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.env = {"PATH": "/usr/bin:/bin", "AGENTC_CRON_LOG_DIR": str(self.dir)}

    def cron(self, *args):
        """Runs agentc-cron with `args`, logging into self.dir."""
        return run(["sh", str(CRON), *args], env=self.env)

    def log(self, name):
        """The text of job `name`'s log."""
        return (self.dir / f"agentc-{name}.log").read_text()

    def test_environment_files_load_like_systemd_without_expansion(self):
        envfile = self.dir / "job.env"
        envfile.write_text("# comment\n\nPLAIN=a b c\nQUOTED=\"x y\"\nSINGLE='z'\nLITERAL=$HOME `id`\n"
                           "# COMMENTED=1\nbad-key=1\nLONELY\n  SPACED = trimmed  \nEMPTY=\nLAST=no newline")
        result = self.cron("--name", "job", "--env", str(envfile), "--", "env")
        self.assertEqual(result.returncode, 0)
        log = self.log("job")
        for line in ("PLAIN=a b c", "QUOTED=x y", "SINGLE=z", "LITERAL=$HOME `id`", "SPACED=trimmed",
                     "EMPTY=", "LAST=no newline"):
            self.assertIn(f"\n{line}\n", log, line)
        self.assertNotIn("COMMENTED=", log)
        self.assertNotIn("LONELY=", log)
        self.assertIn("ignoring line", log)

    def test_the_exit_status_is_logged_and_returned(self):
        result = self.cron("--name", "job", "--", "sh", "-c", "echo out; exit 4")
        self.assertEqual(result.returncode, 4)
        log = self.log("job")
        self.assertIn("agentc-job: start: sh -c echo out; exit 4", log)
        self.assertIn("\nout\n", log)
        self.assertIn("agentc-job: exit 4", log)

    def test_a_missing_required_environment_file_fails_and_an_optional_one_is_skipped(self):
        missing = str(self.dir / "absent.env")
        self.assertEqual(self.cron("--name", "job", "--env", missing, "--", "true").returncode, 1)
        self.assertIn("missing environment file", self.log("job"))
        self.assertEqual(self.cron("--name", "opt", "--env-optional", missing, "--", "true").returncode, 0)

    def test_name_must_come_first_and_be_a_plain_word(self):
        for args in ([], ["--", "true"], ["--name", "../x", "--", "true"], ["--name", "", "--", "true"]):
            self.assertEqual(self.cron(*args).returncode, 2, args)
        self.assertEqual(list(self.dir.iterdir()), [])


class QuietHours(unittest.TestCase):
    """agentc-quiet-hours and its install."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.switch = self.dir / "kill-switch"

    def quiet(self, window, now):
        """Applies `window` as if the local time were `now`."""
        return run(["sh", str(QUIET), window, str(self.switch)], env={"PATH": "/usr/bin:/bin", "QUIET_HOURS_NOW": now})

    def install(self, quiet_hours, extra=""):
        """Runs install_quiet_hours with paths in self.dir, a fake install, a
        cron daemon reported present whatever the host has, and shell code
        `extra` run first."""
        setting = "unset QUIET_HOURS" if quiet_hours is None else f"QUIET_HOURS='{quiet_hours}'"
        code = (f'install() {{ local dst="${{@: -1}}" src="${{@: -2:1}}"; cat "$src" > "$dst"; chmod 0755 "$dst"; }}; '
                f'QUIET_SCRIPT="{self.dir}/agentc-quiet-hours"; QUIET_FILE="{self.dir}/cron-quiet"; '
                f'KILL_SWITCH="{self.switch}"; e2e_ready() {{ false; }}; has_cron_daemon() {{ true; }}; {extra}{setting}; install_quiet_hours')
        return run(["bash", "-c", f'set -euo pipefail; source "$1"; {code}', "bash", str(SCRIPT)])

    def test_outside_the_window_it_holds_the_switch_and_inside_it_releases_it(self):
        for now, held in (("12:00", True), ("21:59", True), ("22:00", False), ("23:30", False),
                          ("00:00", False), ("06:59", False), ("07:00", True)):
            self.assertEqual(self.quiet("22:00-07:00", now).returncode, 0)
            self.assertEqual(self.switch.exists(), held, now)
        self.quiet("09:00-17:00", "08:00")
        self.assertEqual(self.switch.read_text(), "quiet-hours\n")
        self.quiet("09:00-17:00", "12:00")
        self.assertFalse(self.switch.exists())

    def test_a_switch_the_owner_set_is_never_removed(self):
        for content in ("", "owner\n", "quiet-hours and more\n"):
            self.switch.write_text(content)
            self.quiet("00:00-23:59", "12:00")
            self.assertEqual(self.switch.read_text(), content)
            run(["sh", str(QUIET), "--release", str(self.switch)])
            self.assertEqual(self.switch.read_text(), content)

    def test_release_drops_only_its_own_switch_and_never_follows_a_symlink(self):
        self.quiet("09:00-17:00", "08:00")
        run(["sh", str(QUIET), "--release", str(self.switch)])
        self.assertFalse(self.switch.exists())
        target = self.dir / "target"
        self.switch.symlink_to(target)
        self.quiet("09:00-17:00", "08:00")
        self.assertFalse(target.exists())
        self.assertTrue(self.switch.is_symlink())

    def test_malformed_windows_are_refused(self):
        for window in ("22-07", "24:00-07:00", "22:60-07:00", "07:00-07:00", "nonsense", "7:00-9:00"):
            result = self.quiet(window, "12:00")
            self.assertEqual(result.returncode, 2, window)
            self.assertFalse(self.switch.exists(), window)

    def test_install_writes_the_minute_check_and_unset_keeps_while_empty_turns_it_off(self):
        result = self.install("00:00-00:01")
        self.assertEqual(result.returncode, 0, result.stderr)
        cron = (self.dir / "cron-quiet").read_text()
        self.assertIn(f"* * * * * root {self.dir}/agentc-quiet-hours 00:00-00:01 {self.switch}", cron)
        self.assertEqual(self.install(None).returncode, 0)
        self.assertTrue((self.dir / "cron-quiet").exists())
        self.switch.write_text("quiet-hours\n")
        self.assertEqual(self.install("").returncode, 0)
        self.assertFalse((self.dir / "cron-quiet").exists())
        self.assertFalse(self.switch.exists())

    def test_install_fails_without_a_cron_daemon_before_touching_the_switch(self):
        result = self.install("09:00-09:01", extra="has_cron_daemon() { false; }; ")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("need a cron daemon", result.stderr)
        self.assertFalse(self.switch.exists())
        self.assertFalse((self.dir / "cron-quiet").exists())

    def test_the_cron_daemon_check_looks_on_path_for_cron_or_crond(self):
        bin_dir = self.dir / "bin"
        bin_dir.mkdir()
        check = f'source "$1"; PATH="{bin_dir}"; has_cron_daemon'
        self.assertNotEqual(run(["bash", "-c", check, "bash", str(SCRIPT)]).returncode, 0)
        for name in ("crond", "cron"):
            daemon = bin_dir / name
            daemon.write_text("#!/bin/sh\n")
            daemon.chmod(0o755)
            self.assertEqual(run(["bash", "-c", check, "bash", str(SCRIPT)]).returncode, 0, name)
            daemon.unlink()

    def test_jobs_that_need_claiming_outside_the_window_are_warned_about(self):
        code = (f'QUIET_SCRIPT="{QUIET}"; e2e_ready() {{ true; }}; has_systemd() {{ true; }}; UPDATE_TIMER=1; ')
        warned = run(["bash", "-c", f'source "$1"; {code} QUIET_HOURS=09:00-17:00; quiet_hours_conflicts', "bash", str(SCRIPT)])
        self.assertIn("end-to-end canary starts at 00:00", warned.stderr)
        self.assertIn("updater starts at 00:00", warned.stderr)
        quiet = run(["bash", "-c", f'source "$1"; {code} QUIET_HOURS=22:00-07:00; quiet_hours_conflicts', "bash", str(SCRIPT)])
        self.assertEqual(quiet.stderr, "")
        self.assertEqual(bash('calendar_start "*-*-* 07:30"').strip(), "07:30")

    def test_install_refuses_a_bad_window_before_writing_anything(self):
        self.assertNotEqual(self.install("25:00-07:00").returncode, 0)
        self.assertFalse((self.dir / "cron-quiet").exists())


if __name__ == "__main__":
    unittest.main()
