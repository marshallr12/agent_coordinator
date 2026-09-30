#!/usr/bin/env python3
"""Check saved GitHub Actions jobs (gh api --paginate --slurp) at one pinned SHA.

Each input contains all job attempts of a required workflow. No API writes.
At least 20 distinct attempts per required check; every non-success fails closed.
"""
import argparse
import json
from pathlib import Path

CHECKS = (
    'Linux format, Clippy, and workspace tests',
    'Audit locked dependencies',
    'Pinned mdBook build and local-link validation',
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sha', required=True)
    parser.add_argument('files', nargs='+', type=Path)
    args = parser.parse_args()
    jobs = {name: {} for name in CHECKS}
    for path in args.files:
        pages = json.loads(path.read_text())
        if isinstance(pages, dict):
            pages = [pages]
        for page in pages:
            for job in page['jobs']:
                if job['name'] not in jobs:
                    continue
                if job['head_sha'] != args.sha:
                    raise SystemExit('input includes a different commit')
                # Job ids uniquely identify attempts; repeated pages cannot inflate sample size.
                jobs[job['name']][job['id']] = job
    report = {}
    for name, attempts in jobs.items():
        failures = sum(j['status'] != 'completed' or j['conclusion'] != 'success'
                       for j in attempts.values())
        size = len(attempts)
        report[name] = {'attempts': size, 'non_success': failures,
                        'rate': failures / size if size else None,
                        'passed': size >= 20 and failures / size < 0.02}
    print(json.dumps({'sha': args.sha, 'checks': report}, indent=2))
    return int(not all(r['passed'] for r in report.values()))


if __name__ == '__main__':
    raise SystemExit(main())
