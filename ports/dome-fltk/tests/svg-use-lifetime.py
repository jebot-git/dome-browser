#!/usr/bin/env python3
"""Exercise repeated private SVG instance/style/resource creation and closure."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
from PIL import Image

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
output = args.output.resolve()
command = [sys.executable, str(Path(__file__).with_name('cache-api.py')),
           '--fixture', str(Path(__file__).with_suffix('.html')), '--stages', 'write',
           '--capture-frame', '--disable-adblock', '--timeout', '180',
           '--binary', str(args.binary.resolve()), '--output', str(output)]
subprocess.run(command, check=True)
report = json.loads((output/'report.json').read_text())
image = Image.open(output/'write/frame.png').convert('RGB')
checks = []
for index in range(4):
    actual = list(image.getpixel((50+index*80,50)))
    expected = [20,180,90] if index < 3 else [255,255,255]
    checks.append({'name':'settled instance '+str(index),'passed':actual==expected,
                   'actual':actual,'expected':expected})
    print(('PASS 'if actual==expected else'FAIL ')+checks[-1]['name']+' '+str(actual),flush=True)
report['pixel_checks'] = checks
report['scope'] = 'Private software-rendered sixty-second instance churn; memory snapshots are diagnostics, not a bounded-memory certification.'
(output/'paint.json').write_text(json.dumps(report,indent=2)+'\n')
assert all(check['passed']for check in checks)
