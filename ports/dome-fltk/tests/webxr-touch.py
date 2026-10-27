#!/usr/bin/env python3
"""Exercise trusted touch/pen through a separate Servo embedder, not page-created input."""
import argparse,contextlib,hashlib,http.server,json,math,os,queue,resource,signal,subprocess,sys,threading,time,tempfile,traceback
signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get("DOME_TOUCH_TEST_BUS")!="1":
 os.execvpe("dbus-run-session",["dbus-run-session","--",sys.executable,*sys.argv],{**os.environ,"DOME_TOUCH_TEST_BUS":"1"})
from pathlib import Path
from Xlib import display
from private_wayland import PrivateWayland

p=argparse.ArgumentParser(description=__doc__);p.add_argument('binary',type=Path);p.add_argument('output',type=Path);a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
binary=a.binary.resolve();binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest()
checks=[];child=None;token=0;replies=queue.Queue();messages=[];last=None;thread_samples=[]

def stop(proc):
 if proc.poll() is None:
  proc.terminate()
  try:proc.wait(timeout=8)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()
def check(value,label):
 assert value,label
 checks.append(label)
def ask(op,**kwargs):
 global token
 token+=1;expected=token
 child.stdin.write(json.dumps({'op':op,'token':token,**kwargs})+'\n');child.stdin.flush()
 deadline=time.monotonic()+15
 while time.monotonic()<deadline:
  if child.poll() is not None:raise AssertionError(f'Embedder exited: {child.returncode}')
  try:r=replies.get(timeout=.1)
  except queue.Empty:continue
  assert r['token']==expected,(r,expected)
  assert 'error' not in r,r
  return r
 raise AssertionError(f'Timeout waiting for {op}')
def evaluate(script):return json.loads(ask('eval',script='JSON.stringify('+script+')')['value'])
def snap():
 global last
 last=evaluate('window.test ? window.test.snapshot() : null')
 if last:assert not last['error'],last['error']
 return last
def wait(fn,label='condition'):
 deadline=time.monotonic()+12
 while time.monotonic()<deadline:
  try:s=snap()
  except AssertionError as error:
   if last is None and 'InternalError' in str(error):time.sleep(.025);continue
   raise
  if s:
   value=fn(s)
   if value:return s
  time.sleep(.025)
 raise AssertionError('Timed out: '+label)
def mark():return len(snap()['events'])
def events(start=0,index=0):return [e for e in snap()['events'][start:] if e.get('index',index)==index]
def named(name,start=0,index=0):return [e for e in events(start,index) if e['name']==name]
def touch(phase,identity=10,kind='touch',x=205,y=195,compat=False):return ask('touch',phase=phase,id=identity,kind=kind,x=x,y=y,compat=compat)
def command(op):
 evaluate('window.test.command('+json.dumps(op)+')')
 before=snap()['frames'];wait(lambda s:s['frames']>before+4)
def zero():wait(lambda s:s['lengths']==[0,0],'no retained sources')
def ray_close(ray,want):
 n=math.sqrt(sum(x*x for x in want));return ray and all(abs(x-y/n)<.025 for x,y in zip(ray,want))

