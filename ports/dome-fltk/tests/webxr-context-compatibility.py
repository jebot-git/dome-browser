#!/usr/bin/env python3
"""Validate XR compatibility against real native GL contexts and a private XR host."""
import argparse,contextlib,hashlib,http.server,json,os,re,resource,select,signal,socket,socketserver,struct,subprocess,sys,tempfile,threading,time
from pathlib import Path
from PIL import Image
from Xlib import X,XK
from Xlib.ext import xtest
from wayland_touch_fixture import TouchWayland

signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_LAYER_SCALE_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_LAYER_SCALE_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__)
for name in ['binary','native-fixture','compositor','output']:p.add_argument('--'+name,type=Path,required=True)
p.add_argument('--no-xr-host',action='store_true',help='Check failure without an immersive device')
p.add_argument('--policy-denied',action='store_true',help='Validate response policy with a live private host available')
p.add_argument('--context-loss',action='store_true',help='Lose and recreate active immersive WebGL contexts')
a=p.parse_args()
if a.policy_denied and (a.context_loss or a.no_xr_host):p.error('Policy scenario uses its own live-host mode')
if a.context_loss and a.no_xr_host:p.error('context loss requires the private XR host')
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
host_timings=[];host_ops=[];availability={'enabled':True};checks=[];events=[];captures=[];errors=[];child=None;failure=None
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
 raise AssertionError('Context compatibility fixture timed out')
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
    timing={'start':time.monotonic(),'phase':'read'};host_timings.append(timing)
    try:
     request=read_json(self.request);timing.update(operation=request['op'],phase='native',read=time.monotonic());host_ops.append(request['op']);size=request.get('bytes',0);assert 0<=size<=4096*4096*4
     pixels=exact(self.request,size) if size else b''
     with socket.socket(socket.AF_UNIX) as native:
      native.settimeout(5);native.connect(str(backend));write_json(native,request)
      if pixels:native.sendall(pixels)
      reply=read_json(native)
     timing.update(native_done=time.monotonic(),phase='reply')
     if request['op']=='hello':
      if availability['enabled']:reply.update(caps)
      else:reply={'ok':False,'error':'Private XR fixture unavailable'}
     if pixels and reply.get('ok'):
      image=Image.frombytes('RGBA',(request['width'],request['height']),pixels)
      marker=image.getpixel((0,image.height-1))[2]
      path=out/('stereo-'+str(marker)+'.png');image.save(path)
      captures.append(dict(header=request,marker=marker,path=str(path),sha256=hashlib.sha256(pixels).hexdigest()))
     write_json(self.request,reply);timing.update(finish=time.monotonic(),phase='done')
    except BaseException as ex:
     timing.update(finish=time.monotonic(),error=str(ex));errors.append(str(ex))
  class Server(socketserver.ThreadingUnixStreamServer):daemon_threads=True
  relay=Server(str(runtime/'proxy.sock'),Relay);threading.Thread(target=relay.serve_forever,daemon=True).start();stack.callback(relay.server_close);stack.callback(relay.shutdown)
  fixture=TouchWayland(a.compositor.resolve(),runtime,out);stack.callback(fixture.close)
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*_):pass
   def do_GET(self):
    b=json.dumps(host_ops).encode() if self.path=='/host-ops' else (Path(__file__).with_name('webxr-permissions-policy-native.html') if a.policy_denied else Path(__file__).with_suffix('.html')).read_bytes();self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(b)));
    if a.policy_denied:self.send_header('Permissions-Policy','xr-spatial-tracking=()')
    self.end_headers();self.wfile.write(b)
   def do_POST(self):
    value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
    if self.path=='/availability':availability['enabled']=bool(value)
    else:events.append(value)
    self.send_response(204);self.end_headers()
  site=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=site.serve_forever,daemon=True).start();stack.callback(site.server_close);stack.callback(site.shutdown)
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')};env.update(HOME=folder,XDG_RUNTIME_DIR=folder,XDG_CONFIG_HOME=folder+'/config',XDG_DATA_HOME=folder+'/data',XDG_STATE_HOME=folder+'/state',XDG_CACHE_HOME=folder+'/cache',FLTK_BACKEND='wayland',WAYLAND_DISPLAY=fixture.wayland.socket,DOME_WEBXR_SOCKET=str(runtime/('absent.sock' if a.no_xr_host else 'proxy.sock')),LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn')
  for name in ['DISPLAY','WAYLAND_SOCKET','FLTK_SCALING_FACTOR']:env.pop(name,None)
  log=stack.enter_context((out/'browser.log').open('w'));child=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',folder+'/profile',f'http://127.0.0.1:{site.server_port}/'+('#no-host' if a.no_xr_host else '#context-loss' if a.context_loss else '')],env=env,stdout=log,stderr=log);stack.callback(stop,child)
  ready=wait(lambda:next((e['data'] for e in events if e['type']=='ready'),None));window=wait(lambda:next((w for w in fixture.wayland.windows() if w.value()['client_pid']==child.pid),None));window.set_input_focus();g=window.get_geometry();ratio=ready['ratio'];top=g.height-ready['viewport'][1]*ratio-28;v=ready['start'];c=fixture.connection
  xtest.fake_input(c,X.MotionNotify,x=g.x+int((v['x']+v['width']/2)*ratio),y=g.y+int(top+(v['y']+v['height']/2)*ratio));xtest.fake_input(c,X.ButtonPress,1);xtest.fake_input(c,X.ButtonRelease,1);c.sync()
  if not a.no_xr_host and not a.policy_denied:
   consent=wait(lambda:next((w for w in fixture.wayland.windows() if w.get_wm_name()=='Enter immersive WebXR'),None));consent.set_input_focus();code=c.keysym_to_keycode(XK.string_to_keysym('Return'));xtest.fake_input(c,X.KeyPress,code);xtest.fake_input(c,X.KeyRelease,code);c.sync()
  def completed():
   if any(e['type']=='error' for e in events):return True
   done=next((e['data'] for e in events if e['type']=='done'),None)
   return done and sum(e['type']=='check' for e in events)==done['count']
  wait(completed)
  check(not [e for e in events if e['type']=='error'],'Both WebGL versions complete context compatibility validation')
  done=next(e['data'] for e in events if e['type']=='done');check(done['versions']==['webgl','webgl2'] and done['noHost']==a.no_xr_host,'The requested native device mode covers both WebGL versions')
  for event in sorted([e for e in events if e['type']=='check'],key=lambda e:e['order']):check(event['data']['passed'],event['data']['name'])
  if a.no_xr_host or a.policy_denied:
   check(not host_ops and not captures,'No immersive host is contacted or leased when policy denies tracking' if a.policy_denied else 'No immersive host is contacted or leased when unavailable')
  else:
   check(host_ops.count('begin_session')==1 and host_ops.count('end_session')==1,'Only the explicitly consented immersive session consumes a lease')
   for marker in ([70,71,78,79] if a.context_loss else [70,71]):
    capture=wait(lambda:next((v for v in reversed(captures) if v['marker']==marker),None));im=Image.open(capture['path']);h=capture['header'];check(h['layer_flags']==2 and list(im.getpixel((31,im.height//2)))==[255,0,marker,255] and list(im.getpixel((95,im.height//2)))==[0,255,marker,255],'Negotiated '+('WebGL 1' if marker in [70,78] else 'WebGL 2')+' context submits correct stereo pixels')
  check(child.poll() is None,'Browser survives context negotiation, device absence and reconnection')
  if a.context_loss or a.policy_denied:
   fixture.wayland.api('/api/action',{'op':'close','id':window.id})
   check(child.wait(timeout=10)==0,'Browser closes cleanly after native XR validation')
except BaseException as ex:failure=str(ex);raise
finally:
 (out/'report.json').write_text(json.dumps(dict(passed=failure is None and bool(checks),failure=failure,checks=checks,events=events,captures=captures,relay_errors=errors,host_operations=host_ops,host_timings=host_timings,no_xr_host=a.no_xr_host,context_loss=a.context_loss,policy_denied=a.policy_denied,simulated_capabilities=caps,binary_sha256=hashlib.sha256(a.binary.read_bytes()).hexdigest(),scope='Real browser, WebGL GPU buffers and native DomeD bridge with a private synthetic XR host; no headset claim'),indent=2))
