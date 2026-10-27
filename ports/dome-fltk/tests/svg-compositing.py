#!/usr/bin/env python3
"""Compare SVG presentation-attribute controls with equivalent stylesheet rules."""
import argparse
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
from PIL import Image

signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_SVG_COMPOSITING_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_SVG_COMPOSITING_BUS':'1'})
source=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,default=source/'target/release/dome-browser');p.add_argument('--reference-browser',type=Path);p.add_argument('--output',type=Path,required=True)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True);page=Path(__file__).with_suffix('.html').read_bytes();(out/'fixture.html').write_bytes(page);reports=[]
class Handler(http.server.BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def do_GET(self):
  self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8');self.send_header('Content-Length',str(len(page)));self.end_headers();self.wfile.write(page)
 def do_POST(self):
  reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))));self.send_response(204);self.end_headers()
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start();xvfb=reference=None
try:
 with tempfile.TemporaryDirectory(prefix='dome-svg-compositing-')as directory:
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1280x900x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
  with os.fdopen(rd)as stream:screen=':'+stream.readline().strip()
  env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','XDG_RUNTIME_DIR':directory,'XDG_DATA_HOME':directory+'/data','XDG_CONFIG_HOME':directory+'/config','XDG_CACHE_HOME':directory+'/cache','DOME_BROWSER_FRAME_CAPTURE':str(out/'frame.png')}
  for key in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(key,None)
  url=f'http://127.0.0.1:{server.server_port}/'
  with(out/'browser.log').open('w')as log:
   if a.reference_browser:
    from reference_browser import ReferenceBrowser
    reference=ReferenceBrowser(a.reference_browser.resolve(),directory,'about:blank',env,log);reference.command('Emulation.setDeviceMetricsOverride',{'width':1280,'height':800,'deviceScaleFactor':1,'mobile':False});reference.command('Page.navigate',{'url':url});deadline=time.monotonic()+20
    while not reports and time.monotonic()<deadline:time.sleep(.05)
    assert reports,'No fixture report';time.sleep(.5);shot=reference.command('Page.captureScreenshot',{'format':'png'});(out/'frame.png').write_bytes(base64.b64decode(shot['data']));closed=reference.close();reference=None
   else:
    result=subprocess.run([str(a.binary.resolve()),'--software','--profile',directory+'/profile','--smoke-test',str(out/'window.ppm'),url],env=env,stdout=log,stderr=log,timeout=90);closed=result.returncode==0
  image=Image.open(out/'frame.png').convert('RGB');checks=[]
  samples = [
   ('group opacity', (50,50), (130,50), [137,217,172]),
   ('display none', (210,50), (290,50), [255,255,255]),
   ('visibility hidden', (370,50), (450,50), [255,255,255]),
   ('clip path outside', (545,50), (625,50), [255,255,255]),
   ('clip path inside', (515,50), (595,50), [20,180,90]),
   ('paint order', (675,50), (755,50), [20,100,180]),
   ('root CSS opacity', (50,150), (130,150), [137,217,172]),
   ('root attribute opacity', (50,150), (210,150), [137,217,172]),
   ('root important inline opacity', (50,150), (290,150), [137,217,172]),
  ]
  for name,control_point,styled_point,expected in samples:
   control=list(image.getpixel(control_point));styled=list(image.getpixel(styled_point))
   passed=all(abs(actual-wanted)<=2 for pixel in [control,styled] for actual,wanted in zip(pixel,expected))
   checks.append({'name':name,'passed':passed,'attribute':control,'stylesheet':styled,'expected':expected})
  styles={entry['id']:entry for report in reports for entry in report['styles']}
  cssom_checks=[
   {'name':'group opacity cascade','passed':all(styles[id]['opacity']=='0.5' for id in ['opacityAttr','opacityCSS'])},
   {'name':'display cascade','passed':all(styles[id]['display']=='none' for id in ['displayAttr','displayCSS'])},
   {'name':'visibility cascade','passed':all(styles[id]['visibility']=='hidden' for id in ['visibilityAttr','visibilityCSS'])},
   {'name':'clip path presentation hint and stylesheet','passed':'#attrClip' in styles['clipAttr']['clipPath'] and '#cssClip' in styles['clipCSS']['clipPath']},
   {'name':'paint order shortest serialization','passed':all(styles[id].get('paintOrder')=='stroke' for id in ['orderAttr','orderCSS'])},
   {'name':'root opacity remains in document CSSOM','passed':all(styles[id]['rootOpacity']=='0.5' for id in ['rootOpacityAttr','rootOpacityCSS','rootOpacityImportant'])},
  ]
  record={'checks':checks,'cssom_checks':cssom_checks,'reports':reports,'clean_close':closed,'fixture_sha256':hashlib.sha256(page).hexdigest(),'binary_sha256':hashlib.sha256((a.reference_browser or a.binary).read_bytes()).hexdigest(),'engine':'Chromium'if a.reference_browser else'Servo'};(out/'report.json').write_text(json.dumps(record,indent=2)+'\n')
  for check in checks:print(('PASS 'if check['passed']else'FAIL ')+check['name']+' '+str(check['attribute'])+' '+str(check['stylesheet']),flush=True)
  for check in cssom_checks:print(('PASS 'if check['passed']else'FAIL ')+'CSSOM '+check['name'],flush=True)
  assert reports and closed and all(check['passed']for check in checks+cssom_checks),record
finally:
 if reference is not None:reference.close()
 if xvfb is not None:xvfb.terminate();xvfb.wait(timeout=5)
 server.shutdown();server.server_close()
