#!/usr/bin/env python3
"""Fail-closed WSL host bring-up; run from a trusted Linux-filesystem checkout."""
import argparse
import configparser
import hashlib
import json
import os
import pwd
from pathlib import Path
import shutil
import subprocess
import sys
import time

INSTALLED = Path('/opt/agentc/bin/agentc-wsl-host')
RECEIPT = Path('/etc/agentc/wsl-containment.json')
UID_RESERVATION = Path('/etc/agentc/wsl-uids.json')
ROLES = ('agentc-impl', 'agentc-rev', 'agentc-egress', 'agentc-push')
BINARIES = ('agentc-supervisor', 'agent-coordinator', 'agentc-push', 'claude', 'codex')


def command(argv):
    return subprocess.run(argv, check=False, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True, timeout=30)


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def configuration_errors(config):
    """These are distro settings, not changes to the shared WSL VM."""
    expected = {'boot': {'systemd': 'true'},
                'automount': {'enabled': 'false', 'mountfstab': 'false'},
                'interop': {'enabled': 'false', 'appendwindowspath': 'false'}}
    errors = []
    for section, values in expected.items():
        for key, value in values.items():
            if config.get(section, key, fallback='').lower() != value:
                errors.append(f'/etc/wsl.conf needs [{section}] {key}={value}')
    try:
        if config.getint('boot', 'initTimeout', fallback=0) < 180000:
            errors.append('/etc/wsl.conf needs [boot] initTimeout=180000 or greater')
    except ValueError:
        errors.append('/etc/wsl.conf initTimeout must be an integer')
    return errors


def networking_errors(mode, returncode=0):
    if returncode or mode.strip().lower() != 'nat':
        return ['actual WSL networking mode must be NAT (wslinfo --networking-mode); '
                'a configured NAT value does not prove that WSL avoided fallback']
    return []


def role_uid_errors(reserved, observed):
    if not isinstance(reserved, dict) or set(reserved) != set(ROLES):
        return ['reserve all four agentc role UIDs for this Windows host']
    values = list(reserved.values())
    if any(type(value) is not int or value < 100000 or value >= 2**32 - 1 for value in values):
        return ['use explicitly reserved high role UIDs, not distro-default system UIDs']
    if len(set(values)) != len(values) or reserved != observed:
        return ['role accounts must match four distinct Windows-host-wide reserved UIDs']
    return []


def preflight():
    errors = []
    if os.geteuid() != 0:
        return ['run with sudo from the Linux owner account']
    if 'microsoft' not in os.uname().release.lower() or 'wsl2' not in os.uname().release.lower():
        errors.append('this helper requires the WSL2 Linux kernel')
    if Path('/proc/1/comm').read_text().strip() != 'systemd':
        errors.append('PID 1 must be systemd')
    config = configparser.ConfigParser()
    config.read('/etc/wsl.conf')
    errors.extend(configuration_errors(config))
    if shutil.which('wslinfo') is None:
        errors.append('wslinfo is required to verify the actual WSL networking mode')
    else:
        mode = command(['wslinfo', '--networking-mode'])
        errors.extend(networking_errors(mode.stdout, mode.returncode))
    if command(['systemctl', 'is-active', '--quiet', 'agentc-wsl-isolation.service']).returncode:
        errors.append('install and start the WSL mount-isolation boot guard before admission')
    try:
        metadata = UID_RESERVATION.lstat()
        if (UID_RESERVATION.is_symlink() or not UID_RESERVATION.is_file()
                or metadata.st_uid != 0 or metadata.st_mode & 0o022
                or metadata.st_nlink != 1 or metadata.st_size > 16384):
            raise ValueError('unsafe reservation')
        reserved = json.loads(UID_RESERVATION.read_text())
        observed = {}
        for role in ROLES:
            try:
                observed[role] = pwd.getpwnam(role).pw_uid
            except KeyError:
                pass
        errors.extend(role_uid_errors(reserved, observed))
    except (OSError, ValueError):
        errors.append('reserve unused role UIDs across all WSL distros; record root-owned /etc/agentc/wsl-uids.json')
    interop = Path('/proc/sys/fs/binfmt_misc/WSLInterop')
    if interop.exists() and interop.read_text().startswith('enabled'):
        errors.append('Windows executable interop is still enabled; restart the distro')
    # Windows files and Docker Desktop sockets must not be reachable by either uid.
    mounts = command(['findmnt', '--json', '--list', '-o', 'TARGET,FSTYPE,OPTIONS'])
    if mounts.returncode:
        errors.append('cannot inspect mounted filesystems')
    else:
        for mount in json.loads(mounts.stdout)['filesystems']:
            target = mount['target']
            if (mount['fstype'] in ('9p', 'drvfs', 'virtiofs')
                    or target.startswith(('/mnt/c', '/mnt/wsl', '/mnt/wslg'))):
                errors.append(f'foreign/shared mount remains at {target}; use a dedicated distro')
    apparmor = Path('/sys/module/apparmor/parameters/enabled')
    if not apparmor.exists() or apparmor.read_text().strip() != 'Y':
        errors.append('AppArmor is inactive; enable it in the WSL kernel before setup')
    for argv, label in ((['nft', 'list', 'tables'], 'nftables'),
                        (['unshare', '--net', '--', '/bin/true'], 'root network namespace')):
        if shutil.which(argv[0]) is None or command(argv).returncode:
            errors.append(f'{label} probe failed')
    if not Path('/usr/bin/bwrap').is_file():
        errors.append('install distribution Bubblewrap 0.8 or newer')
    return errors


