#!/usr/bin/env python3
"""Validate native boxed fallback geometry, text paint, clips and state changes."""
import argparse
import base64
import json
import math
from pathlib import Path
import subprocess
import sys
from PIL import Image

root=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,default=root/'target/release/dome-browser')
p.add_argument('--reference-browser',type=Path)
p.add_argument('--output',type=Path,required=True)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True)
font=root/'components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans-Bold.ttf'
fixture=Path(__file__).with_suffix('.html').read_text().replace('__FONT_DATA__',base64.b64encode(font.read_bytes()).decode())
materialized=out/'materialized.html';materialized.write_text(fixture)
cmd=[sys.executable,str(Path(__file__).with_name('cache-api.py')),'--binary',str(a.binary.resolve()),'--fixture',str(materialized),'--stages','write','--capture-frame','--output',str(out/'browser')]
if a.reference_browser:cmd+=['--reference-browser',str(a.reference_browser.resolve())]
result=subprocess.run(cmd)
report=json.loads((out/'browser/report.json').read_text())['runs'][0]
im=Image.open(out/'browser/write/frame.png').convert('RGB');checks=[]
for case in report['paint']:
 x,y,w,h=case['rect'];box=(math.floor(x),math.floor(y),math.ceil(x+w),math.ceil(y+h))
 assert box[0]>=0 and box[1]>=0 and box[2]<=im.width and box[3]<=im.height,(case,im.size)
 pixels=list(im.crop(box).getdata());red=sum(r>100 and g<80 and b<80 for r,g,b in pixels);green=sum(g>70 and r<80 and b<80 and g>r+40 and g>b+40 for r,g,b in pixels)
 visible=case['visible'];color=case['color'];count=red if color=='red'else green
 passed=(count>15 and (color!='green'or red==0))if visible else red==0
 hx,hy,hw,hh=case['host'];outside=0
 for py in range(max(0,math.floor(hy)),min(im.height,math.ceil(hy+hh))):
  for px in range(max(0,math.floor(hx)),min(im.width,math.ceil(hx+hw))):
   if box[0]<=px<box[2] and box[1]<=py<box[3]:continue
   r,g,b=im.getpixel((px,py))
   outside+=int((r>100 and g<80 and b<80)or(color=='green'and g>70 and r<80 and b<80 and g>r+40 and g>b+40))
 passed=passed and outside==0
 checks.append({'name':case['name'],'passed':passed,'red_pixels':red,'green_pixels':green,'outside_pixels':outside,'rect':box})
 print(('PASS'if passed else'FAIL'),'paint:',case['name'],red,green,flush=True)
(out/'paint.json').write_text(json.dumps({'checks':checks,'geometry_checks':report['checks'],'browser_exit_code':result.returncode},indent=2)+'\n')
raise SystemExit(0 if result.returncode==0 and all(c['passed']for c in checks) else 1)
