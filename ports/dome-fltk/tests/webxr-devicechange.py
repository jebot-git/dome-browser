#!/usr/bin/env python3
"""Exercise WebXR availability events, observer lifetimes and document policy."""
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
 deadline=time.monotonic()+60
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
  env.update(XR_INPUT_MOCK_XR='1',HOME=str(runtime),DISPLAY=screen,WAYLAND_DISPLAY=wayland.socket,XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME=str(runtime/'config'),XDG_CACHE_HOME=str(runtime/'cache'),XDG_DATA_HOME=str(runtime/'data'),XDG_STATE_HOME=str(runtime/'state'),XR_INPUT_PROFILE=str(runtime/'profile'),LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn')
  log=stack.enter_context((out/'embedder.log').open('w'))
  child=subprocess.Popen([str(a.binary.resolve()),f'http://127.0.0.1:{server.server_port}/'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1,env=env);stack.callback(stop,child)
  def read():
   for line in child.stdout:
    messages.append(line)
    if line.startswith('XR_INPUT '):replies.put(json.loads(line[len('XR_INPUT '):]))
  threading.Thread(target=read,daemon=True).start()
  wait(lambda s:s['finished'],'device-change checks finished')
  check(True,'Test embedder completes device-change lifecycle checks')
  result=snap();checks.extend(result['checks'])
  ask('quit');child.wait(timeout=15);check(child.returncode==0,'Device-change embedder shuts down cleanly')
  (out/'report.json').write_text(json.dumps({'passed':True,'checks':checks,'count':len(checks),'events':last,'exit':child.returncode,'physical_device':False,'binary':str(binary),'binary_sha256':binary_sha256},indent=2)+'\n')
except BaseException:
 (out/'report.json').write_text(json.dumps({'passed':False,'checks':checks,'count':len(checks),'events':last,'error':traceback.format_exc()},indent=2)+'\n')
 raise
finally:(out/'stdout.log').write_text(''.join(messages))
print(json.dumps({'passed':True,'checks':len(checks),'report':str(out/'report.json')}))
