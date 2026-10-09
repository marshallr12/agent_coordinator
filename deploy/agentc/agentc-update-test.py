#!/usr/bin/env python3
"""End-to-end tests for agentc-update.py against a fake host and a fake release server.

The host is a temporary prefix, state and etc directory with a fake systemctl, a fake
supervisor (its preflight outcome is written into the script), a fake canary and a fake
containment suite. The release server is a loopback HTTP server. No root or systemd needed.
"""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, HERE / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


update = load("agentc_update", "agentc-update.py")

REPO = "owner/agentc"
OLD = "1.0.0"
NEW = "1.1.0"

SYSTEMCTL = """#!/bin/sh
echo "systemctl $*" >> "$FAKE/events"
case "$1" in
  is-active) [ "$(cat "$FAKE/unit")" = active ] ;;
  stop) echo inactive > "$FAKE/unit" ;;
  start) [ -e "$FAKE/start-fails" ] && exit 1; echo active > "$FAKE/unit" ;;
esac
"""

# $FAKE_PREFIX is the host prefix. The canary and the suite judge the files that are live.
CANARY = """#!/bin/sh
echo "canary $1" >> "$FAKE/events"
if grep -q CANARY_BAD "$FAKE_PREFIX/bin/agentc-supervisor"; then echo "task not done" >&2; exit 1; fi
"""

SUITE = """#!/bin/sh
echo "suite" >> "$FAKE/events"
if grep -q SUITE_BAD "$FAKE_PREFIX/bin/claude"; then echo "FAIL claude: cannot write the mirror"; exit 1; fi
echo "PASS all"
"""


def supervisor(version, preflight_ok=True, canary_ok=True):
    return f"""#!/bin/sh
# {version} {'' if canary_ok else 'CANARY_BAD'}
case "$1" in
  clone) while [ $# -gt 0 ]; do [ "$1" = --dest ] && mkdir -p "$2"; shift; done ;;
  prepare) while [ $# -gt 0 ]; do [ "$1" = --run ] && run=$2; shift; done
    [ -f "$run/prompt.md" ] || {{ echo "no prompt" >&2; exit 1; }} ;;
  preflight) echo "preflight {version}" >> "$FAKE/events"
    {'' if preflight_ok else 'echo problems >&2; exit 1'} ;;
esac
exit 0
"""


def claude(version, suite_ok=True):
    return f'#!/bin/sh\n# {"" if suite_ok else "SUITE_BAD"}\necho "claude {version}"\n'


def write_exec(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755)


def bundle(version, files):
    """A release archive's bytes and the SHA256SUMS line for it; `files` maps names to text."""
    root = f"agentc-host-{version}-linux-{update.target_arch()}"
    members = dict(files)
    members["SHA256SUMS"] = "".join(
        f"{hashlib.sha256(text.encode()).hexdigest()}  {name}\n" for name, text in sorted(files.items()))
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        for name, text in sorted(members.items()):
            info = tarfile.TarInfo(f"{root}/{name}")
            info.size = len(text.encode())
            info.mode = 0o755
            archive.addfile(info, io.BytesIO(text.encode()))
    return buffer.getvalue()


def release_files(version, sup=None, harness=None):
    files = {"bin/agentc-supervisor": sup or supervisor(version), "bin/agent-coordinator": f"#!/bin/sh\n# cli {version}\n",
             "bin/agentc-push": f"#!/bin/sh\n# push {version}\n"}
    if harness:
        files["harness.json"] = json.dumps({n: {"file": f"harness/{n}", "version": v} for n, (v, _) in harness.items()})
        for name, (_, text) in harness.items():
            files[f"harness/{name}"] = text
        files["scripts/containment-suite.sh"] = SUITE
    return files


