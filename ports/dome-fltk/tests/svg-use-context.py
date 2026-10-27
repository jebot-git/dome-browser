#!/usr/bin/env python3
"""Compare instance selector scope, resources, host inheritance and mutations."""
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
green = [20, 180, 90]
blue = [30, 90, 220]
names = ['source ancestor selector does not cross instance boundary',
         'host ancestor selector does not cross instance boundary',
         'internal selector matches instance ancestry', 'first host currentColor',
         'second host currentColor', 'first host variable', 'second host variable',
         'document gradient ignores first host variable', 'document gradient ignores second host variable', 'nested use inheritance',
         'same-root inherited fill', 'same-root stylesheet fill', 'local clip visible side',
         'source class mutation', 'stylesheet mutation', 'script-containing source renders',
         'host attribute mutation', 'source child insertion']
checks = []
for index, name in enumerate(names):
    point = (50 + (index % 6)*80, 50 + (index // 6)*100)
    if index == 12:
        point = (30, 250)
    expected = [255, 0, 0] if index in [7, 8] else blue if index in [4, 6] else green
    actual = list(image.getpixel(point))
    checks.append({'name': name, 'passed': actual == expected, 'actual': actual, 'expected': expected})
actual = list(image.getpixel((60, 250)))
checks.append({'name': 'local clip excludes right side', 'passed': actual == [255, 255, 255],
               'actual': actual, 'expected': [255, 255, 255]})
for check in checks:
    print(('PASS ' if check['passed'] else 'FAIL ') + check['name'] + ' ' + str(check['actual']), flush=True)
report['pixel_checks'] = checks
report['driver_exit_code'] = result.returncode
(output/'paint.json').write_text(json.dumps(report, indent=2)+'\n')
assert result.returncode == 0, 'DOM or closure checks failed; raw results retained'
assert all(check['passed'] for check in checks)
