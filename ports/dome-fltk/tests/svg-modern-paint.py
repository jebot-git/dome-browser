#!/usr/bin/env python3
"""Compare native SVG raster pixels and CSSOM for modern colors and variables."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
from PIL import Image

source=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,default=source/'target/release/dome-browser')
p.add_argument('--reference-browser',type=Path)
p.add_argument('--output',type=Path,required=True)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True);runs=[]
points={'rgb':(35,35),'current':(115,35),'modern':(195,35),'attribute':(275,35),
        'css':(355,35),'gradient':(435,35),'stroke':(490,35),'fallback':(595,35),
        'alpha':(675,35),'nested':(755,35),'invalid':(835,35),'unset':(915,35)}
for mutate in [False,True]:
 name='mutated'if mutate else'initial';folder=out/name
 command=[sys.executable,str(Path(__file__).with_name('cache-api.py')),'--binary',str(a.binary.resolve()),
          '--fixture',str(Path(__file__).with_suffix('.html')),'--stages','write','--capture-frame',
          '--path','/?mutate=1'if mutate else'/','--output',str(folder)]
 if a.reference_browser:command+=['--reference-browser',str(a.reference_browser.resolve())]
 with(out/(name+'.log')).open('w')as log:result=subprocess.run(command,stdout=log,stderr=log)
 record={'case':name,'exit_code':result.returncode,'checks':{},'pixels':{}}
 report=folder/'report.json';frame=folder/'write/frame.png'
 if report.exists()and frame.exists():
  data=json.loads(report.read_text());styles={s['id']:s for s in data['runs'][0]['styles']}
  image=Image.open(frame).convert('RGB');paint=[220,40,80]if mutate else[20,180,90]
  for key,point in points.items():
   color=list(image.getpixel(point));expected=paint if key in ['attribute','css','nested','invalid','unset']else[222,222,222]if key=='alpha'else[190,190,190]
   record['pixels'][key]=color
   record['checks']['pixel '+key]=all(abs(x-y)<=2 for x,y in zip(color,expected))
  for key in ['attribute','nested','invalid','unset']:
   record['checks']['computed '+key]=styles[key]['fill']=='rgb('+', '.join(map(str,paint))+')'
  record['checks']['modern CSSOM preserved']=styles['modern']['fill'].startswith('oklab(')
  record['fixture_sha256']=data['fixture_sha256']
 record['passed']=result.returncode==0 and bool(record['checks'])and all(record['checks'].values())
 runs.append(record);(out/'report.json').write_text(json.dumps({'runs':runs},indent=2)+'\n')
 print(('PASS 'if record['passed']else'FAIL ')+name,flush=True)
 for key,passed in record['checks'].items():
  if not passed:print('FAIL '+key,flush=True)
assert all(r['passed']for r in runs),runs
