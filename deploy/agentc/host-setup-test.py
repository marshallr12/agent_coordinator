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
                     "STATE", "MAX_HRI", "HOURS", "SMTP_PORT", "MAIL_FROM"):
            self.assertTrue(any(line.startswith(f"# ATTENTION_{name}=") for line in lines), name)
        self.assertIn("# ATTENTION_TOKEN_FILE=/etc/agentc/attention-token", lines)

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

    def test_the_token_is_held_root_owned_and_role_readable_only(self):
        secure = bash("declare -f secure_attention_token")
        self.assertIn('chown "root:$ATTENTION_TOKEN_GROUP"', secure)
        self.assertIn("chmod 0440", secure)
        self.assertEqual(bash('echo "$ATTENTION_TOKEN_GROUP"').strip(), "agentc-impl")

    def test_the_script_parses(self):
        subprocess.run(["bash", "-n", str(SCRIPT)], check=True)


if __name__ == "__main__":
    unittest.main()
