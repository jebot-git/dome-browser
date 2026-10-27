#!/usr/bin/env python3
"""Verify module evaluation settlement, errors, dependencies and worker shutdown."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
source=Path(__file__).resolve().parents[3]
cases=['sync-ok','await-ok','timer-ok','dependency-ok','sync-error','await-error','timer-undefined','sync-undefined','syntax-error','dynamic-ok','dynamic-error','worker-ok','worker-error','worker-pending','worker-busy','window-pending','async-unhandled']
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,default=source/'target/release/dome-browser');p.add_argument('--reference-browser',type=Path);p.add_argument('--output',type=Path,required=True);p.add_argument('--cases',nargs='+',choices=cases,default=cases)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True);runs=[]
for case in a.cases:
 folder=out/case;command=[sys.executable,str(Path(__file__).with_name('cache-api.py')),'--binary',str(a.binary.resolve()),'--fixture',str(Path(__file__).with_suffix('.html')),'--stages','write','--path','/?case='+case,'--timeout','20','--output',str(folder)]
 if a.reference_browser:command+=['--reference-browser',str(a.reference_browser.resolve()),'--capture-exceptions']
 with(out/(case+'.log')).open('w')as log:result=subprocess.run(command,stdout=log,stderr=log)
 record={'case':case,'exit_code':result.returncode};report=folder/'report.json'
 if report.exists():
  record['report']=json.loads(report.read_text())
  if not a.reference_browser:
   record['console_errors']=[line for line in (folder/'write/browser.log').read_text(errors='replace').splitlines()if line.startswith('PAGE Error ')]
   record['console_count_correct']=len(record['console_errors'])==(1 if case=='async-unhandled'else 0)
 record['passed']=result.returncode==0 and record.get('console_count_correct',True)
 runs.append(record);(out/'report.json').write_text(json.dumps({'runs':runs,'scope':'Static module evaluation, top-level-await fulfillment/rejection, dynamic imports and module worker closure'},indent=2)+'\n');print(('PASS 'if record['passed']else'FAIL ')+case,flush=True)
assert all(r['passed']for r in runs),[r['case']for r in runs if not r['passed']]
