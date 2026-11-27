#!/usr/bin/env python3
"""Tests for how ../gcp-backup-transfer.py opens the backup repository's lock.

The script lives one directory up; this file lives here because the CI job
"Deploy script tests" runs every deploy/agentc/*-test.py by its name alone.
"""
import fcntl
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load(name, file):
    """Imports a script whose file name is not a valid module name."""
    spec = importlib.util.spec_from_file_location(name, file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


transfer = load("gcp_backup_transfer", HERE.parent / "gcp-backup-transfer.py")


class RepositoryLockTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.repository = Path(directory.name)
        self.lock = self.repository / ".lock"

    def refusal(self):
        """The one-line message the transfer exits with instead of taking the lock."""
        with self.assertRaises(SystemExit) as raised:
            transfer.open_repository_lock(self.repository)
        message = raised.exception.code
        self.assertIsInstance(message, str)  # a string code prints to stderr and exits 1
        self.assertNotIn("\n", message)
        return message

    def held(self, path, mode):
        """A second holder of `path`, as the backup service would be."""
        holder = open(path, "a")
        self.addCleanup(holder.close)
        fcntl.flock(holder, mode | fcntl.LOCK_NB)
        return holder

    def test_a_missing_lock_is_named_and_nothing_is_created(self):
        message = self.refusal()
        self.assertIn(str(self.lock), message)
        self.assertIn("No backup repository lock", message)
        self.assertEqual(list(self.repository.iterdir()), [])

    def test_a_missing_repository_is_refused_the_same_way(self):
        self.repository = self.repository / "absent"
        self.assertIn("No backup repository lock", self.refusal())
        self.assertFalse(self.repository.exists())

    def test_a_symlink_is_refused_and_its_target_is_not_locked(self):
        target = self.repository / "elsewhere"
        target.touch()
        self.lock.symlink_to(target)
        self.assertIn("symbolic link", self.refusal())
        self.held(target, fcntl.LOCK_EX)  # still free: the refusal locked nothing

    def test_a_directory_or_fifo_is_refused_as_not_a_regular_file(self):
        self.lock.mkdir()
        self.assertIn("not a regular file", self.refusal())
        self.lock.rmdir()
        os.mkfifo(self.lock)
        self.assertIn("not a regular file", self.refusal())

    def test_an_existing_lock_is_opened_read_only_and_left_as_it_was(self):
        self.lock.touch(mode=0o600)
        before = self.lock.stat()
        with transfer.open_repository_lock(self.repository) as lock:
            self.assertEqual(os.get_blocking(lock.fileno()), False)
            self.assertEqual(fcntl.fcntl(lock, fcntl.F_GETFL) & os.O_ACCMODE, os.O_RDONLY)
        after = self.lock.stat()
        self.assertEqual((before.st_mode, before.st_uid, before.st_ino), (after.st_mode, after.st_uid, after.st_ino))

    def test_the_shared_lock_waits_for_an_exclusive_holder(self):
        self.lock.touch()
        self.held(self.lock, fcntl.LOCK_EX)
        with transfer.open_repository_lock(self.repository) as lock:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)

    def test_the_shared_lock_coexists_with_another_shared_holder(self):
        self.lock.touch()
        self.held(self.lock, fcntl.LOCK_SH)
        with transfer.open_repository_lock(self.repository) as lock:
            fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
            with open(self.lock, "a") as writer:
                with self.assertRaises(BlockingIOError):  # and it does exclude a writer
                    fcntl.flock(writer, fcntl.LOCK_EX | fcntl.LOCK_NB)


class ImportTests(unittest.TestCase):
    def test_importing_the_script_starts_no_transfer(self):
        """Loading the module above must not create state or take locks."""
        self.assertTrue(callable(transfer.main))
        self.assertEqual(transfer.state, Path("/var/lib/agent-coordinator-transfer"))


if __name__ == "__main__":
    unittest.main()
