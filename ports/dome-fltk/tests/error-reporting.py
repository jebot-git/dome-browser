#!/usr/bin/env python3
"""Verify canceled errors stay out of the console and unhandled errors report once."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

source=Path(__file__).resolve().parents[3]
cases=['window-listener-cancel','window-handler-true','window-handler-false','window-unhandled','worker-self-cancel','worker-owner-cancel','worker-window-cancel','worker-unhandled']
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,default=source/'target/release/dome-browser')
p.add_argument('--reference-browser',type=Path)
p.add_argument('--output',type=Path,required=True)
p.add_argument('--cases',nargs='+',choices=cases,default=cases)
p.add_argument('--values',nargs='+',choices=['error','undefined'],default=['error','undefined'])
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True);runs=[]
for case in a.cases:
 for value in a.values:
  folder=out/(case+'-'+value)
  command=[sys.executable,str(Path(__file__).with_name('cache-api.py')),'--fixture',str(Path(__file__).with_suffix('.html')),'--stages','write','--path','/?case='+case+'&value='+value,'--timeout','20','--binary',str(a.binary.resolve()),'--output',str(folder)]
  if a.reference_browser:command+=['--reference-browser',str(a.reference_browser.resolve()),'--capture-exceptions']
  with(out/(case+'-'+value+'.log')).open('w')as log:result=subprocess.run(command,stdout=log,stderr=log)
  expected=1 if case in ['window-handler-false','window-unhandled','worker-unhandled'] else 0
  record={'case':case,'value':value,'expected_console_errors':expected,'exit_code':result.returncode}
  report=folder/'report.json'
  if report.exists():
   record['report']=json.loads(report.read_text())
   if a.reference_browser:
    all_events=record['report']['runs'][0].get('reference_exception_events',[])
    relevant=[event for event in all_events if event['method']=='Runtime.exceptionThrown' or (event['method']=='Log.entryAdded' and event['params']['entry'].get('source') in ['javascript','worker'] and event['params']['entry'].get('level')=='error')]
    record['console_errors']=relevant
   else:
    record['console_errors']=[line for line in (folder/'write/browser.log').read_text(errors='replace').splitlines()if line.startswith('PAGE Error ')]
   record['actual_console_errors']=len(record['console_errors'])
  record['passed']=result.returncode==0 and record.get('actual_console_errors')==expected
  runs.append(record);(out/'report.json').write_text(json.dumps({'runs':runs,'scope':'DOM error propagation, cancellation and console reporting; native embedder or reference exception events'},indent=2)+'\n')
  print(('PASS 'if record['passed']else'FAIL ')+case+' '+value+': console '+str(record.get('actual_console_errors'))+'/'+str(expected),flush=True)
assert all(record['passed']for record in runs),[(r['case'],r['value'],r.get('actual_console_errors'),r['expected_console_errors'])for r in runs if not r['passed']]