def require_preflight():
    errors = preflight()
    for error in errors:
        print(f'FAIL {error}', file=sys.stderr)
    if errors:
        raise SystemExit('WSL host refused; fix the failures or use a Linux host. Containment is unchanged.')
    print('PASS WSL host prerequisites (the full containment suite is still required)')


def fingerprint():
    return {'kernel': os.uname().release,
            'controller': digest(INSTALLED),
            'wsl_conf': digest(Path('/etc/wsl.conf')),
            'role_uids': digest(UID_RESERVATION),
            'binaries': {name: digest(Path('/opt/agentc/bin') / name) for name in BINARIES}}


def require_receipt():
    stat = RECEIPT.lstat()
    if RECEIPT.is_symlink() or stat.st_uid != 0 or stat.st_mode & 0o022:
        raise SystemExit('unsafe containment receipt; rerun verify')
    receipt = json.loads(RECEIPT.read_text())
    if receipt['fingerprint'] != fingerprint():
        raise SystemExit('host changed since containment verification; rerun verify')


def install(source):
    require_preflight()
    source = source.resolve(strict=True)
    fs = command(['findmnt', '-n', '-o', 'FSTYPE', '-T', str(source)])
    if fs.returncode or fs.stdout.strip() not in ('ext4', 'btrfs', 'xfs'):
        raise SystemExit('source checkout must be on a Linux filesystem')
    status = command(['git', '-C', str(source), 'status', '--porcelain'])
    if status.returncode or status.stdout:
        raise SystemExit('commit the trusted source before setup')
    # Preserve host-setup's owner, harness pinning, uid/firewall and manual secrets flow.
    owner = os.environ.get('SUDO_USER')
    if not owner or owner == 'root':
        raise SystemExit('use sudo from the Linux owner account')
    for name in ('claude', 'codex'):
        binary = Path(pwd.getpwnam(owner).pw_dir) / '.local/bin' / name
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise SystemExit(f'install owner-pinned Linux {name} before host setup')
    environment = os.environ.copy()
    environment['APPARMOR_BWRAP'] = '1'
    subprocess.run(['bash', str(source / 'deploy/agentc/host-setup.sh')],
                   env=environment, cwd=source, check=True)
    subprocess.run(['install', '-o', 'root', '-g', 'root', '-m', '0755',
                    str(source / 'deploy/agentc/wsl-host.py'), str(INSTALLED)], check=True)
    print('Installed. Complete host-setup manual steps, then run verify before registering the Windows task.')


def verify(source):
    require_preflight()
    # Invalidate first: a failed rerun must never leave an earlier success usable.
    RECEIPT.unlink(missing_ok=True)
    source = source.resolve(strict=True)
    suite = source / 'deploy/agentc/containment-suite.sh'
    revision = command(['git', '-C', str(source), 'rev-parse', 'HEAD'])
    status = command(['git', '-C', str(source), 'status', '--porcelain'])
    if revision.returncode or status.returncode or status.stdout:
        raise SystemExit('verify requires clean committed source')
    log = Path('/var/log/agentc-wsl-containment.log')
    # root opens a new file exclusively; never follow a planted symlink.
    if log.exists() or log.is_symlink():
        raise SystemExit(f'preserve and move the previous {log} before rerunning')
    with open(log, 'x', opener=lambda path, flags: os.open(path, flags, 0o600)) as output:
        result = subprocess.run(['bash', str(suite), '--cargo-test'], cwd=source,
                                stdout=output, stderr=subprocess.STDOUT, check=False)
    text = log.read_text()
    print(f'containment exit={result.returncode}; log={log}')
    if result.returncode or 'FAIL' in text or 'SKIP' in text:
        raise SystemExit('containment incomplete; inspect the private log; no startup receipt issued')
    receipt = {'fingerprint': fingerprint(), 'source_revision': revision.stdout.strip(),
               'suite_sha256': digest(suite), 'log_sha256': digest(log),
               'verified_at': int(time.time())}
    with open(RECEIPT, 'x', opener=lambda path, flags: os.open(path, flags, 0o600)) as output:
        json.dump(receipt, output, indent=2)
    print(f'PASS full containment; receipt={RECEIPT}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('check', 'ready', 'install', 'verify', 'run', 'stop'))
    parser.add_argument('--source', type=Path, default=Path.cwd())
    args = parser.parse_args()
    if args.action == 'check':
        require_preflight()
    elif args.action == 'ready':
        require_preflight()
        require_receipt()
    elif args.action == 'install':
        install(args.source)
    elif args.action == 'verify':
        verify(args.source)
    elif args.action == 'run':
        require_preflight()
        require_receipt()
        subprocess.run(['systemctl', 'start', 'agentc-run.service'], check=True)
        # A foreground WSL client keeps the distro alive; systemd services alone do not.
        while True:
            subprocess.run(['systemctl', 'is-active', '--quiet', 'agentc-run.service'], check=True)
            time.sleep(15)
    elif args.action == 'stop':
        if os.geteuid() != 0:
            raise SystemExit('run with sudo')
        subprocess.run(['systemctl', 'stop', 'agentc-run.service'], check=True)
        result = command(['systemctl', 'show', 'agentc-run.service', '-p', 'Result', '--value'])
        if result.returncode or result.stdout.strip() != 'success':
            raise SystemExit('drain did not finish cleanly; inspect jobs and holds before shutdown')


if __name__ == '__main__':
    main()
