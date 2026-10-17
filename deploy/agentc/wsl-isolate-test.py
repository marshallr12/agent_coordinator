#!/usr/bin/env python3
"""Unprivileged tests of WSL mount propagation and supervisor boot gating."""
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('wsl-isolate.sh')


class Isolation(unittest.TestCase):
    def test_mounts_are_private_before_any_unmount(self):
        with tempfile.TemporaryDirectory() as directory:
            code = r'''
source "$1"
trace=$2/trace
mount() { printf '%s\n' "mount:$*" >> "$trace"; }
# Track two stacked mounts without touching the real mount namespace.
count=0
mountpoint() { [ "$3" = /mnt/wsl ] && [ "$count" -lt 2 ]; }
umount() { printf '%s\n' "umount:$*" >> "$trace"; count=$((count + 1)); }
isolate_mounts
cat "$trace"
'''
            result = subprocess.run(['bash', '-c', code, 'test', str(SCRIPT), directory],
                                    capture_output=True, text=True, check=True)
            self.assertEqual(result.stdout.splitlines(), [
                'mount:--make-rprivate /',
                'umount:--recursive -- /mnt/wsl',
                'umount:--recursive -- /mnt/wsl'])

    def test_busy_mount_failure_is_not_ignored(self):
        code = r'''
source "$1"
mount() { :; }
mountpoint() { [ "$3" = /mnt/wsl ]; }
umount() { return 23; }
isolate_mounts
echo incorrectly-continued
'''
        result = subprocess.run(['bash', '-c', code, 'test', str(SCRIPT)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 23)
        self.assertNotIn('incorrectly-continued', result.stdout)

    def test_supervisor_requires_successful_isolation_at_boot(self):
        with tempfile.TemporaryDirectory() as directory:
            subprocess.run(['bash', '-c', 'source "$1"; write_units "$2"',
                            'test', str(SCRIPT), directory], check=True)
            unit = Path(directory, 'agentc-wsl-isolation.service').read_text()
            gate = Path(directory, 'agentc-run.service.d/wsl-isolation.conf').read_text()
            self.assertIn('RemainAfterExit=yes', unit)
            self.assertIn('Requires=agentc-wsl-isolation.service', gate)
            self.assertIn('After=agentc-wsl-isolation.service', gate)


if __name__ == '__main__':
    unittest.main()
