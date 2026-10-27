#!/usr/bin/env python3
"""Private SVG snapshots must not invoke custom element constructors."""
import argparse
from pathlib import Path
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--reference-browser', type=Path)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
command = [sys.executable, str(Path(__file__).with_name('cache-api.py')),
           '--fixture', str(Path(__file__).with_suffix('.html')), '--stages', 'write',
           '--disable-adblock', '--binary', str(args.binary.resolve()), '--output', str(args.output.resolve())]
if args.reference_browser:
    command += ['--reference-browser', str(args.reference_browser.resolve())]
subprocess.run(command, check=True)
