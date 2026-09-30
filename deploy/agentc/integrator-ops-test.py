#!/usr/bin/env python3
"""Regression checks for cutover evidence gates; entirely local fixtures."""
import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent


class CutoverGates(unittest.TestCase):
    def flip(self, mutate=lambda jobs: None):
        names = ['Linux format, Clippy, and workspace tests', 'Audit locked dependencies',
                 'Pinned mdBook build and local-link validation']
        jobs = [{'id': i * 20 + n, 'name': name, 'head_sha': 'abc',
                 'status': 'completed', 'conclusion': 'success'}
                for i, name in enumerate(names) for n in range(20)]
        mutate(jobs)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'jobs.json'
            path.write_text(json.dumps([{'jobs': jobs}]))
            return subprocess.run([sys.executable, str(ROOT / 'integrator-flip-rate.py'),
                                   '--sha', 'abc', str(path)], capture_output=True, text=True)

    def test_twenty_passes_and_one_failure_boundary(self):
        self.assertEqual(self.flip().returncode, 0)
        failed = self.flip(lambda jobs: jobs[0].update(conclusion='failure'))
        self.assertEqual(failed.returncode, 1)
        self.assertEqual(json.loads(failed.stdout)['checks'][
            'Linux format, Clippy, and workspace tests']['rate'], 0.05)

    def test_wrong_sha_and_duplicate_attempts_fail(self):
        self.assertNotEqual(self.flip(lambda jobs: jobs[0].update(head_sha='other')).returncode, 0)
        self.assertEqual(self.flip(lambda jobs: jobs[0].update(id=1)).returncode, 1)

    def test_pending_and_canceled_fail_closed(self):
        self.assertEqual(self.flip(lambda jobs: jobs[0].update(status='in_progress')).returncode, 1)
        self.assertEqual(self.flip(lambda jobs: jobs[0].update(conclusion='cancelled')).returncode, 1)

    def test_preflight_rejects_unknown_jobs_and_preserves_database(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'db.sqlite'
            with sqlite3.connect(path) as db:
                db.executescript('''
                    CREATE TABLE publication_intents(activity_id);
                    CREATE TABLE integration_results(activity_id,publication_state);
                    CREATE TABLE publication_reconciliations(activity_id);
                    CREATE TABLE integration_holds(state);
                    CREATE TABLE reservations(state);
                    CREATE TABLE jobs(state);
                    CREATE TABLE workflow_activities(kind,state);
                ''')
            command = [sys.executable, str(ROOT / 'integrator-preflight.py'), '--database', str(path)]
            self.assertEqual(subprocess.run(command, capture_output=True).returncode, 0)
            with sqlite3.connect(path) as db:
                db.execute("INSERT INTO jobs VALUES ('unknown')")
                db.execute("INSERT INTO workflow_activities VALUES ('review','recovery_required')")
                db.execute("INSERT INTO publication_intents VALUES ('unresolved')")
            before = path.read_bytes()
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            counts = json.loads(result.stdout)
            self.assertEqual((counts['jobs'], counts['recovery'], counts['publication_intents']), (1, 1, 1))
            self.assertEqual(path.read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
