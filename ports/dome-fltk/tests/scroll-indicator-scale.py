#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Measure scaled native document-position cues on isolated X11/Wayland pages."""
from pathlib import Path
import os,sys,subprocess,tempfile,time,select,json,threading,queue,hashlib,argparse,re
from http.server import ThreadingHTTPServer,BaseHTTPRequestHandler
from PIL import Image,ImageChops
from Xlib import display,X,XK,protocol
from Xlib.ext import xtest
from private_wayland import PrivateWayland
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--backend',choices=['wayland','x11'],default='wayland');p.add_argument('--scale',type=float,default=1.0);p.add_argument('--dark',action='store_true');p.add_argument('--hardware',action='store_true',help='Use default accelerated browser rendering and verify the actual renderer');p.add_argument('--zoom-steps',type=int,default=0);a=p.parse_args();assert 0<=a.zoom_steps<=3;assert 1<=a.scale<=2;out=a.output.resolve()
if os.environ.get('DOME_SCROLL_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_SCROLL_BUS':'1'})
out.mkdir(exist_ok=False)
def fingerprint(path):
 with path.open('rb') as stream: return hashlib.file_digest(stream,'sha256').hexdigest()
binary_sha256=fingerprint(a.binary.resolve())
phases=queue.Queue();reports=[]
script='''const wait=ms=>new Promise(r=>setTimeout(r,ms));
const settle=async()=>{await new Promise(requestAnimationFrame);await new Promise(requestAnimationFrame);await wait(80)};
const send=async name=>{await fetch('/phase',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name,width:innerWidth,outer:[outerWidth,outerHeight],dpr:devicePixelRatio,y:scrollY,visible:innerHeight,total:document.scrollingElement.scrollHeight,nested:document.querySelector('#nested')?.scrollTop||0})})};
(async()=>{await wait(500);if(location.pathname==='/short'){await send('short');return;}
await send('calibrate');await settle();await wait(200);await send('zoom-request');await settle();await send('baseline');scrollTo(0,(document.scrollingElement.scrollHeight-innerHeight)/2);await settle();await send('middle');
await wait(1000);document.querySelector('#nested').scrollTop=200;await settle();await send('nested');
scrollTo(0,document.scrollingElement.scrollHeight-innerHeight);await settle();await send('bottom');
await wait(1000);document.body.style.height='9000px';await settle();await send('expanded');
scrollTo(0,(document.scrollingElement.scrollHeight-innerHeight)/2);await settle();await send('dynamic-middle');
await send('wheel-request');await wait(250);await send('wheel');
await send('key-request');await wait(250);await send('keyboard');location.href='/short';})();'''
def pointer_position(window,g,data,scale,initial_dpr):
 zoom=data['dpr']/initial_dpr
 px=max(1,g.width-round(40*scale));py=round(g.height-28*scale-data['visible']*zoom*scale*.4)
 if wayland:
  host=wayland.host.get_geometry();return type('Point',(),{'x':host.x+g.x+px,'y':host.y+g.y+py})()
 return connection.screen().root.translate_coords(window,px,py)
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_GET(self):
  if self.path=='/probe.js':body=script.encode();kind='text/javascript'
  else:
   height=100 if self.path=='/short'else 5000;bg='#111'if a.dark else'white'
   body=(f'<!doctype html><meta charset="utf-8"><title>Scroll indicator fixture</title><style>html{{background:{bg}}}body{{margin:0;height:{height}px;background:{bg}}}#nested{{position:fixed;left:10px;top:10px;width:200px;height:100px;overflow:auto}}#inside{{height:2000px}}</style><div id="nested"><div id="inside">Nested scroll fixture</div></div><script src="/probe.js"></script>').encode();kind='text/html'
  self.send_response(200);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def do_POST(self):
  data=json.loads(self.rfile.read(int(self.headers['Content-Length'])));ack=threading.Event();phases.put((data,ack));ack.wait(20);self.send_response(200);self.end_headers()
