#!/usr/bin/env python3
"""Check SVG image completion after removal in an isolated browser profile."""
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
           '--fixture', str(here/'svg-image-lifecycle.html'), '--stages', 'write',
           '--timeout', '25', '--output', str(out)]
if a.reference_browser:
    command += ['--reference-browser', str(a.reference_browser.resolve())]
result = subprocess.run(command)
report = json.loads((out/'report.json').read_text())
assert len(report['runs']) == 1
run = report['runs'][0]
assert run['clean_close'] and run['exit_code'] == 0, 'unclean browser shutdown'
assert len(run['checks']) == 16
requests = [r for r in report['requests'] if urlsplit(r['path']).path == '/timing-control.svg']
controls = [parse_qs(urlsplit(r['path']).query)['control'][0] for r in requests]
assert sorted(controls) == ['lifecycle-delayed', 'lifecycle-failure', 'lifecycle-immediate', 'lifecycle-parent-removed'], controls
binary = (a.reference_browser or a.binary).resolve()
with binary.open('rb') as stream:
    assert report['binary_sha256'] == hashlib.file_digest(stream, 'sha256').hexdigest()
assert result.returncode == 0 and all(c['passed'] for c in run['checks']), 'image lifecycle regression'
