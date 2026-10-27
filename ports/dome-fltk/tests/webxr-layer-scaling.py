#!/usr/bin/env python3
"""Validate scaled stereo framebuffers through the real browser and native bridge."""
import argparse,contextlib,hashlib,http.server,json,os,re,resource,select,signal,socket,socketserver,struct,subprocess,sys,tempfile,threading,time
from pathlib import Path
from PIL import Image
from Xlib import X,XK
from Xlib.ext import xtest
from wayland_touch_fixture import TouchWayland
from webxr_layer_attributes import validate_attributes

signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_LAYER_SCALE_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_LAYER_SCALE_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__)
for name in ['binary','native-fixture','compositor','output']:p.add_argument('--'+name,type=Path,required=True)
p.add_argument('--lifecycle',action='store_true',help='Also verify opaque protection and depth/stencil release between frames')
p.add_argument('--attributes',action='store_true',help='Validate real buffer formats, reported attributes and implicit clears')
p.add_argument('--opaque-context',action='store_true',help='Use default buffers without alpha/premultiplication in the attribute fixture')
a=p.parse_args();
if a.attributes and a.lifecycle:p.error('--attributes and --lifecycle are separate fixtures')
if a.opaque_context and not a.attributes:p.error('--opaque-context requires --attributes')
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
backend_cleanup={};checks=[];events=[];captures=[];errors=[];child=None;failure=None
caps=dict(width=127,height=95,view_sizes=[[63,95],[64,95]],max_width=512,max_height=256)

def check(value,label):
 checks.append(dict(name=label,passed=bool(value)));print(('PASS ' if value else 'FAIL ')+label,flush=True);assert value,label
def wait(fn):
 end=time.monotonic()+30
 while time.monotonic()<end:
  if child:assert child.poll() is None,'Browser exited unexpectedly'
  assert not errors,errors
  result=fn()
  if result:return result
  time.sleep(.025)
 raise AssertionError('Scaled framebuffer fixture timed out')
def stop(process):
 if process.poll() is None:
  process.terminate()
  try:process.wait(timeout=8)
  except subprocess.TimeoutExpired:process.kill();process.wait()
def exact(stream,count):
 chunks=[]
 while count:
  b=stream.recv(count)
  if not b:raise EOFError('Truncated private bridge message')
  chunks.append(b);count-=len(b)
 return b''.join(chunks)
def read_json(stream):return json.loads(exact(stream,struct.unpack('<I',exact(stream,4))[0]))
def write_json(stream,value):
 b=json.dumps(value).encode();stream.sendall(struct.pack('<I',len(b))+b)

