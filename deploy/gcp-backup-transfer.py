#!/usr/bin/env python3
"""Archive, upload, download-verify, and retain complete coordinator snapshots."""
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tarfile
import tempfile
import time

repository = Path('/var/lib/agent-coordinator-backups')
state = Path('/var/lib/agent-coordinator-transfer')
bucket = 'gs://sithbit-19b44-agent-coordinator-backups-east'
binary = '/usr/local/bin/agent-coordinator-server'

def run(args):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if result.returncode:
        # Neither CLI diagnostic streams nor authenticated URLs are logged.
        raise RuntimeError(f'{Path(args[0]).name} operation failed with exit {result.returncode}')
    return result.stdout

def step(name):
    """One journal line per step, so a pressure reading can be matched to the step that caused it."""
    print(f'step {name} at {time.strftime("%H:%M:%S", time.gmtime())} UTC (monotonic {time.monotonic():.0f}s)',
          flush=True)

def drop_cache(path):
    """Ask the kernel to forget this file's clean pages.

    The VM has under 1 GB of memory; reading and writing a 345 MB snapshot
    several times would otherwise evict the page cache the service's SQLite
    reads depend on. Dirty pages are flushed first, because only clean pages
    are dropped. Advisory: a failure changes nothing.
    """
    if path.is_dir():
        for child in path.rglob('*'):
            if child.is_file():
                drop_cache(child)
        return
    try:
        fd = os.open(path, os.O_RDONLY)
        try:
            os.fsync(fd)
            os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
        finally:
            os.close(fd)
    except OSError:
        pass

def digest(path):
    with path.open('rb') as source:
        result = hashlib.file_digest(source, 'sha256').hexdigest()
    drop_cache(path)
    return result

def open_repository_lock(repository):
    """Open the backup repository's lock file read-only, never creating it.

    The backup service owns that file and creates it before its first
    snapshot. This script runs as root with a 0077 umask, so creating the file
    here would leave one that the service can no longer open, and every later
    backup would fail. A shared flock works on a read-only descriptor.
    """
    path = repository / '.lock'
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        raise SystemExit(f'No backup repository lock at {path} yet; run a backup before the transfer.') from None
    except OSError as error:
        raise SystemExit(f'Backup repository lock {path} {unopenable(error)}.') from None
    return regular_file(fd, path)

def unopenable(error):
    """Why the lock could not be opened; O_NOFOLLOW reports a symlink as ELOOP."""
    if error.errno == errno.ELOOP:
        return 'is a symbolic link, not a regular file'
    return f'cannot be opened: {error.strerror}'

def regular_file(fd, path):
    """Wrap `fd` in a file object, refusing anything but a regular file."""
    if stat.S_ISREG(os.fstat(fd).st_mode):
        return os.fdopen(fd, 'rb')
    os.close(fd)
    raise SystemExit(f'Backup repository lock {path} is not a regular file.')

def main():
    """Transfer the newest complete snapshot, holding both locks throughout."""
    os.umask(0o077)
    state.mkdir(mode=0o700, exist_ok=True)
    with (state / '.lock').open('a') as transfer_lock:
        fcntl.flock(transfer_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with open_repository_lock(repository) as source_lock:
            fcntl.flock(source_lock, fcntl.LOCK_SH)
            snapshots = sorted(p for p in (repository / 'snapshots').iterdir()
                               if p.is_dir() and (p / 'COMPLETE').is_file())
            if not snapshots:
                raise RuntimeError('No completed snapshot is available.')
            snapshot = snapshots[-1]
            previous = state / 'last-verified.json'
            if previous.exists() and json.loads(previous.read_text())['snapshot'] == snapshot.name:
                print('Newest snapshot already verified off-server.')
                raise SystemExit(0)
            step('verify-source')
            report = json.loads(run([binary, 'backup-verify', '--snapshot', str(snapshot)]))
            drop_cache(snapshot)
            destination = bucket + '/snapshots/' + snapshot.name + '.tar.gz'
            with tempfile.TemporaryDirectory(prefix='transfer-', dir=state) as temporary:
                working = Path(temporary)
                archive = working / 'snapshot.tar.gz'
                # Deterministic gzip header and tar contents for the immutable source.
                import gzip
                step('archive')
                with archive.open('wb') as raw:
                    with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as zipped:
                        with tarfile.open(fileobj=zipped, mode='w') as bundle:
                            bundle.add(snapshot, arcname='snapshot', recursive=True)
                drop_cache(snapshot)
                step('digest-archive')
                source_digest = digest(archive)
                step('upload')
                run(['gcloud','storage','cp',str(archive),destination,'--quiet'])
                drop_cache(archive)
                downloaded = working / 'downloaded.tar.gz'
                step('download')
                run(['gcloud','storage','cp',destination,str(downloaded),'--quiet'])
                drop_cache(downloaded)
                step('digest-download')
                if digest(downloaded) != source_digest:
                    raise RuntimeError('Downloaded backup archive does not match uploaded bytes.')
                restored = working / 'verified'
                restored.mkdir(mode=0o700)
                step('extract')
                with tarfile.open(downloaded) as bundle:
                    bundle.extractall(restored, filter='data')
                drop_cache(restored)
                step('verify-restored')
                run([binary,'backup-verify','--snapshot',str(restored / 'snapshot')])
                drop_cache(restored)
                step('receipt')
                receipt = {'snapshot': snapshot.name, 'object': destination,
                           'sha256': source_digest, 'verified_at_unix': int(time.time())}
                staged = state / 'last-verified.json.tmp'
                with staged.open('w') as output:
                    json.dump(receipt, output)
                    output.flush()
                    os.fsync(output.fileno())
                os.replace(staged, previous)
            # Keep the same snapshots as the source's retention (5 daily).
            retained = {p.name + '.tar.gz' for p in snapshots}
            step('prune')
            objects = run(['gcloud','storage','ls',bucket + '/snapshots/','--quiet']).splitlines()
            for item in objects:
                if item.startswith(bucket + '/snapshots/') and item.endswith('.tar.gz'):
                    if item.rsplit('/',1)[-1] not in retained:
                        run(['gcloud','storage','rm',item,'--quiet'])
            print(json.dumps(receipt))


if __name__ == '__main__':
    main()
