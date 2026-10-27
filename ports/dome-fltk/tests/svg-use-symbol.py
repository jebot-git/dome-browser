#!/usr/bin/env python3
"""Exercise symbol and nested SVG viewports through native use instances."""
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
cases = [('same-root symbol currentColor',(50,50),[20,180,90]),
         ('cross-root symbol variable',(130,50),[20,180,90]),
         ('use overrides nested SVG viewport',(210,50),[20,180,90]),
         ('narrow symbol instance paints inside',(270,50),[20,180,90]),
         ('narrow symbol instance leaves outside blank',(310,50),[255,255,255]),
         ('hidden symbol instance does not paint',(370,50),[255,255,255]),
         ('unreferenced symbol does not paint',(450,50),[255,255,255])]
checks = []
for name, point, expected in cases:
    actual = list(image.getpixel(point))
    checks.append({'name':name,'passed':actual==expected,'actual':actual,'expected':expected})
    print(('PASS 'if actual==expected else'FAIL ')+name+' '+str(actual),flush=True)
report = json.loads((output/'report.json').read_text())
report['pixel_checks'] = checks
(output/'paint.json').write_text(json.dumps(report, indent=2)+'\n')
assert all(check['passed'] for check in checks)