try:
 with contextlib.ExitStack() as stack:
  folder=stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-xr-scale-'));runtime=Path(folder);runtime.chmod(0o700)
  host_log=stack.enter_context((out/'native-host.log').open('w'))
  backend=runtime/'host.sock';host=subprocess.Popen([str(a.native_fixture.resolve()),str(backend)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=host_log,text=True,bufsize=1);stack.callback(stop,host)
  assert select.select([host.stdout],[],[],5)[0] and json.loads(host.stdout.readline())['ready']
  class Relay(socketserver.BaseRequestHandler):
   def handle(self):
    try:
     request=read_json(self.request);size=request.get('bytes',0);assert 0<=size<=4096*4096*4
     pixels=exact(self.request,size) if size else b''
     with socket.socket(socket.AF_UNIX) as native:
      native.settimeout(5);native.connect(str(backend));write_json(native,request)
      if pixels:native.sendall(pixels)
      reply=read_json(native)
     if request['op']=='hello':reply.update(caps)
     if pixels and reply.get('ok'):
      image=Image.frombytes('RGBA',(request['width'],request['height']),pixels)
      marker=image.getpixel((0,image.height-1))[2]
      path=out/('stereo-'+str(marker)+'.png');image.save(path)
      captures.append(dict(header=request,marker=marker,path=str(path),sha256=hashlib.sha256(pixels).hexdigest()))
     write_json(self.request,reply)
    except BaseException as ex:errors.append({'operation':locals().get('request',{}).get('op'),'error':str(ex)})
  class Server(socketserver.ThreadingUnixStreamServer):daemon_threads=True
  relay=Server(str(runtime/'proxy.sock'),Relay);threading.Thread(target=relay.serve_forever,daemon=True).start();stack.callback(relay.server_close);stack.callback(relay.shutdown)
  fixture=TouchWayland(a.compositor.resolve(),runtime,out);stack.callback(fixture.close)
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*_):pass
   def do_GET(self):
    b=(Path(__file__).with_name('webxr-layer-attributes.html') if a.attributes else Path(__file__).with_suffix('.html')).read_bytes();self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(b)));self.end_headers();self.wfile.write(b)
   def do_POST(self):
    events.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))));self.send_response(204);self.end_headers()
  site=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=site.serve_forever,daemon=True).start();stack.callback(site.server_close);stack.callback(site.shutdown)
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')};env.update(HOME=folder,XDG_RUNTIME_DIR=folder,XDG_CONFIG_HOME=folder+'/config',XDG_DATA_HOME=folder+'/data',XDG_STATE_HOME=folder+'/state',XDG_CACHE_HOME=folder+'/cache',FLTK_BACKEND='wayland',WAYLAND_DISPLAY=fixture.wayland.socket,DOME_WEBXR_SOCKET=str(runtime/'proxy.sock'),LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn,webgl::webgl_thread=trace' if a.lifecycle else 'warn')
  for name in ['DISPLAY','WAYLAND_SOCKET','FLTK_SCALING_FACTOR']:env.pop(name,None)
  log=stack.enter_context((out/'browser.log').open('w'));child=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',folder+'/profile',f'http://127.0.0.1:{site.server_port}/'+('#opaque-context' if a.opaque_context else '#lifecycle' if a.lifecycle else '')],env=env,stdout=log,stderr=log);stack.callback(stop,child)
  ready=wait(lambda:next((e['data'] for e in events if e['type']=='ready'),None));window=wait(lambda:next((w for w in fixture.wayland.windows() if w.value()['client_pid']==child.pid),None));window.set_input_focus();g=window.get_geometry();ratio=ready['ratio'];top=g.height-ready['viewport'][1]*ratio-28;v=ready['start'];c=fixture.connection
  xtest.fake_input(c,X.MotionNotify,x=g.x+int((v['x']+v['width']/2)*ratio),y=g.y+int(top+(v['y']+v['height']/2)*ratio));xtest.fake_input(c,X.ButtonPress,1);xtest.fake_input(c,X.ButtonRelease,1);c.sync()
  consent=wait(lambda:next((w for w in fixture.wayland.windows() if w.get_wm_name()=='Enter immersive WebXR'),None));consent.set_input_focus();code=c.keysym_to_keycode(XK.string_to_keysym('Return'));xtest.fake_input(c,X.KeyPress,code);xtest.fake_input(c,X.KeyRelease,code);c.sync()
  def complete():
   types=[e['type'] for e in events]
   if 'error' in types:return True
   if 'done' not in types:return False
   if a.attributes:return types.count('inline')==4 and types.count('allocation')==32 and types.count('buffer')==96 and types.count('after')==96
   return types.count('phase')==18 and (not a.lifecycle or types.count('released')==54 and types.count('opaque')==54 and types.count('transparent')==2)
  wait(complete)
  failures=[e for e in events if e['type']=='error'];check(not failures,'Both WebGL versions complete the requested layer fixture')
  if a.attributes:
   validate_attributes(check,events,captures,wait)
  else:
   phases=sorted([e['data'] for e in events if e['type']=='phase'],key=lambda v:v['marker']);check(len(phases)==18,'Every scale and WebGL version produces a validated frame')
   for phase in phases:
    label=phase['version']+' scale '+str(phase['scale']);w,h=phase['size'];views=phase['views']
    check(phase['error']==0 and phase['status']==0x8CD5,label+': GPU framebuffer is complete without GL errors')
    check(phase['initialSize']==phase['size'],label+': allocation dimensions remain stable through rendering')
    requested=phase['scale']
    check((abs(w-127*requested)<=1 and abs(h-95*requested)<=1) if 0<requested<=1.5 else ((w<127 and h<95) if requested<=0 else h==256),label+': requested scale is applied or clamped to the device bound')
    check(0<w<=caps['max_width'] and 0<h<=caps['max_height'] and len(views)==2,label+': reported allocation fits device limits')
    check(all(v['width']>0 and v['height']>0 and v['x']>=0 and v['y']>=0 and v['x']+v['width']<=w and v['y']+v['height']<=h for v in views) and views[0]['x']+views[0]['width']==views[1]['x'] and views[1]['x']+views[1]['width']==w,label+': eye viewports are positive, adjacent and inside the allocation')
    capture=wait(lambda:next((v for v in reversed(captures) if v['marker']==phase['marker'] and [v['header']['width'],v['header']['height']]==phase['size']),None));image=Image.open(capture['path']);header=capture['header']
    check([v['rect'] for v in header['views']]==[dict(x=v['x'],y=h-v['y']-v['height'],width=v['width'],height=v['height']) for v in views],label+': native submitted rectangles match the WebGL viewports')
    for index,v in enumerate(views):
     x=v['x']+v['width']//2;bottom=[255,0,phase['marker'],255] if index==0 else [0,255,phase['marker'],255];upper=[255,100,phase['marker'],255] if index==0 else [100,255,phase['marker'],255]
     if v['height']==1:bottom=upper
     check(phase['samples'][index]==bottom and list(image.getpixel((x,h-v['y']-1)))==bottom and list(image.getpixel((x,h-v['y']-v['height'])))==upper,label+': '+v['eye']+' eye pixels retain their color and vertical orientation')
   if a.lifecycle:
    # GL 2 attachment-size queries return zero for absent images. Legacy GL 1
    # bit queries can retain the last complete framebuffer's bit depth, so also
    # verify the actual backend command sequence in both contexts.
    for context,image in re.findall(r'processing WebGLCommand\(WebGLContextId\((\d+)\), FramebufferTexture2D\(36160, 33306, 3553, (None|Some\(WebGLTextureId\(\d+\)\)), 0\)',(out/'browser.log').read_text()):
     state=backend_cleanup.setdefault(context,dict(pending=False,valid=True,attached=0,detached=0))
     if image=='None':state['pending']=False;state['detached']+=1
     else:state['valid'] &= not state['pending'];state['pending']=True;state['attached']+=1
    check(len(backend_cleanup)==2,'Both native WebGL contexts provide attachment cleanup traces')
    for context,state in backend_cleanup.items():
     check(state['valid'] and not state['pending'] and state['attached']>=18 and state['detached']>=27,'Context '+context+': every depth/stencil attachment is followed by backend detachment')
    for control in [e['data'] for e in events if e['type']=='transparent']:
     check(control['attached']==0x8CD5 and control['detached']==0x8CD7 and control['reattached']==0x8CD5 and control['error']==0,control['version']+': ordinary framebuffer attachments can still be detached and replaced')
    opaque=sorted([e['data'] for e in events if e['type']=='opaque'],key=lambda v:(v['marker'],v['frameIndex']));released=sorted([e['data'] for e in events if e['type']=='released'],key=lambda v:(v['marker'],v['frameIndex']))
    check(len(opaque)==54 and len(released)==54,'Every rendered frame also validates its opaque lifecycle')
    for active,inactive in zip(opaque,released):
     label=active['version']+' marker '+str(active['marker'])+' frame '+str(active['frameIndex'])
     check(active['marker']==inactive['marker'] and active['frameIndex']==inactive['frameIndex'] and ((active['bits'][0]>0 and active['bits'][1]>0) if active['expectsDepthStencil'] else active['bits']==[0,0]),label+': active depth/stencil storage matches the layer request')
     check(active['rejected']==[0x502]*4 and active['status']==0x8CD5,label+': page cannot change, inspect or delete opaque attachments')
     check(inactive['error']==0 and (active['version']=='webgl' or inactive['bits']==[0,0]),label+(': outside-frame queries complete without errors' if active['version']=='webgl' else ': native depth and stencil attachments are released after the frame'))
     phase=next(v for v in phases if v['marker']==inactive['marker'])
     check(inactive['status']==0x8CDD and inactive['size']==phase['initialSize'],label+': opaque framebuffer remains unavailable outside callbacks with stable dimensions')
  check(child.poll() is None,'Browser survives repeated scaled layer and context replacement')
except BaseException as ex:failure=str(ex);raise
finally:
 (out/'report.json').write_text(json.dumps(dict(passed=failure is None and bool(checks),failure=failure,checks=checks,events=events,captures=captures,relay_errors=errors,backend_cleanup=backend_cleanup,simulated_capabilities=caps,binary_sha256=hashlib.sha256(a.binary.read_bytes()).hexdigest(),scope='Real browser, WebGL GPU buffers and native DomeD bridge with a private synthetic XR host; no headset claim'),indent=2))
