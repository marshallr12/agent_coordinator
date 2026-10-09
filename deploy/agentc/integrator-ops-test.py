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
                    CREATE TABLE tasks(current_attempt_id);
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

    def test_preflight_rejects_unprojected_task_and_review_recovery(self):
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
                    CREATE TABLE tasks(current_attempt_id);
                    CREATE TABLE attempts(id,state,expires_at);
                    INSERT INTO workflow_activities VALUES ('review','active');
                    INSERT INTO tasks VALUES ('expired-review'), ('expired-task');
                    INSERT INTO attempts VALUES ('expired-review','active',0),
                                                ('expired-task','active',0);
                ''')
            result = subprocess.run([sys.executable, str(ROOT / 'integrator-preflight.py'),
                                     '--database', str(path)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            counts = json.loads(result.stdout)
            self.assertEqual(counts['recovery'], 0)
            self.assertEqual(counts['current_attempts'], 2)


class HostSetup(unittest.TestCase):
    """integrator-host-setup.sh gives the canary project an integrator instance of its own."""

    def units(self):
        text = (ROOT / 'integrator-host-setup.sh').read_text()
        function = 'write_unit() {' + text.split('write_unit() {')[1].split('\n}\n')[0] + '\n}\n'
        with tempfile.TemporaryDirectory() as tmp:
            function = function.replace('/etc/systemd/system', tmp)
            subprocess.run(['bash', '-c', function + 'write_unit agentc-integrator integrator.toml daemon.lock\n'
                            'write_unit agentc-integrator-canary integrator-canary.toml daemon-canary.lock\n'],
                           check=True)
            return {p.name: p.read_text() for p in Path(tmp).iterdir()}, text

    def test_each_unit_template_has_its_own_configuration_and_lock(self):
        units, _ = self.units()
        self.assertEqual(sorted(units), ['agentc-integrator-canary@.service', 'agentc-integrator@.service'])
        main = next(l for l in units['agentc-integrator@.service'].splitlines() if l.startswith('ExecStart='))
        canary = next(l for l in units['agentc-integrator-canary@.service'].splitlines() if l.startswith('ExecStart='))
        self.assertEqual(main, 'ExecStart=/usr/bin/flock --nonblock /var/lib/agentc/integrator/daemon.lock '
                               '/opt/agentc/bin/agentc-integrator --config /etc/agentc/integrator.toml %i')
        self.assertEqual(canary, 'ExecStart=/usr/bin/flock --nonblock /var/lib/agentc/integrator/daemon-canary.lock '
                                 '/opt/agentc/bin/agentc-integrator --config /etc/agentc/integrator-canary.toml %i')
        for unit in units.values():  # everything else is shared
            self.assertIn('User=agentc-integrator', unit)
            self.assertIn('ReadWritePaths=/var/lib/agentc/integrator\n', unit)

    def test_the_canary_configuration_keeps_its_state_and_credentials_apart(self):
        _, text = self.units()
        config = text.split("integrator-canary.toml <<'CONFIG'")[1].split('\nCONFIG')[0]
        self.assertIn('projects = []', config)  # the owner inserts the canary project
        self.assertIn('state_dir = "/var/lib/agentc/integrator/canary"', config)
        self.assertNotIn('agentc-integrator@', config)
        self.assertIn('agentc-integrator-canary@shadow.service', config)


if __name__ == '__main__':
    unittest.main()
