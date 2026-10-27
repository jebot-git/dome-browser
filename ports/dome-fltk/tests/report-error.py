#!/usr/bin/env python3
"""Check reportError robustness in isolated window and worker environments."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

source = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=source/'target/release/dome-browser')
p.add_argument('--reference-browser', type=Path)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--cases', nargs='+', choices=['symbol','throwing','nonprimitive','nullprototype','revoked','control','undefined'], default=['symbol','throwing','nonprimitive','nullprototype','revoked','control','undefined'])
p.add_argument('--scopes', nargs='+', choices=['window','worker'], default=['window','worker'])
p.add_argument('--timeout', type=float, default=15)
a = p.parse_args()
out = a.output.resolve(); out.mkdir(parents=True, exist_ok=True)
runs = []
for scope in a.scopes:
    for case in a.cases:
        folder = out/(scope+'-'+case)
        command = [sys.executable,str(Path(__file__).with_name('cache-api.py')),
                   '--fixture',str(Path(__file__).with_suffix('.html')),'--stages','write',
                   '--path','/?case='+case+'&scope='+scope,'--timeout',str(a.timeout),
                   '--binary',str(a.binary.resolve()),'--output',str(folder)]
        if a.reference_browser:command += ['--reference-browser',str(a.reference_browser.resolve())]
        with (out/(scope+'-'+case+'.log')).open('w') as log:
            result = subprocess.run(command,stdout=log,stderr=log)
        record = {'scope':scope,'case':case,'exit_code':result.returncode}
        report = folder/'report.json'
        if report.exists():record['report'] = json.loads(report.read_text())
        failure = folder/'failure.json'
        if failure.exists():record['failure'] = json.loads(failure.read_text())
        record['passed'] = result.returncode == 0
        runs.append(record)
        (out/'report.json').write_text(json.dumps({'runs':runs},indent=2)+'\n')
        print(('PASS ' if record['passed'] else 'FAIL ')+scope+': '+case,flush=True)
assert all(run['passed'] for run in runs), [(r['scope'],r['case']) for r in runs if not r['passed']]
