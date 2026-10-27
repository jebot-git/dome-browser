#!/usr/bin/env python3
"""Exercise chained document paint resources used by SVG instances."""
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
subprocess.run(command, check=True)
image = Image.open(output/'write/frame.png').convert('RGB')
names = ['linked gradient modern href precedence', 'linked document pattern',
         'percent-encoded resource fragment', 'document resource keeps source variables',
         'external resource uses fallback without local alias', 'circular empty gradient paints none']
checks = []
for index, name in enumerate(names):
    actual = list(image.getpixel((50 + index*80, 50)))
    checks.append({'name': name, 'passed': actual == ([255, 255, 255] if index == 5 else [20, 180, 90]), 'actual': actual,
                   'expected': [255, 255, 255] if index == 5 else [20, 180, 90]})
    print(('PASS ' if checks[-1]['passed'] else 'FAIL ') + name + ' ' + str(actual), flush=True)
report = json.loads((output/'report.json').read_text())
report['pixel_checks'] = checks
(output/'paint.json').write_text(json.dumps(report, indent=2)+'\n')
assert all(check['passed'] for check in checks)
