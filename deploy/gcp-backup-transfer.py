#!/usr/bin/env python3
"""Archive, upload, download-verify, and retain complete coordinator snapshots."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import time

os.umask(0o077)
repository = Path('/var/lib/agent-coordinator-backups')
state = Path('/var/lib/agent-coordinator-transfer')
state.mkdir(mode=0o700, exist_ok=True)
bucket = 'gs://sithbit-19b44-agent-coordinator-backups-east'
binary = '/usr/local/bin/agent-coordinator-server'

def run(args):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if result.returncode:
        # Neither CLI diagnostic streams nor authenticated URLs are logged.
        raise RuntimeError(f'{Path(args[0]).name} operation failed with exit {result.returncode}')
    return result.stdout

def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()

with (state / '.lock').open('a') as transfer_lock:
    fcntl.flock(transfer_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    with (repository / '.lock').open('a') as source_lock:
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
        report = json.loads(run([binary, 'backup-verify', '--snapshot', str(snapshot)]))
        destination = bucket + '/snapshots/' + snapshot.name + '.tar.gz'
        with tempfile.TemporaryDirectory(prefix='transfer-', dir=state) as temporary:
            working = Path(temporary)
            archive = working / 'snapshot.tar.gz'
            # Deterministic gzip header and tar contents for the immutable source.
            import gzip
            with archive.open('wb') as raw:
                with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as zipped:
                    with tarfile.open(fileobj=zipped, mode='w') as bundle:
                        bundle.add(snapshot, arcname='snapshot', recursive=True)
            source_digest = digest(archive)
            run(['gcloud','storage','cp',str(archive),destination,'--quiet'])
            downloaded = working / 'downloaded.tar.gz'
            run(['gcloud','storage','cp',destination,str(downloaded),'--quiet'])
            if digest(downloaded) != source_digest:
                raise RuntimeError('Downloaded backup archive does not match uploaded bytes.')
            restored = working / 'verified'
            restored.mkdir(mode=0o700)
            with tarfile.open(downloaded) as bundle:
                bundle.extractall(restored, filter='data')
            run([binary,'backup-verify','--snapshot',str(restored / 'snapshot')])
            receipt = {'snapshot': snapshot.name, 'object': destination,
                       'sha256': source_digest, 'verified_at_unix': int(time.time())}
            staged = state / 'last-verified.json.tmp'
            with staged.open('w') as output:
                json.dump(receipt, output)
                output.flush()
                os.fsync(output.fileno())
            os.replace(staged, previous)
        # Keep the same represented 24-hour/30-day snapshots as the source.
        retained = {p.name + '.tar.gz' for p in snapshots}
        objects = run(['gcloud','storage','ls',bucket + '/snapshots/','--quiet']).splitlines()
        for item in objects:
            if item.startswith(bucket + '/snapshots/') and item.endswith('.tar.gz'):
                if item.rsplit('/',1)[-1] not in retained:
                    run(['gcloud','storage','rm',item,'--quiet'])
        print(json.dumps(receipt))
