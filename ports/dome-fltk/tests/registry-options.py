#!/usr/bin/env python3
"""Registry factory, shadow parsing, clone isolation and rendered defined state."""
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
           '--capture-frame', '--disable-adblock', '--timeout', '90',
           '--binary', str(args.binary.resolve()), '--output', str(output)]
if args.reference_browser:
    command += ['--reference-browser', str(args.reference_browser.resolve())]
subprocess.run(command, check=True)
report = json.loads((output/'report.json').read_text())
image = Image.open(output/'write/frame.png').convert('RGB')
checks = []
for index in range(3):
    actual = list(image.getpixel((50+index*80,50)))
    expected = [20,180,90] if index < 2 else [120,120,120]
    checks.append({'name':'registry-defined paint '+str(index),'passed':actual==expected,
                   'actual':actual,'expected':expected})
    print(('PASS 'if actual==expected else'FAIL ')+checks[-1]['name']+' '+str(actual),flush=True)
report['pixel_checks'] = checks
report['scope'] = 'Isolated registry factories and shadow/parser rendering; not full scoped-registry conformance.'
(output/'paint.json').write_text(json.dumps(report,indent=2)+'\n')
assert all(check['passed']for check in checks)
