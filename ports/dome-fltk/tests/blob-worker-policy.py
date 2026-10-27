#!/usr/bin/env python3
"""Validate claimed local/HTTP blob workers, revocation and foreign-origin denial."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

here = Path(__file__).resolve().parent
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=here.parents[2]/'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
binary, out = a.binary.resolve(), a.output.resolve()
if out.exists():
    p.error('choose a fresh output directory')
out.mkdir(parents=True)
def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()
binary_sha, fixture_sha = sha(binary), sha(here/'blob-worker-policy.html')
rows = []
for mode in ['http', 'local']:
    folder = out/mode
    command = [sys.executable, str(here/'cache-api.py'), '--binary', str(binary),
               '--fixture', str(here/'blob-worker-policy.html'), '--local-file-controls',
               '--stages', 'write', '--timeout', '40', '--output', str(folder)]
    if mode == 'local':
        command.append('--local-top-level')
    with (out/(mode+'.log')).open('w') as log:
        result = subprocess.run(command, stdout=log, stderr=log)
    d = json.loads((folder/'report.json').read_text())
    assert d['binary_sha256'] == binary_sha and d['fixture_sha256'] == fixture_sha
    assert len(d['runs']) == 1
    run = d['runs'][0]
    assert run['clean_close'] and run['exit_code'] == 0
    assert len(run['checks']) == 12 and result.returncode == 0
    assert all(c['passed'] for c in run['checks'])
    assert run['diagnostics']['localTop'] == (mode == 'local')
    rows.append({'mode': mode, 'checks': 12, 'report': str(folder/'report.json')})
    print('PASS '+mode+' claimed blob worker policy', flush=True)
assert sha(binary) == binary_sha
(out/'matrix.json').write_text(json.dumps({'binary': str(binary), 'binary_sha256': binary_sha,
    'fixture_sha256': fixture_sha, 'runs': rows, 'checks': 24, 'status': 'passed',
    'test_inputs_sha256': {f: sha(here/f) for f in ['blob-worker-policy.py', 'blob-worker-policy.html', 'cache-api.py']},
    'scope': 'Dedicated classic workers only. Existing claims survive revocation; missing, revoked, foreign HTTP and unrelated opaque blob workers cannot execute in the parent. File authority remains bound to the local principal.'}, indent=2)+'\n')