server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start();app=xvfb=connection=wayland=None
try:
 with tempfile.TemporaryDirectory(prefix='dome-scroll-browser-')as tmp:
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','3600x2400x24','-nolisten','tcp'],pass_fds=[wr],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);os.close(wr);screen=':'+os.read(rd,128).decode().strip();os.close(rd);connection=display.Display(screen)
  if a.backend=='wayland':
   wayland=PrivateWayland(connection,screen,Path(tmp),out,hardware=a.hardware);wayland.host.configure(width=3400,height=2250);connection.sync();time.sleep(.3)
  env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','FLTK_SCALING_FACTOR':str(a.scale),'LIBGL_ALWAYS_SOFTWARE':'1','HOME':tmp,'XDG_RUNTIME_DIR':tmp,'XDG_CONFIG_HOME':tmp+'/config','XDG_DATA_HOME':tmp+'/data','XDG_CACHE_HOME':tmp+'/cache','LD_LIBRARY_PATH':'/opt/fltk-vr/toolkit/lib:/opt/fltk-vr/lib:/opt/fltk-vr/lib/bitmap'};env.pop('WAYLAND_DISPLAY',None);env.pop('WAYLAND_SOCKET',None)
  env['RUST_LOG']='warn,webrender::device::gl=info'
  if a.hardware:env.pop('LIBGL_ALWAYS_SOFTWARE',None)
  if wayland:env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket);env.pop('DISPLAY',None)
  log=(out/'browser.log').open('w');app=subprocess.Popen([str(a.binary.resolve()),*([]if a.hardware else['--software']),'--no-restore','--profile',tmp+'/profile',f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log)
  baseline=None;previous=None;window=None;scale=None;initial_dpr=None
  while True:
   try:data,ack=phases.get(timeout=20)
   except queue.Empty:raise AssertionError('Browser fixture timed out; browser exit='+str(app.poll()))
   try:
    assert app.poll() is None,'Browser exited during fixture'
    window=(next(w for w in wayland.windows() if w.value()['client_pid']==app.pid)if wayland else next(w for w in connection.screen().root.query_tree().children if w.get_attributes().map_state==X.IsViewable and w.get_geometry().width>=720))
    g=window.get_geometry();name=data['name']
    if name=='calibrate':
     before={'metrics':data,'window':window.value()if wayland else {'width':g.width,'height':g.height}}
     if wayland:
      width=round(data['outer'][0]*a.scale);height=round(data['outer'][1]*a.scale)
      assert 160<=width<=3200 and 100<=height<=2100,('Fixture target must fit private output',width,height)
      wayland.api('/api/action',{'op':'desktop_resize','id':window.id,'w':width,'h':height})
      before['requested_desktop_size']=[width,height]
     (out/'scale-calibration.json').write_text(json.dumps(before,indent=2)+'\n')
     continue
    if name=='zoom-request':
     scale=g.width/data['width'];initial_dpr=data['dpr'];assert abs(scale-a.scale)<.08,('Actual scale not established',scale,a.scale,g.width,data['width'])
     window.set_input_focus(X.RevertToParent,X.CurrentTime);connection.sync()
     pos=pointer_position(window,g,data,scale,initial_dpr);xtest.fake_input(connection,X.MotionNotify,x=pos.x,y=pos.y);xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync();time.sleep(.1)
     ctrl=connection.keysym_to_keycode(XK.XK_Control_L);shift=connection.keysym_to_keycode(XK.XK_Shift_L);equal=connection.keysym_to_keycode(XK.XK_equal)
     for _ in range(a.zoom_steps):
      xtest.fake_input(connection,X.KeyPress,ctrl);xtest.fake_input(connection,X.KeyPress,shift);xtest.fake_input(connection,X.KeyPress,equal);xtest.fake_input(connection,X.KeyRelease,equal);xtest.fake_input(connection,X.KeyRelease,shift);xtest.fake_input(connection,X.KeyRelease,ctrl);connection.sync();time.sleep(.1)
     continue
    assert scale is not None
    zoom=data['dpr']/initial_dpr
    assert abs(g.width/data['width']/zoom-scale)<.08,('UI scale changed unexpectedly',g.width,data,scale)
    if a.zoom_steps:assert zoom>1.05,('Native page zoom shortcut did not change zoom',data)

    if name in ['wheel-request','key-request']:
     window.set_input_focus(X.RevertToParent,X.CurrentTime);pos=pointer_position(window,g,data,scale,initial_dpr)
     xtest.fake_input(connection,X.MotionNotify,x=pos.x,y=pos.y);xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1)
     if name=='wheel-request':
      for _ in range(3):xtest.fake_input(connection,X.ButtonPress,5);xtest.fake_input(connection,X.ButtonRelease,5)
     else:
      code=connection.keysym_to_keycode(XK.XK_Next);xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
     connection.sync();continue
    time.sleep(.18);raw=window.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff);image=Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX');image.save(out/(name+'.png'));edge=image.crop((g.width-round(12*scale),g.height-round(data['visible']*zoom*scale)-round(28*scale),g.width,g.height-round(28*scale)))
    if name=='baseline':baseline=edge;box=None;passed=data['y']==0
    else:
     box=ImageChops.difference(baseline,edge).getbbox()
     if name in ['nested','expanded','short']:passed=box is None
     else:
      track=edge.height-12*scale;length=max(18*scale,track*data['visible']/data['total']);expected=6*scale+(track-length)*data['y']/(data['total']-data['visible']);passed=box is not None and abs(box[1]-expected)<4*scale
      if name in ['wheel','keyboard']:passed=passed and data['y']>previous['y']
    result={**data,'marker_bounds':box,'actual_ui_scale':scale,'actual_page_zoom':zoom,'passed':passed};reports.append(result);print(json.dumps(result),flush=True);assert passed,result
    previous=data
    if name=='short':break
   finally:ack.set()
  if wayland:wayland.api('/api/action',{'op':'close','id':window.id})
  else:
   atom=connection.intern_atom('WM_PROTOCOLS');delete=connection.intern_atom('WM_DELETE_WINDOW');window.send_event(protocol.event.ClientMessage(window=window,client_type=atom,data=(32,[delete,X.CurrentTime,0,0,0])));connection.flush()
  app.wait(timeout=10);assert app.returncode==0
  assert fingerprint(a.binary.resolve())==binary_sha256,'Candidate binary changed during runtime validation'
  log.flush()
  renderers=sorted(set(re.findall(r'Renderer: ([^\r\n]+)',(out/'browser.log').read_text())))
  assert renderers and all(any(token in name.lower()for token in ['llvmpipe','softpipe','software rasterizer'])==(not a.hardware)for name in renderers),('Actual renderer does not match requested graphics mode',renderers,a.hardware)
  report={'passed':True,'binary_sha256':binary_sha256,'script_sha256':fingerprint(Path(__file__)), 'backend':a.backend,'hardware':a.hardware,'renderers':renderers,'requested_ui_scale':a.scale,'zoom_steps':a.zoom_steps,'dark_page':a.dark,'cases':reports,'browser_exit_code':app.returncode,'binary':str(a.binary.resolve()),'scope':'Actual Servo metrics and native overlay at verified UI scale, isolated browser with verified graphics mode, local pages and native wheel/keyboard input','desktop_sessions_restarted':False};(out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
finally:
 server.shutdown()
 if app and app.poll() is None:app.terminate();app.wait(timeout=5)
 if wayland:wayland.close()
 if connection:connection.close()
 if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
