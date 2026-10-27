#!/usr/bin/env python3
"""Compare same-document SVG use references, namespace precedence and source paint."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
from PIL import Image
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,required=True)
p.add_argument('--reference-browser',type=Path)
p.add_argument('--output',type=Path,required=True)
p.add_argument('--base',action='store_true',help='Check that an external base does not alias local IDs')
p.add_argument('--cycles',action='store_true',help='Include a circular reference once import recursion is guarded')
a=p.parse_args();out=a.output.resolve()
command=[sys.executable,str(Path(__file__).with_name('cache-api.py')),'--fixture',str(Path(__file__).with_suffix('.html')),'--stages','write','--capture-frame','--binary',str(a.binary.resolve()),'--output',str(out)]
query='&'.join(flag+'=1'for flag,on in [('cycles',a.cycles),('base',a.base)]if on)
if query:command+=['--path','/?'+query]
if a.reference_browser:command+=['--reference-browser',str(a.reference_browser.resolve())]
subprocess.run(command,check=True)
image=Image.open(out/'write/frame.png').convert('RGB');green=[20,180,90];white=[255,255,255]
cases=[('same-root href',(50,50),green),('cross-root href',(130,50),green),('cross-root xlink href',(210,50),green),('href takes precedence',(290,50),green),('empty href overrides xlink',(370,50),white),('Unicode fragment',(50,150),green),('absolute same-document URL',(130,150),green),('percent-encoded fragment',(210,150),green),('source stylesheet paint',(290,150),green),('external URL never aliases local ID',(370,150),white)]
cases.extend([('translated use does not paint definition at origin',(30,230),white),('translated use paints instance',(50,250),green),('hidden use does not paint imported definition',(130,250),white),('first shared instance paints',(190,230),green),('second shared instance paints',(220,260),green)])
if a.cycles:cases.append(('circular reference is not painted',(290,250),white))
if a.base:cases=[(name,point,green if name=='absolute same-document URL'else white)for name,point,_ in cases]
checks=[]
for name,point,expected in cases:
 actual=list(image.getpixel(point));checks.append({'name':name,'passed':actual==expected,'actual':actual,'expected':expected});print(('PASS 'if actual==expected else'FAIL ')+name+' '+str(actual),flush=True)
raw=json.loads((out/'report.json').read_text());raw['pixel_checks']=checks;(out/'paint.json').write_text(json.dumps(raw,indent=2)+'\n');assert all(c['passed']for c in checks)
