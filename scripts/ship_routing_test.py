#!/usr/bin/env python3
"""Routing tests for ship: real local Git configuration, mocked transports."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("ship", Path(__file__).with_name("ship.py"))
ship = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ship)


SHA = "a" * 40
FETCHED = "b" * 40
MOVED = "c" * 40


def completed(args, stdout="", returncode=0):
    return subprocess.CompletedProcess(args, returncode, stdout, "mock failure")


class RepositoryTests(unittest.TestCase):
    def test_common_github_clone_urls(self):
        for url in (
            "https://github.com/Owner/repo",
            "https://github.com/Owner/repo.git",
            "https://GITHUB.COM/Owner/repo.git/",
            "git@github.com:Owner/repo",
            "git@github.com:Owner/repo.git",
            "ssh://git@github.com/Owner/repo.git",
            "ssh://git@github.com:22/Owner/repo.git",
        ):
            with self.subTest(url=url):
                self.assertEqual(ship.repository_from_url(url), "Owner/repo")
        self.assertEqual(
            ship.repository_from_url("https://github.com/Owner/my_repo.v2-1.git"),
            "Owner/my_repo.v2-1",
        )

    def test_invalid_urls_fail_closed_without_echoing_url(self):
        for url in (
            "", "/local/repo", "https://gitlab.com/Owner/repo.git",
            "https://github.com.evil.example/Owner/repo.git",
            "http://github.com/Owner/repo.git",
            "https://secret@github.com/Owner/repo.git",
            "https://github.com:443/Owner/repo.git",
            "ssh://other@github.com/Owner/repo.git",
            "ssh://git@github.com:2222/Owner/repo.git",
            "git@github.com:/Owner/repo.git",
            "https://github.com/Owner", "https://github.com/Owner/",
            "https://github.com/Owner/repo/tree/main",
            "https://github.com/Owner/repo?other=repo",
            "https://github.com/Owner/repo#other",
            "https://github.com/Owner/re%70o",
            "https://github.com/Owner/.", "https://github.com/Owner/..",
            "https://github.com/Owner/.git", "https://github.com/Owner/..git",
            "https://g\u0131thub.com/Owner/repo.git", "https://g\u0130thub.com/Owner/repo.git",
            "git@g\u0131thub.com:Owner/repo.git", "ssh://git@G\u0130THUB.COM/Owner/repo.git",
            "https://github.co\u1d0d/Owner/repo.git", "https://github.com/Owner/re\u212apo.git",
            "https://github.com/Owner/repo.GIT", "git@github.com:Owner/repo.Git",
            "https://github.com/Owner/repo.git.git",
        ):
            with self.subTest(url=url), self.assertRaises(SystemExit) as error:
                ship.repository_from_url(url)
            self.assertEqual(
                str(error.exception),
                "ship: selected remote must have a supported GitHub clone URL",
            )

    def test_fetch_and_push_identity_may_use_different_forms_and_case(self):
        with patch.object(ship, "run", side_effect=[
            "https://github.com/Owner/Repo.git",
            "git@github.com:owner/repo.git",
        ]) as run, patch.object(ship.subprocess, "run", return_value=completed([], returncode=1)):
            self.assertEqual(ship.validated_remote("publish"), (
                "https://github.com/Owner/Repo.git",
                "git@github.com:owner/repo.git", "Owner/Repo",
            ))
        self.assertEqual(run.call_args_list[0].args,
                         ("git", "remote", "get-url", "--all", "publish"))
        self.assertEqual(run.call_args_list[1].args,
                         ("git", "remote", "get-url", "--push", "--all", "publish"))


class FastForwardTests(unittest.TestCase):
    def test_ancestry_uses_fetch_head_even_when_remote_moves(self):
        calls = []

        def fake(args, **kwargs):
            calls.append(tuple(args))
            if args[1] == "fetch":
                return completed(args)
            if args[1:3] == ("rev-parse", "FETCH_HEAD"):
                return completed(args, FETCHED)
            if args[1] == "ls-remote":
                return completed(args, f"{MOVED}\trefs/heads/main")
            if args[1] == "merge-base":
                return completed(args)
            raise AssertionError(args)

        with patch.object(ship.subprocess, "run", side_effect=fake):
            ship.require_fast_forward("publish", "main", SHA)
        self.assertEqual(calls, [
            ("git", "fetch", "--no-tags", "publish", "refs/heads/main"),
            ("git", "rev-parse", "FETCH_HEAD"),
            ("git", "merge-base", "--is-ancestor", FETCHED, SHA),
        ])

    def test_fetch_failure_stops_before_ancestry(self):
        with patch.object(ship.subprocess, "run", return_value=completed([], returncode=1)) as run:
            with self.assertRaises(SystemExit):
                ship.require_fast_forward("publish", "main", SHA)
        self.assertEqual(run.call_count, 1)

    def test_non_ancestor_and_git_error_both_refuse(self):
        for returncode in (1, 128):
            with self.subTest(returncode=returncode):
                with patch.object(ship, "run", side_effect=["", FETCHED]), patch.object(
                    ship.subprocess, "run", return_value=completed([], returncode=returncode)
                ):
                    with self.assertRaisesRegex(SystemExit, "not an ancestor"):
                        ship.require_fast_forward("publish", "main", SHA)



class RemoteTipTests(unittest.TestCase):
    """Readback must name the exact ref, never a suffix-matching decoy."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        self.env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t",
                        GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@t")
        self.work = os.path.join(directory.name, "work")
        self.bare = os.path.join(directory.name, "remote.git")
        self.git("init", "--quiet", self.work, cwd=directory.name)
        self.git("init", "--quiet", "--bare", self.bare, cwd=directory.name)
        self.real, self.decoy = self.commit("real"), self.commit("decoy")

    def git(self, *args, cwd=None):
        return subprocess.run(("git",) + args, cwd=cwd or self.work, env=self.env,
                              text=True, capture_output=True, check=True).stdout.strip()

    def commit(self, message):
        self.git("commit", "--quiet", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")

    def tip(self, branch):
        previous = os.getcwd()
        os.chdir(self.work)
        try:
            with patch.dict(os.environ, self.env, clear=True):
                return ship.remote_tip(self.bare, branch)
        finally:
            os.chdir(previous)

    def test_decoy_suffix_ref_is_ignored(self):
        self.git("push", "--quiet", self.bare, f"{self.decoy}:refs/heads/a/refs/heads/main",
                 f"{self.real}:refs/heads/main")
        self.assertEqual(self.tip("main"), self.real)

    def test_only_a_decoy_reads_as_absent(self):
        self.git("push", "--quiet", self.bare, f"{self.decoy}:refs/heads/x/refs/heads/main",
                 f"{self.decoy}:refs/heads/x/refs/heads/ac/human/topic")
        self.assertEqual(self.tip("main"), "")
        self.assertEqual(self.tip("ac/human/topic"), "")

    def test_exact_nested_branch_is_found(self):
        self.git("push", "--quiet", self.bare, f"{self.real}:refs/heads/ac/human/topic")
        self.assertEqual(self.tip("ac/human/topic"), self.real)

class ShipTestCase(unittest.TestCase):
    def invoke(self, fetch_url="https://github.com/Owner/Repo.git",
               push_url="git@github.com:Owner/Repo.git", human_tip=SHA,
               target_tip=SHA, conclusion="success", mutate_remote=False,
               local_git=None):
        self.calls = []
        polls = 0
        alias_fetch_url = fetch_url
        alias_push_url = push_url

        def fake(args, **kwargs):
            nonlocal polls, alias_fetch_url, alias_push_url
            self.calls.append(tuple(args))
            if args[0] == "gh":
                polls += 1
                if mutate_remote:
                    alias_fetch_url = "https://github.com/Other/Unchecked.git"
                    alias_push_url = "git@github.com:Other/Unchecked.git"
                runs = [] if polls == 1 else [
                    {"databaseId": index, "workflowName": name, "status": "completed",
                     "conclusion": conclusion, "createdAt": "2026-10-02T00:00:00Z"}
                    for index, name in enumerate(ship.REQUIRED_WORKFLOWS)
                ]
                return completed(args, json.dumps(runs))
            command = args[1]
            if local_git and command in ("remote", "config"):
                return local_git(args, **kwargs)
            if command == "branch":
                return completed(args, "local-branch")
            if command == "status":
                return completed(args)
            if command == "remote":
                return completed(args, alias_push_url if "--push" in args else alias_fetch_url)
            if command == "config":
                return completed(args, returncode=1)
            if command == "rev-parse":
                return completed(args, SHA if args[2] == "HEAD" else FETCHED)
            if command in ("fetch", "merge-base"):
                return completed(args)
            if command == "push":
                # A failed/lost push response is reconciled by remote readback.
                return completed(args, returncode=1)
            if command == "ls-remote":
                tip = human_tip if "ac/human/" in args[-1] else target_tip
                return completed(args, f"{tip}\t{args[-1]}" if tip else "")
            raise AssertionError(args)

        with patch.object(ship.subprocess, "run", side_effect=fake), patch.object(
            ship.time, "sleep"
        ), patch.object(ship.sys, "argv", ["ship.py", "--remote", "publish", "--name", "candidate"]), contextlib.redirect_stdout(io.StringIO()):
            ship.main()


class MainTests(ShipTestCase):
    def test_all_checks_use_selected_remote_repository_and_exact_sha(self):
        self.invoke()
        gh_calls = [args for args in self.calls if args[0] == "gh"]
        self.assertEqual(len(gh_calls), 2)
        for args in gh_calls:
            self.assertEqual(args[args.index("--repo") + 1], "github.com/Owner/Repo")
            self.assertEqual(args[args.index("--commit") + 1], SHA)
        pushes = [args for args in self.calls if args[1] == "push"]
        self.assertEqual(pushes, [
            ("git", "push", "git@github.com:Owner/Repo.git", f"{SHA}:refs/heads/ac/human/candidate"),
            ("git", "push", "git@github.com:Owner/Repo.git", f"{SHA}:refs/heads/main"),
        ])
        self.assertEqual(len([args for args in self.calls if args[1] == "fetch"]), 2)

    def test_enterprise_gh_host_cannot_redirect_check_queries(self):
        with patch.dict(os.environ, {"GH_HOST": "github.enterprise.example"}):
            self.invoke()
        gh_calls = [args for args in self.calls if args[0] == "gh"]
        self.assertTrue(gh_calls)
        for args in gh_calls:
            self.assertEqual(args[args.index("--repo") + 1], "github.com/Owner/Repo")

    def test_remote_alias_change_during_polling_cannot_redirect_git(self):
        self.invoke(mutate_remote=True)
        # Only the initial snapshot may resolve the alias; subsequent operations
        # use those URLs even if the remote now names a different repository.
        self.assertEqual(len([args for args in self.calls if args[1] == "remote"]), 2)
        network_calls = [args for args in self.calls if args[1] in ("fetch", "push", "ls-remote")]
        self.assertEqual(len(network_calls), 6)
        for args in network_calls:
            if args[1] == "fetch":
                self.assertEqual(args[3], "https://github.com/Owner/Repo.git")
            elif args[1] == "push":
                self.assertEqual(args[2], "git@github.com:Owner/Repo.git")
            else:
                self.assertEqual(args[3], "git@github.com:Owner/Repo.git")

    def test_ambiguous_or_invalid_remote_stops_before_fetch_push_and_gh(self):
        for fetch_url, push_url in (
            ("https://github.com/Owner/Repo.git", "https://github.com/Other/Repo.git"),
            ("https://gitlab.com/Owner/Repo.git", "https://github.com/Owner/Repo.git"),
            ("https://github.com/Owner/Repo.git", "https://gitlab.com/Owner/Repo.git"),
            ("https://github.com/Owner/Repo.git", ""),
            ("https://github.com/Owner/Repo.git", "git@github.com:Owner/Repo.git\ngit@github.com:Other/Repo.git"),
            ("https://github.com/Owner/Repo.git\nhttps://github.com/Other/Repo.git", "git@github.com:Owner/Repo.git"),
        ):
            with self.subTest(fetch_url=fetch_url, push_url=push_url), self.assertRaises(SystemExit):
                self.invoke(fetch_url, push_url)
            self.assertFalse(any(args[0] == "gh" or args[1] in ("fetch", "push") for args in self.calls))

    def test_human_readback_mismatch_stops_before_checks_or_target_push(self):
        with self.assertRaisesRegex(SystemExit, "does not point"):
            self.invoke(human_tip=MOVED)
        self.assertEqual(len([args for args in self.calls if args[1] == "push"]), 1)
        self.assertFalse(any(args[0] == "gh" for args in self.calls))

    def test_target_readback_mismatch_refuses_success(self):
        with self.assertRaisesRegex(SystemExit, "was not fast-forwarded"):
            self.invoke(target_tip=MOVED)

    def test_failed_checks_stop_before_target_fetch_or_push(self):
        with self.assertRaisesRegex(SystemExit, "did not succeed"):
            self.invoke(conclusion="failure")
        self.assertEqual(len([args for args in self.calls if args[1] == "push"]), 1)
        self.assertEqual(len([args for args in self.calls if args[1] == "fetch"]), 1)


class LocalGitTestCase(ShipTestCase):
    """Runs config and remote commands against a throwaway real repository."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.directory = directory.name
        self.subprocess_run = subprocess.run
        # Never inherit the caller's repository or URL rewrite configuration.
        self.git_env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.git_env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.local_git(["git", "init", "--quiet"], check=True)

    def local_git(self, args, **kwargs):
        self.assertEqual(args[0], "git")
        self.assertIn(args[1], ("init", "config", "remote"))
        return self.subprocess_run(args, cwd=self.directory, env=self.git_env, **kwargs)

    def config(self, key, value):
        self.local_git(["git", "config", "--add", key, value], check=True)

    def assert_preflight_refuses(self, message="would change", local_git=None):
        with self.assertRaisesRegex(SystemExit, message):
            self.invoke(local_git=local_git or self.local_git)
        self.assertFalse(any(
            args[0] == "gh" or args[1] in ("fetch", "push", "ls-remote")
            for args in self.calls
        ))



class RewriteTests(LocalGitTestCase):
    def setUp(self):
        super().setUp()
        self.config("remote.publish.url", "alias:Repo.git")
        self.config("url.https://github.com/Checked/.insteadOf", "alias:")

    def test_harmless_original_alias_expansion_is_allowed(self):
        self.invoke(local_git=self.local_git)
        self.assertIn(
            ("git", "fetch", "--no-tags", "https://github.com/Checked/Repo.git", "refs/heads/main"),
            self.calls,
        )
        for args in self.calls:
            if args[0] == "gh":
                self.assertEqual(args[args.index("--repo") + 1], "github.com/Checked/Repo")

    def test_second_fetch_url_rewrite_is_refused_before_transport(self):
        self.config("url.https://github.com/Unchecked/.insteadOf", "https://github.com/Checked/")
        self.assert_preflight_refuses()

    def test_second_push_url_rewrite_is_refused_before_transport(self):
        # Explicit pushurl suppresses pushInsteadOf during alias resolution,
        # but passing the resulting literal URL to push enables it again.
        self.config("remote.publish.pushurl", "https://github.com/Checked/Repo.git")
        self.config("url.https://github.com/Unchecked/.pushInsteadOf", "https://github.com/Checked/")
        self.assert_preflight_refuses()

    def test_push_url_readback_rewrite_is_refused_before_transport(self):
        self.config("remote.publish.pushurl", "push:Repo.git")
        self.config("url.git@github.com:Checked/.insteadOf", "push:")
        self.config("url.git@github.com:Unchecked/.insteadOf", "git@github.com:Checked/")
        self.assert_preflight_refuses()

    def test_unrelated_rules_and_whitespace_prefixes_are_allowed(self):
        for prefix in ("other:", " https://github.com/Checked/", "https://github.com/Checked/\n"):
            self.config("url.https://github.com/Unchecked/.insteadOf", prefix)
            self.config("url.https://github.com/Unchecked/.pushInsteadOf", prefix)
        self.invoke(local_git=self.local_git)

    def test_replacement_whitespace_is_not_discarded(self):
        self.config("url.https://github.com/Checked/ .insteadOf", "https://github.com/Checked/")
        self.assert_preflight_refuses()

    def test_no_op_rules_are_allowed(self):
        for kind in ("insteadOf", "pushInsteadOf"):
            self.config(f"url.https://github.com/Checked/.{kind}", "https://github.com/Checked/")
        self.invoke(local_git=self.local_git)

    def test_losing_changing_rule_is_conservatively_refused(self):
        self.config("url.https://github.com/Checked/.insteadOf", "https://github.com/Checked/")
        self.config("url.https://elsewhere.example/.insteadOf", "https://github.com/")
        self.assert_preflight_refuses()

    def test_push_instead_of_matching_only_fetch_url_is_allowed(self):
        self.config("remote.publish.pushurl", "git@github.com:Checked/Repo.git")
        self.config("url.https://github.com/Unchecked/.pushInsteadOf", "https://github.com/Checked/")
        self.invoke(local_git=self.local_git)

    def test_last_key_suffix_preserves_dotted_replacement_base(self):
        self.config("url.https://github.com/Checked/Repo.insteadof.git.insteadOf", "https://github.com/Checked/Repo.git")
        self.assert_preflight_refuses()

    def test_no_matching_rules_exit_one_is_allowed(self):
        self.local_git(["git", "config", "--remove-section", "url.https://github.com/Checked/"], check=True)
        self.local_git(["git", "config", "remote.publish.url", "https://github.com/Checked/Repo.git"], check=True)
        result = self.local_git(
            ["git", "config", "--null", "--get-regexp", r"^url\..*\.(insteadof|pushinsteadof)$"],
            text=True, capture_output=True,
        )
        self.assertEqual((result.returncode, result.stdout), (1, ""))
        self.invoke(local_git=self.local_git)

    def test_config_command_failure_is_closed_without_echoing_config(self):
        for returncode in (2, 3, 128):
            def failing_config(args, **kwargs):
                if args[1] == "config":
                    return completed(args, "secret configuration", returncode=returncode)
                return self.local_git(args, **kwargs)

            with self.subTest(returncode=returncode):
                self.assert_preflight_refuses("^ship: unable to inspect Git URL rewrite rules$", failing_config)

    def test_malformed_config_output_is_closed(self):
        for output in ("", "unterminated", "url.bad.insteadof\0", "url.bad.other\nprefix\0"):
            def malformed_config(args, **kwargs):
                if args[1] == "config":
                    return completed(args, output)
                return self.local_git(args, **kwargs)

            with self.subTest(output=output):
                self.assert_preflight_refuses("unable to parse", malformed_config)


class RemoteAliasTests(LocalGitTestCase):
    """A remote named like a snapshot URL makes Git re-resolve that URL."""

    def setUp(self):
        super().setUp()
        self.config("remote.publish.url", "https://github.com/Checked/Repo.git")

    def resolved(self, url):
        """Asks real Git where a literal URL argument would fetch from."""
        result = self.subprocess_run(["git", "ls-remote", "--get-url", url], cwd=self.directory,
                                     env=self.git_env, text=True, capture_output=True, check=True)
        return result.stdout.strip()

    def test_remote_named_as_fetch_url_is_refused_before_transport(self):
        self.config("remote.https://github.com/Checked/Repo.git.url", "https://github.com/Unchecked/Repo.git")
        # Prove the counterexample is real Git behavior, not a mock artifact.
        self.assertEqual(self.resolved("https://github.com/Checked/Repo.git"),
                         "https://github.com/Unchecked/Repo.git")
        self.assert_preflight_refuses("remote named like a selected URL")

    def test_pushurl_only_alias_is_refused_before_transport(self):
        # ls-remote --get-url cannot see this; only the config scan does.
        self.config("remote.https://github.com/Checked/Repo.git.pushurl", "https://github.com/Unchecked/Repo.git")
        self.assert_preflight_refuses("remote named like a selected URL")

    def test_remote_named_as_push_url_is_refused_before_transport(self):
        self.config("remote.publish.pushurl", "git@github.com:Checked/Repo.git")
        self.config("remote.git@github.com:Checked/Repo.git.url", "git@github.com:Unchecked/Repo.git")
        self.assert_preflight_refuses("remote named like a selected URL")

    def test_alias_with_any_setting_or_case_is_conservatively_refused(self):
        self.config("remote.HTTPS://GITHUB.COM/Checked/Repo.git.proxy", "")
        self.assert_preflight_refuses("remote named like a selected URL")

    def test_other_remotes_including_dotted_names_are_allowed(self):
        self.config("remote.mirror.url", "https://github.com/Checked/Repo.git")
        self.config("remote.a.b.url", "https://github.com/Unchecked/Repo.git")
        self.config("remote.https://github.com/Checked/Other.git.url", "https://github.com/Unchecked/Repo.git")
        self.invoke(local_git=self.local_git)

    def test_section_level_remote_settings_are_allowed(self):
        self.config("remote.pushDefault", "publish")
        self.invoke(local_git=self.local_git)

    def test_remote_config_failure_or_malformed_output_is_closed(self):
        for returncode, output, message in ((128, "secret", "unable to inspect Git remote"),
                                            (0, "remote.publish.url", "unable to parse Git remote"),
                                            (0, "other.publish.url\0", "unable to parse Git remote")):
            def remote_config(args, **kwargs):
                if args[1] == "config" and "--name-only" in args:
                    return completed(args, output, returncode=returncode)
                return self.local_git(args, **kwargs)

            with self.subTest(output=output):
                self.assert_preflight_refuses(message, remote_config)


if __name__ == "__main__":
    unittest.main()
