#!/usr/bin/env python3
"""Verify unavailable SVG paint servers use their color or transparent fallback.

Run with python-xlib and Pillow; reference comparisons also need websockets.
Paint fallback semantics: https://svgwg.org/svg2-draft/painting.html#SpecifyingPaint
"""
import argparse
import json
from pathlib import Path
import subprocess
import sys
from PIL import Image

source = Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=source/'target/release/dome-browser')
parser.add_argument('--reference-browser', type=Path)
parser.add_argument('--output', type=Path, required=True)
a = parser.parse_args()
out = a.output.resolve()
command = [sys.executable, str(Path(__file__).with_name('cache-api.py')),
           '--fixture', str(Path(__file__).with_suffix('.html')), '--stages', 'write',
           '--capture-frame', '--binary', str(a.binary.resolve()), '--output', str(out)]
if a.reference_browser:
    command += ['--reference-browser', str(a.reference_browser.resolve())]
subprocess.run(command, check=True)
image = Image.open(out/'write/frame.png').convert('RGB')
samples = [
    ('attribute fallback',(50,50),[20,180,90]),
    ('inline fallback',(130,50),[20,180,90]),
    ('stylesheet fallback',(210,50),[20,180,90]),
    ('external reference never aliases a local ID',(290,50),[20,180,90]),
    ('inline none fallback',(370,50),[255,255,255]),
    ('stylesheet none fallback',(450,50),[255,255,255]),
    ('external omitted fallback',(50,150),[255,255,255]),
    ('local omitted fallback',(130,150),[255,255,255]),
    ('external stroke fallback',(195,150),[20,180,90]),
]
checks = []
for name, point, expected in samples:
    actual = list(image.getpixel(point))
    check = {'name':name, 'passed':actual == expected, 'pixel':actual, 'expected':expected}
    checks.append(check)
    print(('PASS ' if check['passed'] else 'FAIL ')+name, flush=True)
report = json.loads((out/'report.json').read_text())
report['pixel_checks'] = checks
(out/'fallback-report.json').write_text(json.dumps(report, indent=2)+'\n')
assert all(check['passed'] for check in checks), checks
