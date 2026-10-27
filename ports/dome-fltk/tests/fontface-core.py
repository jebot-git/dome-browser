#!/usr/bin/env python3
"""Exercise FontFaceSet matched loading, CSS membership, CSP and CORS."""
import argparse
from pathlib import Path
import subprocess
import sys

root=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,default=root/'target/release/dome-browser')
p.add_argument('--reference-browser',type=Path)
p.add_argument('--output',type=Path,required=True)
a=p.parse_args()
command=[sys.executable,str(Path(__file__).with_name('cache-api.py')),'--binary',str(a.binary.resolve()),'--fixture',str(Path(__file__).with_suffix('.html')),'--stages','write','--output',str(a.output.resolve())]
if a.reference_browser:command+=['--reference-browser',str(a.reference_browser.resolve())]
result=subprocess.run(command)
import json
report=json.loads((a.output.resolve()/'report.json').read_text())
blocked=[r['path']for r in report['requests']if 'csp-blocked'in r['path']]
assert not blocked, ('CSP-blocked font reached HTTP server',blocked)
raise SystemExit(result.returncode)