try:
 with contextlib.ExitStack() as stack:
  runtime=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-xr-touch-')));runtime.chmod(0o700)
  rd,wr=os.pipe()
  xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1200x900x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr)
  screen=':'+os.read(rd,64).decode().strip();os.close(rd)
  connection=display.Display(screen);stack.callback(connection.close)
  wayland=PrivateWayland(connection,screen,runtime,out);stack.callback(wayland.close)
  class Handler(http.server.BaseHTTPRequestHandler):
   def do_GET(self):
    body=Path(__file__).with_suffix('.html').read_bytes();self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(body)
   def log_message(self,*_):pass
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_') and k not in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']}
  env.update(HOME=str(runtime),DISPLAY=screen,WAYLAND_DISPLAY=wayland.socket,XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME=str(runtime/'config'),XDG_CACHE_HOME=str(runtime/'cache'),XDG_DATA_HOME=str(runtime/'data'),XDG_STATE_HOME=str(runtime/'state'),XR_INPUT_PROFILE=str(runtime/'profile'),LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn')
  log=stack.enter_context((out/'embedder.log').open('w'))
  child=subprocess.Popen([str(a.binary.resolve()),f'http://127.0.0.1:{server.server_port}/'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1,env=env);stack.callback(stop,child)
  def read():
   for line in child.stdout:
    messages.append(line)
    if line.startswith('XR_INPUT '):replies.put(json.loads(line[len('XR_INPUT '):]))
  threading.Thread(target=read,daemon=True).start()
  wait(lambda s:s['ready'] and s['frames']>4,'inline sessions ready')
  check(True,'Two inline sessions start with painted native hit-testable canvases')
  for kind in ['touch','pen']:
   start=mark();touch('down',kind=kind)
   s=wait(lambda s:len(named('selectstart',start))==1)
   source=named('selectstart',start)[0]['source'];ident=source['id']
   check(source['mode']=='screen' and source['handedness']=='none' and all(source[k] for k in ['grip','gamepad','hand']),kind+': transient screen source has no tracked hardware capabilities')
   check(ray_close(named('selectstart',start)[0]['ray'],[0,0,-1]),kind+': center contact has a forward ray')
   check(s['lengths']==[1,1],kind+': native contact is available in both inline sessions')
   touch('move',kind=kind,x=280)
   s=wait(lambda s:any(r['id']==ident and ray_close(r['ray'],[.75,0,-1]) for r in s['latest'][0]['rays']))
   check(True,kind+': held contact updates its perspective ray in animation frames')
   touch('up',kind=kind,compat=True);zero()
   names=[e['name'] for e in events(start) if e['name'] in ['pointerdown','selectstart','select','click','selectend','pointerup','sources']]
   check(names==['pointerdown','sources','selectstart','select','click','selectend','sources','pointerup'],kind+': selection brackets compatibility click and retires before pointerup')
   complete=named('select',start)[0];end=named('selectend',start)[0]
   check(complete['frame']==end['frame'] and complete['time']==end['time'],kind+': completion shares one action frame and time')
   check(all(e.get('trusted',True) for e in events(start)),kind+': native DOM and XR events are trusted')
  start=mark();touch('down',20);touch('down',21,x=280)
  check(snap()['lengths']==[2,2],'Multitouch exposes simultaneous transient sources')
  check(len(named('selectstart',start))==1,'Secondary touch is auxiliary and emits no selectstart')
  touch('up',20);touch('move',21,x=260);touch('up',21);zero()
  check(len(named('select',start))==1 and len(named('selectend',start))==1,'Secondary contact is not promoted into an XR primary action')
  pointer=named('pointerup',start)
  check([e['primary'] for e in pointer]==[True,False],'DOM primary identity remains fixed after the primary finger lifts')
  removals=[e for e in named('sources',start) if e['removed']]
  check(len(removals)==2 and len({e['removed'][0]['id'] for e in removals})==2,'Both multitouch sources retire independently')
  start=mark();touch('down',22);touch('down',23,'pen');touch('up',22);touch('up',23,'pen');zero()
  check(len(named('selectstart',start))==2 and len(named('select',start))==2,'Touch and pen each have a primary pointer of their own type')
  for kind in ['touch','pen']:
   start=mark();touch('down',30,kind);touch('cancel',30,kind);zero()
   check(len(named('selectend',start))==1 and not named('select',start),kind+': native cancellation ends without selection')
   names=[e['name'] for e in events(start) if e['name'] in ['selectend','pointercancel','sources']]
   check(names[-3:]==['selectend','sources','pointercancel'],kind+': source retires before pointercancel')
  command('prevent');start=mark();touch('down',40);touch('up',40,compat=True);zero()
  check(len(named('select',start))==1 and not named('click',start),'Prevented touch defaults suppress compatibility click without suppressing XR selection')
  command('allow');start=mark();touch('down',47);touch('move',47,x=600,y=500);zero();touch('up',47,x=600,y=500)
  check(len(named('selectend',start))==1 and not named('select',start),'Crossing into another document cancels the original inline contact')
  command('capture');start=mark();touch('down',41);touch('move',41,x=390);touch('up',41,x=390);zero()
  check(not named('select',start) and len(named('selectend',start))==1,'Moving outside the output canvas cancels XR even with pointer capture')
  check(len(named('gotpointercapture',start))==1 and len(named('lostpointercapture',start))==1,'Pointer capture is acquired and released around the canceled contact')
  start=mark();touch('down',42);touch('up',42,x=-100,y=-100);zero()
  check(len(named('selectend',start))==1 and not named('select',start),'A release with no native hit test cancels and retires the source')
  check(len(named('lostpointercapture',start))==1,'A release with no native hit test releases native pointer capture')
  command('release-capture')
  start=mark();touch('down',43);touch('move',43,x=-100,y=-100);touch('up',43);zero()
  check(len(named('selectend',start))==1 and not named('select',start),'No-hit movement cancels without recreating input on release')
  start=mark();command('synthetic')
  check(not named('sources',start) and not named('selectstart',start),'Script-dispatched touch-like pointer events do not create XR sources')
  start=mark();touch('down',44);command('swap');wait(lambda s:s['lengths']==[0,1]);touch('up',44);zero()
  check(not named('select',start,0) and len(named('select',start,1))==1,'Changing one session output canvas cancels only that session contact')
  command('restore');start=mark();touch('down',45);touch('down',46,'pen');ask('hide');zero()
  check(not named('select',start) and len(named('selectend',start))==2,'Hiding the webview cancels both touch and pen primary actions')
  check(all(e['error']=='SecurityError' for e in named('selectend',start)),'Hidden cancellation does not expose XR input poses')
  ask('show');wait(lambda s:s['lengths']==[0,0]);touch('cancel',45);touch('cancel',46,'pen')
  before=snap()['frames'];wait(lambda s:s['frames']>before+4);start=mark()
  thread_samples.append({'stage':'before-32-gestures','threads':len(list(Path('/proc',str(child.pid),'task').iterdir()))})
  for i in range(32):touch('down',100+i,kind='pen' if i%2 else 'touch');touch('up',100+i,kind='pen' if i%2 else 'touch')
  zero()
  thread_samples.append({'stage':'after-32-gestures','threads':len(list(Path('/proc',str(child.pid),'task').iterdir()))})
  check(len(named('selectstart',start))==32 and len(named('select',start))==32 and len(named('selectend',start))==32,'32 alternating touch/pen gestures complete without retained sources')
  check(len({e['source']['id'] for e in named('selectstart',start)})==32,'Each repeated gesture receives a fresh transient source')
  command('capture');start=mark();touch('down',200);touch('down',201);command('remove');zero();touch('cancel',200);touch('cancel',201)
  check(not named('select',start) and len(named('selectend',start))==1,'Removing a canvas cancels primary and auxiliary contacts without selecting')
  check(len(named('lostpointercapture',start))==2,'Disconnected canvas capture is released for both primary and auxiliary contacts')
  # Reentrant end must run against a connected output canvas.
  evaluate('(document.body.appendChild(document.getElementById("other")), true)')
  evaluate('(window.test.command("swap"), true)');wait(lambda s:s['frames']>5)
  command('end-on-start');start=mark();touch('down',202,x=595);wait(lambda s:any(e['name']=='ended' for e in s['events'][start:]));touch('up',202,x=595)
  check(len(named('selectend',start))==1 and not named('select',start),'Reentrant repeated session end cancels the contact exactly once')
  start=mark();touch('down',203,x=595);touch('up',203,x=595,compat=True);zero()
  ordinary=[e['name'] for e in events(start) if e['name'] in ['pointerdown','pointerup','click','sources','selectstart','select','selectend']]
  check(ordinary==['pointerdown','pointerup','click'],'Ordinary native contact retains pointerup-before-click ordering without an active XR output')
  ask('quit');child.wait(timeout=15);check(child.returncode==0,'Native touch/pen embedder shuts down cleanly')
  (out/'report.json').write_text(json.dumps({'passed':True,'checks':checks,'count':len(checks),'events':last,'exit':child.returncode,'physical_device':False,'thread_samples':thread_samples,'binary':str(binary),'binary_sha256':binary_sha256},indent=2)+'\n')
except BaseException:
 (out/'report.json').write_text(json.dumps({'passed':False,'checks':checks,'count':len(checks),'events':last,'error':traceback.format_exc()},indent=2)+'\n')
 raise
finally:(out/'stdout.log').write_text(''.join(messages))
print(json.dumps({'passed':True,'checks':len(checks),'report':str(out/'report.json')}))