class Releases:
    """A loopback server for /repos/<repo>/releases/latest and the assets it lists."""

    def __init__(self):
        self.tag = "v" + NEW
        self.assets = {}
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                if self.path == f"/repos/{REPO}/releases/latest":
                    base = f"http://127.0.0.1:{outer.server.server_port}/dl/"
                    body = json.dumps({"tag_name": outer.tag, "draft": False, "prerelease": False,
                                       "assets": [{"name": n, "browser_download_url": base + n} for n in outer.assets]})
                elif self.path.startswith("/dl/") and self.path[4:] in outer.assets:
                    body = outer.assets[self.path[4:]]
                else:
                    self.send_error(404)
                    return
                data = body if isinstance(body, bytes) else body.encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def url(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def publish(self, version, files, corrupt_sums=False):
        archive = bundle(version, files)
        name = update.bundle_name(version)
        digest = hashlib.sha256(b"tampered" if corrupt_sums else archive).hexdigest()
        self.tag = "v" + version
        self.assets = {name: archive, "SHA256SUMS": f"{digest}  {name}\n"}

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class Host(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.fake, self.prefix, self.state, self.etc = root / "fake", root / "opt", root / "var", root / "etc"
        for path in (self.fake, self.prefix / "bin", self.state, self.etc):
            path.mkdir(parents=True)
        for path in (root, self.prefix, self.prefix / "bin", self.state, self.etc):
            path.chmod(0o755)
        (self.fake / "unit").write_text("active")
        write_exec(self.fake / "systemctl", SYSTEMCTL)
        write_exec(self.fake / "canary", CANARY)
        write_exec(self.prefix / "bin/agentc-supervisor", supervisor("host"))
        write_exec(self.prefix / "bin/agent-coordinator", "#!/bin/sh\n# cli host\n")
        write_exec(self.prefix / "bin/agentc-push", "#!/bin/sh\n# push host\n")
        write_exec(self.prefix / "bin/claude", claude(OLD))
        self.config = self.etc / "supervisor.toml"
        self.config.write_text('bin_dir = "/opt/agentc"\n\n[pinned]\nclaude = "%s"\ncodex = "0.9"\n\n[push_helper]\nprogram = "x"\n' % OLD)
        mirror = self.state / "mirror.git"
        subprocess.run(["git", "init", "-q", "-b", "main", str(mirror)], check=True)
        subprocess.run(["git", "-C", str(mirror), "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q",
                        "--allow-empty", "-m", "x"], check=True)
        subprocess.run(["git", "-C", str(mirror), "remote", "add", "origin", "https://example.invalid/x.git"], check=True)
        self.releases = Releases()
        self.addCleanup(self.releases.close)
        patched = mock.patch.dict(os.environ, {"FAKE": str(self.fake), "FAKE_PREFIX": str(self.prefix)})
        patched.start()
        self.addCleanup(patched.stop)
        for name in [n for n in os.environ if n.startswith(("UPDATE_", "E2E_"))]:
            os.environ.pop(name)

    def run_update(self, *extra, attest="true {file} {repo}"):
        argv = ["--repo", REPO, "--api-url", self.releases.url(), "--allow-insecure-loopback",
                "--attest-command", attest, "--prefix", str(self.prefix), "--state-dir", str(self.state),
                "--etc", str(self.etc), "--systemctl", str(self.fake / "systemctl"), "--as-impl", "env",
                "--canary-command", f"{self.fake / 'canary'} {{harness}}", "--canary-harnesses", "claude",
                "--drain-timeout-minutes", "0.02", "--settle-seconds", "0", "--poll-seconds", "0.01", *extra]
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            code = update.main(argv)
        self.stderr = stderr.getvalue()
        return code

    def events(self):
        path = self.fake / "events"
        return path.read_text().splitlines() if path.exists() else []

    def live(self, name):
        return (self.prefix / "bin" / name).read_text()

    def unit_state(self):
        return (self.fake / "unit").read_text().strip()

    def results(self):
        return [json.loads(line) for line in (self.state / "update.jsonl").read_text().splitlines()]

    def kill_switch(self):
        return (self.state / "kill-switch").exists()


class GoodAndBadReleases(Host):
    def test_a_good_release_is_promoted_after_drain_switch_preflight_and_canary(self):
        self.releases.publish(NEW, release_files(NEW))
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertIn(f"# {NEW}", self.live("agentc-supervisor"))
        self.assertIn(f"cli {NEW}", self.live("agent-coordinator"))
        self.assertEqual(self.unit_state(), "active")
        self.assertFalse(self.kill_switch())
        events = [e for e in self.events() if not e.startswith("systemctl is-active")]
        self.assertEqual(events, ["systemctl stop agentc-run.service", f"preflight {NEW}",
                                  "systemctl start agentc-run.service", "canary claude"])
        state = json.loads((self.state / "update-state.json").read_text())
        self.assertEqual(state["core"]["version"], NEW)
        self.assertTrue((self.prefix / "releases" / NEW / "bin/agentc-supervisor").is_file())
        self.assertEqual(self.results()[-1]["outcome"], "ok")
        # The old binaries were saved side by side for a rollback.
        saved = Path(state["core"]["previous_dir"]) / "bin/agentc-supervisor"
        self.assertIn("# host", saved.read_text())

    def test_a_second_run_with_nothing_new_changes_nothing(self):
        self.releases.publish(NEW, release_files(NEW))
        self.assertEqual(self.run_update(), 0, self.stderr)
        before = self.events()
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertEqual(self.events(), before)
        self.assertEqual(self.results()[-1]["outcome"], "current")

    def test_a_failing_canary_rolls_back_and_the_release_is_not_retried(self):
        self.releases.publish(NEW, release_files(NEW, supervisor(NEW, canary_ok=False)))
        self.assertEqual(self.run_update(), 1, self.stderr)
        self.assertIn("# host", self.live("agentc-supervisor"))
        self.assertIn("cli host", self.live("agent-coordinator"))
        self.assertEqual(self.unit_state(), "active")
        self.assertFalse(self.kill_switch())
        events = [e for e in self.events() if not e.startswith("systemctl is-active")]
        self.assertEqual(events, ["systemctl stop agentc-run.service", f"preflight {NEW}",
                                  "systemctl start agentc-run.service", "canary claude",
                                  "systemctl stop agentc-run.service", "systemctl start agentc-run.service"])
        self.assertEqual(self.results()[-1]["outcome"], "rejected")
        self.assertIn("rejected and rolled back", self.stderr)
        # The timer does not retry a rejected release; --retry does.
        before = self.events()
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertEqual(self.events(), before)
        self.assertEqual(self.run_update("--retry"), 1, self.stderr)

    def test_a_failing_preflight_rolls_back_before_the_supervisor_restarts(self):
        self.releases.publish(NEW, release_files(NEW, supervisor(NEW, preflight_ok=False)))
        self.assertEqual(self.run_update(), 1, self.stderr)
        self.assertIn("# host", self.live("agentc-supervisor"))
        self.assertNotIn("canary claude", self.events())
        self.assertEqual(self.unit_state(), "active")
        self.assertFalse(self.kill_switch())

    def test_a_rollback_that_cannot_restart_the_loop_is_reported_as_failed(self):
        self.releases.publish(NEW, release_files(NEW))
        (self.fake / "start-fails").write_text("")
        self.assertEqual(self.run_update(), 2, self.stderr)
        self.assertIn("ROLLBACK FAILED", self.stderr)
        self.assertIn("# host", self.live("agentc-supervisor"))

    def test_a_running_launch_defers_the_update_without_touching_the_host(self):
        self.releases.publish(NEW, release_files(NEW))
        (self.state / "heartbeat.json").write_text(json.dumps({"iteration": 3, "launch": {"role": "implementer", "task": "t"}}))
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertEqual(self.results()[-1]["outcome"], "deferred")
        self.assertIn("# host", self.live("agentc-supervisor"))
        self.assertEqual(self.unit_state(), "active")
        self.assertNotIn("systemctl stop agentc-run.service", self.events())
        self.assertFalse(self.kill_switch())

    def test_an_owners_kill_switch_stops_the_update_and_stays(self):
        self.releases.publish(NEW, release_files(NEW))
        (self.state / "kill-switch").write_text("")
        self.assertEqual(self.run_update(), 2, self.stderr)
        self.assertTrue(self.kill_switch())
        self.assertIn("# host", self.live("agentc-supervisor"))

    def test_an_inactive_loop_stops_the_update(self):
        self.releases.publish(NEW, release_files(NEW))
        (self.fake / "unit").write_text("inactive")
        self.assertEqual(self.run_update(), 2, self.stderr)
        self.assertIn("not active", self.stderr)

    def test_check_reports_without_changing_anything(self):
        self.releases.publish(NEW, release_files(NEW))
        self.assertEqual(self.run_update("--check"), 0, self.stderr)
        self.assertIn("would update", self.stderr)
        self.assertIn("# host", self.live("agentc-supervisor"))
        self.assertEqual(self.events(), [])

    def test_a_manual_rollback_restores_the_saved_binaries(self):
        self.releases.publish(NEW, release_files(NEW))
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertEqual(self.run_update("--rollback", "core"), 0, self.stderr)
        self.assertIn("# host", self.live("agentc-supervisor"))
        self.assertEqual(self.unit_state(), "active")
        self.assertFalse(self.kill_switch())
        # The release rolled back from is not reinstalled by the next timer tick.
        before = self.events()
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertEqual(self.events(), before)


class Verification(Host):
    def assert_untouched(self):
        self.assertIn("# host", self.live("agentc-supervisor"))
        self.assertNotIn("systemctl stop agentc-run.service", self.events())
        self.assertFalse(self.kill_switch())

    def test_a_release_without_a_valid_attestation_is_refused_before_the_drain(self):
        self.releases.publish(NEW, release_files(NEW))
        self.assertEqual(self.run_update(attest="false {file} {repo}"), 2, self.stderr)
        self.assertIn("attestation", self.stderr)
        self.assert_untouched()

    def test_a_missing_attestation_verifier_refuses_the_release(self):
        self.releases.publish(NEW, release_files(NEW))
        self.assertEqual(self.run_update(attest="/nonexistent/gh {file}"), 2, self.stderr)
        self.assert_untouched()

    def test_an_archive_that_does_not_match_sha256sums_is_refused(self):
        self.releases.publish(NEW, release_files(NEW), corrupt_sums=True)
        self.assertEqual(self.run_update(), 2, self.stderr)
        self.assertIn("SHA256SUMS", self.stderr)
        self.assert_untouched()

    def test_a_bundle_whose_own_manifest_is_wrong_is_refused(self):
        files = release_files(NEW)
        archive = bundle(NEW, files)
        # Rebuild with one file changed after its hash was listed.
        buffer = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(archive)) as source, tarfile.open(fileobj=buffer, mode="w:gz") as out:
            for member in source.getmembers():
                data = source.extractfile(member).read()
                if member.name.endswith("bin/agentc-push"):
                    data = b"#!/bin/sh\n# swapped\n"
                member.size = len(data)
                out.addfile(member, io.BytesIO(data))
        name = update.bundle_name(NEW)
        tampered = buffer.getvalue()
        self.releases.tag = "v" + NEW
        self.releases.assets = {name: tampered, "SHA256SUMS": f"{hashlib.sha256(tampered).hexdigest()}  {name}\n"}
        self.assertEqual(self.run_update(), 2, self.stderr)
        self.assertIn("SHA256SUMS", self.stderr)
        self.assert_untouched()

    def test_a_bundle_path_escaping_its_directory_is_refused(self):
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as out:
            info = tarfile.TarInfo("root/../../escape")
            info.size = 1
            out.addfile(info, io.BytesIO(b"x"))
        with self.assertRaises(update.Failed):
            archive = Path(self.tmp.name) / "bad.tar.gz"
            archive.write_bytes(buffer.getvalue())
            update.unpack(archive, Path(self.tmp.name) / "out")
        self.assertFalse((Path(self.tmp.name) / "escape").exists())

    def test_a_release_url_must_be_https(self):
        settings = mock.Mock(allow_insecure_loopback=False)
        with self.assertRaises(update.Failed):
            update.http_get(settings, "http://example.invalid/x", 10)
        update.check_url("http://127.0.0.1:1/x", True)
        with self.assertRaises(update.Failed):
            update.check_url("http://example.invalid/x", True)


class HarnessUpdates(Host):
    def publish(self, version=NEW, claude_text=None, **kwargs):
        text = claude_text or claude("2.0")
        self.releases.publish(version, release_files(version, harness={"claude": ("2.0", text)}, **kwargs))

    def test_a_good_harness_is_staged_checked_and_promoted_with_its_pin(self):
        self.publish()
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertIn("claude 2.0", self.live("claude"))
        pins = self.config.read_text()
        self.assertIn('claude = "2.0"', pins)
        self.assertIn('codex = "0.9"', pins)
        self.assertIn('[push_helper]\nprogram = "x"', pins)
        events = [e for e in self.events() if not e.startswith("systemctl is-active")]
        # Core: stop, preflight, start, canary. Harness: stop, preflight, suite, start, canary.
        self.assertEqual(events[4:], ["systemctl stop agentc-run.service", f"preflight {NEW}", "suite",
                                      "systemctl start agentc-run.service", "canary claude"])
        self.assertEqual(self.results()[-1]["outcome"], "ok")
        self.assertIn("harness promoted", self.results()[-1]["detail"])
        self.assertFalse(self.kill_switch())

    def test_a_harness_failing_the_containment_suite_is_reverted_but_the_core_stays(self):
        self.publish(claude_text=claude("2.0", suite_ok=False))
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertIn(f"# {NEW}", self.live("agentc-supervisor"))
        self.assertIn(f"claude {OLD}", self.live("claude"))
        self.assertIn(f'claude = "{OLD}"', self.config.read_text())
        self.assertNotIn("canary claude", self.events()[-2:])
        self.assertEqual(self.unit_state(), "active")
        self.assertFalse(self.kill_switch())
        self.assertIn("harness reverted", self.results()[-1]["detail"])
        self.assertIn("containment-suite.sh failed", self.stderr)
        # Not retried every tick.
        before = self.events()
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertEqual(self.events(), before)

    def test_a_harness_that_misreports_its_version_is_never_pinned(self):
        self.publish(claude_text="#!/bin/sh\necho claude 9.9\n")
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertIn(f"claude {OLD}", self.live("claude"))
        self.assertIn(f'claude = "{OLD}"', self.config.read_text())

    def test_a_failing_core_canary_skips_the_harness_stage(self):
        self.publish(sup=supervisor(NEW, canary_ok=False))
        self.assertEqual(self.run_update(), 1, self.stderr)
        self.assertIn(f"claude {OLD}", self.live("claude"))
        self.assertNotIn("suite", self.events())

    def test_a_manual_harness_rollback_restores_binary_and_pin(self):
        self.publish()
        self.assertEqual(self.run_update(), 0, self.stderr)
        self.assertEqual(self.run_update("--rollback", "harness"), 0, self.stderr)
        self.assertIn(f"claude {OLD}", self.live("claude"))
        self.assertIn(f'claude = "{OLD}"', self.config.read_text())
        self.assertEqual(self.unit_state(), "active")


class Helpers(unittest.TestCase):
    def test_set_pins_replaces_only_the_pinned_table_entries(self):
        text = 'claude = "outer"\n[pinned]\nclaude = "1"\ncodex = "2"\n[other]\ncodex = "keep"\n'
        out = update.set_pins(text, {"claude": "3"})
        self.assertEqual(out, 'claude = "outer"\n[pinned]\nclaude = "3"\ncodex = "2"\n[other]\ncodex = "keep"\n')

    def test_set_pins_adds_a_missing_entry_and_table(self):
        self.assertIn('codex = "5"', update.set_pins('[pinned]\nclaude = "1"\n[x]\na = 1\n', {"codex": "5"}).split("[x]")[0])
        self.assertTrue(update.set_pins("a = 1\n", {"claude": "2"}).endswith('[pinned]\nclaude = "2"\n'))

    def test_versions_compare_numerically_and_never_downgrade(self):
        self.assertTrue(update.newer("v1.10.0", "1.9.0"))
        self.assertFalse(update.newer("v1.0.0", "1.0.0"))
        self.assertFalse(update.newer("v0.9.0", "1.0.0"))
        self.assertTrue(update.newer("v0.0.1", None))
        self.assertFalse(update.newer("nightly", None))

    def test_every_setting_has_a_code_default(self):
        with mock.patch.dict(os.environ, clear=True):
            args = update.parse([])
        settings = update.Settings(args)
        self.assertEqual((settings.repo, str(settings.prefix), str(settings.etc)),
                         (update.DEFAULT_REPO, "/opt/agentc", "/etc/agentc"))
        self.assertTrue(settings.canary_is_default and settings.harness_updates)
        self.assertEqual(settings.drain_seconds, update.DEFAULT_DRAIN_MINUTES * 60)

    def test_settings_come_from_update_variables(self):
        with mock.patch.dict(os.environ, {"UPDATE_REPO": "a/b", "UPDATE_KEEP": "5", "UPDATE_HARNESS": "0"}, clear=True):
            settings = update.Settings(update.parse([]))
        self.assertEqual((settings.repo, settings.keep, settings.harness_updates), ("a/b", 5, False))


if __name__ == "__main__":
    unittest.main()
