#!/usr/bin/env python3
"""Check SVG color attributes through the native cascade and rendered pixels."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
from PIL import Image

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--reference-browser', type=Path)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
output = args.output.resolve()
command = [sys.executable, str(Path(__file__).with_name('cache-api.py')),
           '--fixture', str(Path(__file__).with_suffix('.html')), '--stages', 'write',
           '--capture-frame', '--binary', str(args.binary.resolve()), '--output', str(output)]
if args.reference_browser:
    command += ['--reference-browser', str(args.reference_browser.resolve())]
result = subprocess.run(command, check=False)
report = json.loads((output/'report.json').read_text())
image = Image.open(output/'write/frame.png').convert('RGB')
names = ['root attribute', 'group inheritance', 'stylesheet override', 'inline override',
         'inherit keyword', 'unset keyword', 'custom property substitution',
         'invalid trailing value', 'namespaced attribute ignored', 'currentColor inheritance',
         'attribute mutation', 'attribute removal']
checks = []
for index, name in enumerate(names):
    point = (50 + (index % 6)*80, 50 + (index // 6)*100)
    actual = list(image.getpixel(point))
    checks.append({'name': name, 'passed': actual == [20, 180, 90], 'actual': actual,
                   'expected': [20, 180, 90]})
    print(('PASS ' if checks[-1]['passed'] else 'FAIL ') + name + ' ' + str(actual), flush=True)
report['pixel_checks'] = checks
report['driver_exit_code'] = result.returncode
(output/'paint.json').write_text(json.dumps(report, indent=2)+'\n')
assert result.returncode == 0, 'DOM checks failed; raw report and pixel results retained'
assert all(check['passed'] for check in checks)
