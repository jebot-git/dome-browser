#!/usr/bin/env python3
"""Run local WebGL resource lifetime regressions in isolated browser profiles."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

here = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
binary, out = args.binary.resolve(), args.output.resolve()
if out.exists():
    parser.error('choose a fresh output directory')
out.mkdir(parents=True, mode=0o700)
def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()
binary_sha, fixture_sha = sha(binary), sha(here/'webgl-gc-lifetime.html')
rows = []
for mode in ['webgl1', 'webgl2']:
    for order in ['buffers-first', 'vao-first']:
        name = mode+'-'+order
        folder = out/name
        command = [sys.executable, str(here/'cache-api.py'), '--binary', str(binary),
                   '--fixture', str(here/'webgl-gc-lifetime.html'), '--stages', 'write',
                   '--path', '/?mode='+mode+'&order='+order, '--timeout', '120', '--output', str(folder)]
        with (out/(name+'.log')).open('w') as log:
            result = subprocess.run(command, stdout=log, stderr=log)
        assert result.returncode == 0, 'Local lifetime regression failed; diagnostics retained privately'
        report = json.loads((folder/'report.json').read_text())
        assert report['binary_sha256'] == binary_sha and report['fixture_sha256'] == fixture_sha
        assert len(report['runs']) == 1
        run = report['runs'][0]
        assert run['clean_close'] and run['exit_code'] == 0
        assert len(run['checks']) == 4 and all(check['passed'] for check in run['checks'])
        assert run['diagnostics']['mode'] == mode and run['diagnostics']['order'] == order
        rows.append({'name': name, 'checks': 4, 'report': str(folder/'report.json')})
        print('PASS local resource lifetime case '+str(len(rows)), flush=True)
assert sha(binary) == binary_sha
(out/'matrix.json').write_text(json.dumps({'status': 'passed', 'binary_sha256': binary_sha,
    'fixture_sha256': fixture_sha, 'runs': rows, 'checks': 16,
    'test_inputs_sha256': {f: sha(here/f) for f in ['webgl-gc-lifetime.py', 'webgl-gc-lifetime.html', 'cache-api.py']},
    'scope': 'Local WebGL1 and WebGL2 resource ownership, explicit cleanup, retained objects, rendering and orderly shutdown under allocation pressure.'}, indent=2)+'\n')
