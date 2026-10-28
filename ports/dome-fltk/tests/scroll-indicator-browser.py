#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Validate native document position cues and clean shutdown on isolated local pages."""
from pathlib import Path
import os,sys,subprocess,tempfile,time,select,json,threading,queue,hashlib,argparse
from http.server import ThreadingHTTPServer,BaseHTTPRequestHandler
from PIL import Image,ImageChops
from Xlib import display,X,XK,protocol
from Xlib.ext import xtest
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();out=a.output.resolve()
if os.environ.get('DOME_SCROLL_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_SCROLL_BUS':'1'})
out.mkdir(exist_ok=False)
def fingerprint(path):
 with path.open('rb') as stream: return hashlib.file_digest(stream,'sha256').hexdigest()
binary_sha256=fingerprint(a.binary.resolve())
phases=queue.Queue();reports=[]
script='''const wait=ms=>new Promise(r=>setTimeout(r,ms));
const settle=async()=>{await new Promise(requestAnimationFrame);await new Promise(requestAnimationFrame);await wait(80)};
const send=async name=>{await fetch('/phase',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name,y:scrollY,visible:innerHeight,total:document.scrollingElement.scrollHeight,nested:document.querySelector('#nested')?.scrollTop||0})})};
(async()=>{await wait(500);if(location.pathname==='/short'){await send('short');return;}
await send('baseline');scrollTo(0,(document.scrollingElement.scrollHeight-innerHeight)/2);await settle();await send('middle');
await wait(1000);document.querySelector('#nested').scrollTop=200;await settle();await send('nested');
scrollTo(0,document.scrollingElement.scrollHeight-innerHeight);await settle();await send('bottom');
await wait(1000);document.body.style.height='9000px';await settle();await send('expanded');
scrollTo(0,(document.scrollingElement.scrollHeight-innerHeight)/2);await settle();await send('dynamic-middle');
await send('wheel-request');await wait(250);await send('wheel');
await send('key-request');await wait(250);await send('keyboard');location.href='/short';})();'''
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_GET(self):
  if self.path=='/probe.js':body=script.encode();kind='text/javascript'
  else:
   height=100 if self.path=='/short'else 5000
   body=(f'<!doctype html><meta charset="utf-8"><title>Scroll indicator fixture</title><style>html{{background:white}}body{{margin:0;height:{height}px;background:white}}#nested{{position:fixed;left:10px;top:10px;width:200px;height:100px;overflow:auto}}#inside{{height:2000px}}</style><div id="nested"><div id="inside">Nested scroll fixture</div></div><script src="/probe.js"></script>').encode();kind='text/html'
  self.send_response(200);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def do_POST(self):
  data=json.loads(self.rfile.read(int(self.headers['Content-Length'])));ack=threading.Event();phases.put((data,ack));ack.wait(20);self.send_response(200);self.end_headers()
server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start();app=xvfb=connection=None
try:
 with tempfile.TemporaryDirectory(prefix='dome-scroll-browser-')as tmp:
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1440x1000x24','-nolisten','tcp'],pass_fds=[wr],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);os.close(wr);screen=':'+os.read(rd,128).decode().strip();os.close(rd);connection=display.Display(screen)
  env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','HOME':tmp,'XDG_RUNTIME_DIR':tmp,'XDG_CONFIG_HOME':tmp+'/config','XDG_DATA_HOME':tmp+'/data','XDG_CACHE_HOME':tmp+'/cache','LD_LIBRARY_PATH':'/opt/fltk-vr/toolkit/lib:/opt/fltk-vr/lib:/opt/fltk-vr/lib/bitmap'};env.pop('WAYLAND_DISPLAY',None);env.pop('WAYLAND_SOCKET',None)
  log=(out/'browser.log').open('w');app=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',tmp+'/profile',f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log)
  baseline=None;previous=None;window=None
  while True:
   try:data,ack=phases.get(timeout=20)
   except queue.Empty:raise AssertionError('Browser fixture timed out; browser exit='+str(app.poll()))
   try:
    assert app.poll() is None,'Browser exited during fixture'
    window=next(w for w in connection.screen().root.query_tree().children if w.get_attributes().map_state==X.IsViewable and w.get_geometry().width>=720)
    g=window.get_geometry();name=data['name']
    if name in ['wheel-request','key-request']:
     window.set_input_focus(X.RevertToParent,X.CurrentTime);pos=connection.screen().root.translate_coords(window,400,320)
     xtest.fake_input(connection,X.MotionNotify,x=pos.x,y=pos.y);xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1)
     if name=='wheel-request':
      for _ in range(3):xtest.fake_input(connection,X.ButtonPress,5);xtest.fake_input(connection,X.ButtonRelease,5)
     else:
      code=connection.keysym_to_keycode(XK.XK_Next);xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
     connection.sync();continue
    time.sleep(.18);raw=window.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff);image=Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX');image.save(out/(name+'.png'));edge=image.crop((g.width-12,120,g.width,g.height-28))
    if name=='baseline':baseline=edge;box=None;passed=data['y']==0
    else:
     box=ImageChops.difference(baseline,edge).getbbox()
     if name in ['nested','expanded','short']:passed=box is None
     else:
      track=edge.height-12;length=max(18,track*data['visible']/data['total']);expected=6+(track-length)*data['y']/(data['total']-data['visible']);passed=box is not None and abs(box[1]-expected)<4
      if name in ['wheel','keyboard']:passed=passed and data['y']>previous['y']
    result={**data,'marker_bounds':box,'passed':passed};reports.append(result);print(json.dumps(result),flush=True);assert passed,result
    previous=data
    if name=='short':break
   finally:ack.set()
  atom=connection.intern_atom('WM_PROTOCOLS');delete=connection.intern_atom('WM_DELETE_WINDOW');window.send_event(protocol.event.ClientMessage(window=window,client_type=atom,data=(32,[delete,X.CurrentTime,0,0,0])));connection.flush();app.wait(timeout=10);assert app.returncode==0
  assert fingerprint(a.binary.resolve())==binary_sha256,'Candidate binary changed during runtime validation'
  report={'passed':True,'binary_sha256':binary_sha256,'script_sha256':fingerprint(Path(__file__)), 'cases':reports,'browser_exit_code':app.returncode,'binary':str(a.binary.resolve()),'scope':'Actual Servo document metrics and FLTK overlay through isolated X11/software browser, local standard pages and native wheel/keyboard input','desktop_sessions_restarted':False};(out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
finally:
 server.shutdown()
 if app and app.poll() is None:app.terminate();app.wait(timeout=5)
 if connection:connection.close()
 if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
