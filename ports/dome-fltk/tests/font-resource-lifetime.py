#!/usr/bin/env python3
"""Measure unpainted CSS font lifetime in a disposable software-rendered browser.

The baseline and final windows surround 96 load/remove cycles after eight warmup
cycles. Memory growth is diagnostic; deterministic ownership assertions live in
the native font unit tests.
"""
import argparse,json,os,statistics,subprocess,sys,time
from pathlib import Path
from Xlib import display, error
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();binary=a.binary.resolve();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False,mode=0o700)
root=Path(__file__).resolve().parents[3];fixture=Path(__file__).with_suffix('.html');samples=[];xd=None
cmd=[sys.executable,str(root/'ports/dome-fltk/tests/cache-api.py'),'--binary',str(binary),'--fixture',str(fixture),'--stages','write','--output',str(out/'browser')]
with(out/'harness.log').open('w')as log:
 proc=subprocess.Popen(cmd,stdout=log,stderr=log);started=time.monotonic();pid=None
 try:
  while proc.poll()is None:
   if pid is None:
    for path in Path('/proc').iterdir():
     if not path.name.isdigit():continue
     try:
      if os.readlink(path/'exe')==str(binary):pid=int(path.name);break
     except OSError:pass
   if pid:
    try:
     path=Path('/proc')/str(pid)
     if xd is None:
      env=dict(item.split(b'=',1)for item in(path/'environ').read_bytes().split(b'\0')if b'='in item);xd=display.Display(env[b'DISPLAY'].decode())
     phase='other'
     for w in xd.screen().root.query_tree().children:
      title=w.get_wm_name()or''
      for name in ['FontResourceBaseline','FontResourceFinal']:
       if name in title:phase=name
     memory={}
     for line in(path/'smaps_rollup').read_text().splitlines():
      fields=line.split()
      if len(fields)>=2 and fields[0]in['Rss:','Pss:','Private_Dirty:','Private_Clean:']:memory[fields[0][:-1]+'_kib']=int(fields[1])
     samples.append({'elapsed':round(time.monotonic()-started,3),'phase':phase,**memory})
    except (OSError,KeyError,error.ConnectionClosedError,error.BadWindow):pass
   time.sleep(.1)
 finally:
  (out/'samples.json').write_text(json.dumps(samples,indent=2)+'\n')
  if xd:
   try:xd.close()
   except error.ConnectionClosedError:pass
assert proc.returncode==0,proc.returncode
raw=json.loads((out/'browser/report.json').read_text());requests=[r for r in raw['requests']if r['path'].startswith('/font-api.ttf?unused-lifetime=')];assert len(requests)==104,len(requests)
summary={}
for phase in ['FontResourceBaseline','FontResourceFinal']:
 all_rows=[r for r in samples if r['phase']==phase];assert all_rows,phase
 rows=[r for r in all_rows if .25<=r['elapsed']-all_rows[0]['elapsed']<=1.75];assert len(rows)>=5,(phase,len(rows))
 summary[phase]={'samples':len(rows),'all_phase_samples':len(all_rows),'window':'0.25 to1.75s after phase title appears, before the2s idle ends',**{k:statistics.median(r[k]for r in rows)for k in ['Rss_kib','Pss_kib','Private_Dirty_kib','Private_Clean_kib']}}
summary.update(binary_sha256=raw['binary_sha256'],requests=len(requests),clean_close=raw['runs'][0]['clean_close'],rss_growth_kib=summary['FontResourceFinal']['Rss_kib']-summary['FontResourceBaseline']['Rss_kib'],private_dirty_growth_kib=summary['FontResourceFinal']['Private_Dirty_kib']-summary['FontResourceBaseline']['Private_Dirty_kib'])
(out/'memory.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
