#!/usr/bin/env python3
"""Exercise native broken-image alternative text and image-state transitions."""
import argparse
from pathlib import Path
import subprocess
import sys

root=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,default=root/'target/release/dome-browser')
p.add_argument('--reference-browser',type=Path)
p.add_argument('--quirks',action='store_true',help='Use the separate quirks-mode document')
p.add_argument('--capture-frame',action='store_true')
p.add_argument('--output',type=Path,required=True)
a=p.parse_args()
fixture=Path(__file__).with_name('image-fallback-quirks.html') if a.quirks else Path(__file__).with_suffix('.html')
command=[sys.executable,str(Path(__file__).with_name('cache-api.py')),'--binary',str(a.binary.resolve()),'--fixture',str(fixture),'--stages','write','--output',str(a.output.resolve())]
if a.reference_browser:command+=['--reference-browser',str(a.reference_browser.resolve())]
if a.capture_frame:command+=['--capture-frame']
raise SystemExit(subprocess.run(command).returncode)
