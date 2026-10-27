#!/usr/bin/env python3
"""Validate native SVG images, policy, private resources and pending shutdown.

Run with python-xlib, pillow, websockets and dbus-next available. All browser
profiles, cookies, filter rules and display/bus sessions are private fixtures.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time
from urllib.parse import parse_qs, urlsplit
from PIL import Image

here = Path(__file__).resolve().parent
source = here.parents[2]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=source/'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
binary = a.binary.resolve()
out = a.output.resolve()
if out.exists():
    p.error('choose a fresh output directory')
out.mkdir(parents=True)

def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

binary_sha = sha(binary)
rows = []
fixtures = {'lifecycle': 'svg-image-lifecycle.html',
            'inert-adoption': 'svg-image-inert-adoption.html',
            'private': 'svg-image-private-resources.html',
            'load-order': 'svg-image-load-order.html'}
jobs = [('lifecycle', '/', False, None)]
jobs += [('policy-'+mode, '/?policy='+mode, False, None) for mode in ['allow', 'none', 'self']]
jobs += [('adoption-'+mode, '/?mode='+mode, False, None) for mode in
         ['pending-blocked', 'loaded-blocked', 'pending-allowed', 'loaded-allowed']]
jobs += [(name, '/', False, None) for name in ['inert-adoption', 'private', 'load-order']]
jobs += [('pending-close-'+str(n), '/', False, None) for n in range(5)]
jobs += [('adblock-enabled', '/', False, 'svg-image-adblock-rules.txt'),
         ('adblock-disabled', '/?disabled=1', True, 'svg-image-adblock-rules.txt')]
manifest = {'binary': str(binary), 'binary_sha256': binary_sha, 'jobs': len(jobs),
            'completed': 0, 'runs': rows,
            'scope': 'SVG image fetch/events, CSP/CORS/credentials, source/adoption isolation, cosmetic/network filters and controlled shutdown; not complete animation/canvas/IDL or all-interleaving proof.'}

def save():
    (out/'matrix.json').write_text(json.dumps(manifest, indent=2)+'\n')

save()
for name, path, disabled, filters in jobs:
    fixture = fixtures.get(name)
    if fixture is None:
        fixture = ('svg-image-policy.html' if name.startswith('policy-') else
                   'svg-image-adoption.html' if name.startswith('adoption-') else
                   'svg-image-pending-window-close.html' if name.startswith('pending-close-') else
                   'svg-image-adblock.html')
    command = [sys.executable, str(here/'cache-api.py'), '--binary', str(binary),
               '--fixture', str(here/fixture), '--svg-image-controls', '--stages', 'write',
               '--timeout', '30', '--path', path, '--output', str(out/name)]
    if name.startswith(('policy-', 'adoption-', 'adblock-')) or name in ['private', 'load-order']:
        command.append('--capture-frame')
    if disabled:
        command.append('--disable-adblock')
    if filters:
        command += ['--fixture-filters', str(here/filters)]
    started = time.monotonic()
    with (out/(name+'.log')).open('w') as log:
        result = subprocess.run(command, stdout=log, stderr=log)
    assert result.returncode == 0, name+' failed; inspect '+str(out/(name+'.log'))
    d = json.loads((out/name/'report.json').read_text())
    assert d['binary_sha256'] == binary_sha and d['fixture_sha256'] == sha(here/fixture)
    assert len(d['runs']) == 1
    run = d['runs'][0]
    assert run['clean_close'] and run['exit_code'] == 0
    assert run['checks'] and all(c['passed'] for c in run['checks'])
    frame = Image.open(out/name/'write/frame.png').convert('RGB') if run.get('pixel_cases') else None
    pixels = []
    for case in run.get('pixel_cases', []):
        rgb = frame.getpixel((case['x'], case['y']))
        expected = case['expected']
        # Broken-resource artwork is UA-defined. Require absence of the green
        # resource, alongside error events and server-hit checks below.
        if expected in ['red', 'not-green']:
            passed = rgb != (0, 128, 0)
        else:
            passed = rgb == {'green': (0, 128, 0), 'white': (255, 255, 255), 'blue': (0, 0, 255)}[expected]
        assert passed, (name, case, rgb)
        pixels.append({'name': case['name'], 'rgb': rgb, 'passed': passed})
    requests = {}
    for request in d['requests']:
        u = urlsplit(request['path'])
        if u.path == '/svg-image.svg':
            requests.setdefault(parse_qs(u.query)['name'][0], []).append(request)
    if name.startswith('policy-'):
        mode = name.removeprefix('policy-')
        expected_names = ({'same-none', 'same-anonymous', 'cross-none', 'cross-anonymous-denied',
                           'cross-anonymous-allowed', 'cross-credentials-allowed',
                           'cross-credentials-denied', 'stale-old', 'stale-new'} if mode == 'allow' else
                          {'same-none', 'same-anonymous'} if mode == 'self' else set())
        assert set(requests) == expected_names and all(len(rs) == 1 for rs in requests.values())
        for key, rs in requests.items():
            assert ('svg-cookie=fixture' in rs[0]['cookie']) != key.startswith('cross-anonymous-')
    elif name.startswith('pending-close-'):
        assert sum(len(rs) for key, rs in requests.items() if key.startswith('close-pending-')) == 6
    elif name.startswith('adblock-'):
        paths = [r['path'] for rs in requests.values() for r in rs]
        assert ('/svg-image.svg?name=adblock-denied&transport=image' in paths) == disabled
        for url in ['/svg-image.svg?name=legitimate&transport=image',
                    '/svg-image.svg?name=adblock-exception&transport=image',
                    '/svg-image.svg?name=adblock-denied&transport=fetch']:
            assert paths.count(url) == 1, (name, url, paths)
    elif name == 'lifecycle':
        controls = [parse_qs(urlsplit(r['path']).query)['control'][0] for r in d['requests']
                    if urlsplit(r['path']).path == '/timing-control.svg']
        assert sorted(controls) == ['lifecycle-delayed', 'lifecycle-failure', 'lifecycle-immediate', 'lifecycle-parent-removed']
        assert len(run['checks']) == 16
    rows.append({'name': name, 'checks': len(run['checks']), 'pixels': pixels,
                 'elapsed_seconds': round(time.monotonic()-started, 2),
                 'fixture_sha256': d['fixture_sha256'], 'report': str(out/name/'report.json')})
    manifest['completed'] = len(rows)
    save()
    print('PASS', name, flush=True)
assert sha(binary) == binary_sha
manifest['status'] = 'passed'
manifest['test_inputs_sha256'] = {f.name: sha(f) for f in [Path(__file__), here/'cache-api.py',
    here/'svg-image-adblock-rules.txt'] + [here/name for name in sorted(set(fixtures.values()) | {
        'svg-image-policy.html', 'svg-image-adoption.html', 'svg-image-pending-window-close.html', 'svg-image-adblock.html'})]}
save()
