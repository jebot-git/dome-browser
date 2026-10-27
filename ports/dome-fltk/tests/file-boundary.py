#!/usr/bin/env python3
"""Check local-file authority using synthetic files and isolated browser profiles.

File-origin blob execution gaps are reported explicitly, never counted as
passing security controls. --require-file-blobs makes those gaps fatal.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
from PIL import Image

here = Path(__file__).resolve().parent
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=here.parents[2]/'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument('--require-file-blobs', action='store_true')
a = p.parse_args()
binary, out = a.binary.resolve(), a.output.resolve()
if out.exists():
    p.error('choose a fresh output directory')
out.mkdir(parents=True)
def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()
binary_sha = sha(binary)
summary = {'binary': str(binary), 'binary_sha256': binary_sha, 'runs': [],
           'known_nonexecuting_contexts': [], 'scope': 'Only generated temporary files. '
           'Nonexecuting file-origin blob contexts do not prove authority isolation.'}
def save():
    (out/'matrix.json').write_text(json.dumps(summary, indent=2)+'\n')
save()
for fixture in ['file-boundary.html', 'file-boundary-inheritance.html']:
    for mode in ['http', 'local']:
        name = ('basic-' if fixture == 'file-boundary.html' else 'inheritance-')+mode
        folder = out/name
        command = [sys.executable, str(here/'cache-api.py'), '--binary', str(binary),
                   '--fixture', str(here/fixture), '--local-file-controls', '--stages', 'write',
                   '--timeout', '30', '--output', str(folder)]
        if mode == 'local':
            command.append('--local-top-level')
        if fixture == 'file-boundary.html':
            command.append('--capture-frame')
        with (out/(name+'.log')).open('w') as log:
            result = subprocess.run(command, stdout=log, stderr=log)
        d = json.loads((folder/'report.json').read_text())
        assert d['binary_sha256'] == binary_sha and d['fixture_sha256'] == sha(here/fixture)
        assert len(d['runs']) == 1
        r = d['runs'][0]
        assert r['clean_close'] and r['exit_code'] == 0
        checks, known, pixels = [], [], []
        diag = r['diagnostics']
        if fixture == 'file-boundary.html':
            assert result.returncode == 0 and len(r['checks']) == 4
            assert all(c['passed'] for c in r['checks'])
            assert diag['localContext'] == (mode == 'local')
            checks = r['checks']
            frame = Image.open(folder/'write/frame.png').convert('RGB')
            assert len(r['pixel_cases']) == 4
            for c in r['pixel_cases']:
                rgb = frame.getpixel((c['x'], c['y']))
                green = rgb[1] > 80 and rgb[0] < 60 and rgb[2] < 60
                red = rgb[0] > 180 and rgb[1] < 60 and rgb[2] < 60
                assert green if c['expected'] == 'green' else red, (name, c, rgb)
                pixels.append({'name': c['name'], 'rgb': rgb, 'passed': True})
        else:
            assert diag['localTop'] == (mode == 'local')
            docs = {c['name']: c for c in diag['documents']}
            assert set(docs) == {'top', 'aboutblank', 'srcdoc', 'sandbox', 'data', 'blob'}
            assert docs['aboutblank']['sameParent'] and docs['srcdoc']['sameParent']
            assert not docs['sandbox']['sameParent'] and not docs['data']['sameParent']
            for context, c in docs.items():
                if mode == 'local' and context == 'blob' and c.get('error'):
                    assert c['error'] == 'timeout', c
                    known.append({'context': 'file-origin blob document', 'result': c})
                    continue
                assert not c.get('error') and c['events']['http'] == 'load', (name, c)
                allowed = mode == 'local' and context in ['top', 'aboutblank', 'srcdoc']
                assert c['events']['html'] == c['events']['svg'] == ('load' if allowed else 'error'), (name, c)
                assert c['fileFetchAllowed'] == allowed, (name, c)
                checks.append({'context': context, 'file_allowed': allowed, 'passed': True})
            assert len(diag['workers']) == (1 if mode == 'local' else 2)
            for c in diag['workers']:
                if mode == 'local' and c.get('error'):
                    assert c['error'] in ['error', 'worker error'], c
                    known.append({'context': 'file-origin blob worker', 'result': c})
                    continue
                assert c.get('positive') is True and c['fileAllowed'] == (mode == 'local'), (name, c)
                checks.append({'worker': c['name'], 'file_allowed': c['fileAllowed'], 'passed': True})
            if mode == 'http':
                assert result.returncode == 0 and not known and len(checks) == 8
            else:
                assert result.returncode in [0, 1] and len(checks) >= 5 and len(known) <= 2
                if not known:
                    assert result.returncode == 0
            assert len(r['checks']) == 3
            expected_raw = [not any(c['context'] == 'file-origin blob document' for c in known), True,
                            not any(c['context'] == 'file-origin blob worker' for c in known)]
            assert [c['passed'] for c in r['checks']] == expected_raw, (name, r['checks'], known)
            assert result.returncode == (1 if known else 0)
            summary['known_nonexecuting_contexts'] += known
        summary['runs'].append({'name': name, 'report': str(folder/'report.json'),
                                'checks': checks, 'pixels': pixels, 'driver_exit_code': result.returncode,
                                'known_nonexecuting_contexts': known})
        save()
        print('PASS supported authority controls: '+name, flush=True)
        for c in known:
            print('KNOWN compatibility gap; no authority claim: '+c['context'], flush=True)
assert sha(binary) == binary_sha
summary['test_inputs_sha256'] = {f: sha(here/f) for f in
    ['file-boundary.py', 'cache-api.py', 'file-boundary.html', 'file-boundary-inheritance.html', 'file-boundary-worker.js']}
summary['status'] = 'supported authority controls passed'
summary['full_context_execution'] = not summary['known_nonexecuting_contexts']
save()
assert not a.require_file_blobs or summary['full_context_execution'], 'file-origin blob execution remains incomplete'
