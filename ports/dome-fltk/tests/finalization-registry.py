#!/usr/bin/env python3
"""Check native FinalizationRegistry cleanup and shutdown in private profiles.

Run with python-xlib, pillow and dbus-next available, as for cache-api.py.
GC timing is implementation-defined. These allocation-triggered diagnostics
must be read alongside their GC records; they do not assert a cleanup deadline.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

here = Path(__file__).resolve().parent
source = here.parents[2]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=source/'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument('--lifecycle-runs', type=int, default=5)
a = p.parse_args()
if a.lifecycle_runs < 0:
    p.error('--lifecycle-runs must be nonnegative')
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=True)
if (out/'matrix.json').exists():
    p.error('choose a fresh output directory')
binary = a.binary.resolve()
def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()
fixtures = {
    'core': here/'finalization-registry.html',
    'window-close': here/'finalization-pending-window-close.html',
    'worker-terminate': here/'finalization-pending-worker-terminate.html',
}
fixture_hashes = {name: sha(path) for name, path in fixtures.items()}
jobs = [('core', 'core', True), ('core-default-adblock', 'core', False)]
for kind in ['window-close', 'worker-terminate']:
    jobs += [(kind+'-'+str(n), kind, True) for n in range(a.lifecycle_runs)]
matrix = {'binary': str(binary), 'binary_sha256': sha(binary),
          'fixture_sha256': fixture_hashes, 'jobs': len(jobs), 'completed': 0,
          'scope': 'Controlled cleanup and teardown diagnostics; no fixed GC deadline or all-interleaving guarantee.',
          'runs': []}
def save():
    (out/'matrix.json').write_text(json.dumps(matrix, indent=2)+'\n')
save()
for name, kind, disabled in jobs:
    folder = out/name
    assert not (folder/'report.json').exists(), 'stale result: '+str(folder)
    command = [sys.executable, str(here/'cache-api.py'), '--binary', str(binary),
               '--fixture', str(fixtures[kind]), '--stages', 'write',
               '--timeout', '30', '--output', str(folder)]
    if disabled:
        command.append('--disable-adblock')
    started = time.monotonic()
    with (out/(name+'.log')).open('w') as log:
        result = subprocess.run(command, env={**os.environ,
            'MOZ_GCTIMER': str(folder/'gc-timer.log')}, stdout=log, stderr=log)
    row = {'name': name, 'exit_code': result.returncode,
           'elapsed_seconds': round(time.monotonic()-started, 2),
           'report': str(folder/'report.json')}
    matrix['runs'].append(row)
    matrix['completed'] += 1
    save()
    assert result.returncode == 0, name+' failed; inspect '+str(out/(name+'.log'))
    report = json.loads((folder/'report.json').read_text())
    assert report['binary_sha256'] == matrix['binary_sha256'], name
    assert report['fixture_sha256'] == fixture_hashes[kind], name
    assert len(report['runs']) == 1, name
    run = report['runs'][0]
    assert run['clean_close'] and run['exit_code'] == 0, name
    assert len(run['checks']) == (19 if kind == 'core' else 1), name
    assert all(check['passed'] for check in run['checks']), name
    gc = (folder/'gc-timer.log').read_text()
    assert re.search(r'^  Invocation Kind: Normal$', gc, re.M), name+' has no normal GC record'
    row['checks'] = len(run['checks'])
    row['clean_close'] = True
    row['normal_gc_observed'] = True
    save()
assert sha(binary) == matrix['binary_sha256']
assert all(sha(fixtures[name]) == value for name, value in fixture_hashes.items())
matrix['validated'] = True
save()
print(json.dumps({'jobs': len(jobs), 'passed': matrix['completed'],
                  'binary_sha256': matrix['binary_sha256']}, indent=2))
