#!/usr/bin/env python3
"""Verify SVG visibility overrides while ordinary hidden images stay hidden.

Run with python-xlib and Pillow; reference comparisons also need websockets.
SVG visibility semantics: https://svgwg.org/svg2-draft/render.html#VisibilityControl
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
names = ['root attribute override', 'root CSS override', 'group attribute override',
         'group CSS override', 'HTML ancestor override', 'root stays hidden',
         'group stays hidden', 'child stays hidden', 'hidden external SVG image',
         'visible external SVG image']
checks = []
for index, name in enumerate(names):
    actual = list(image.getpixel((50+80*index, 50)))
    expected = [20,180,90] if index < 5 or index == 9 else [255,255,255]
    check = {'name':name, 'passed':actual == expected, 'pixel':actual, 'expected':expected}
    checks.append(check)
    print(('PASS ' if check['passed'] else 'FAIL ')+name, flush=True)
report = json.loads((out/'report.json').read_text())
report['pixel_checks'] = checks
(out/'visibility-report.json').write_text(json.dumps(report, indent=2)+'\n')
assert all(check['passed'] for check in checks), checks
