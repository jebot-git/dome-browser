#!/usr/bin/env python3
"""Preserve SVG image request identity across moves and retarget changed bases."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
from urllib.parse import parse_qs, urlsplit

here = Path(__file__).resolve().parent
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--reference-browser', type=Path)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
out = a.output.resolve()
if out.exists():
    p.error('choose a fresh output directory')
command = [sys.executable, str(here/'cache-api.py'), '--binary', str(a.binary.resolve()),
           '--fixture', str(here/'svg-image-reparent.html'), '--stages', 'write',
           '--timeout', '30', '--output', str(out)]
if a.reference_browser:
    command += ['--reference-browser', str(a.reference_browser.resolve())]
result = subprocess.run(command)
d = json.loads((out/'report.json').read_text())
assert len(d['runs']) == 1
run = d['runs'][0]
assert run['clean_close'] and run['exit_code'] == 0
assert len(run['checks']) == 16
binary = (a.reference_browser or a.binary).resolve()
with binary.open('rb') as stream:
    assert d['binary_sha256'] == hashlib.file_digest(stream, 'sha256').hexdigest()
requests = {}
for row in d['requests']:
    u = urlsplit(row['path'])
    if u.path == '/timing-control.svg':
        name = parse_qs(u.query)['control'][0]
        requests.setdefault(name, []).append(row)
expected = {'reparent-'+name for name in ['completed-move', 'completed-detach', 'repeat-move',
            'handler-move', 'pending-move', 'clone-control', 'base']}
assert set(requests) == expected
assert all(len(rows) == (2 if name == 'reparent-base' else 1) for name, rows in requests.items())
assert len({row['port'] for row in requests['reparent-base']}) == 2
assert result.returncode == 0 and all(c['passed'] for c in run['checks']), 'SVG request identity regression'
