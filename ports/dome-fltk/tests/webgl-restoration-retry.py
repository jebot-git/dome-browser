#!/usr/bin/env python3
"""Exercise delayed WebGL recreation through existing native test preferences."""
import argparse,contextlib,hashlib,http.server,json,os,queue,resource,signal,subprocess,sys,tempfile,threading,time
from pathlib import Path
from Xlib import display
from private_wayland import PrivateWayland
signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_RESTORE_TEST_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_RESTORE_TEST_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);page=Path(__file__).with_suffix('.html').read_bytes();checks=[];child=None;failure=None;replies=queue.Queue();token=0;log_path=out/'embedder.log'
def check(ok,name):
 checks.append({'name':name,'passed':bool(ok)});assert ok,name
def stop(proc):
 if proc.poll()is None:
  proc.terminate()
  try:proc.wait(timeout=8)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()
def ask(op,**values):
 global token
 token+=1;wanted=token;child.stdin.write(json.dumps({'op':op,'token':wanted,**values})+'\n');child.stdin.flush();end=time.monotonic()+10
 while time.monotonic()<end:
  row=replies.get(timeout=max(.01,end-time.monotonic()))
  if row.get('token')==wanted:
   assert 'error'not in row,row;return row
 raise AssertionError('Native test command timed out')
def evaluate(script):return json.loads(ask('eval',script='JSON.stringify('+script+')')['value'])
def snapshot():return evaluate('snapshot()')
def wait(predicate):
 end=time.monotonic()+10
 while time.monotonic()<end:
  value=snapshot()
  if predicate(value):return value
  time.sleep(.03)
 raise AssertionError('Restoration state timed out: '+json.dumps(value))
class Server(http.server.BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def do_GET(self):
  self.send_response(200);self.send_header('Content-Type','text/html;charset=utf-8');self.send_header('Content-Length',str(len(page)));self.end_headers();self.wfile.write(page)
try:
 with contextlib.ExitStack()as stack:
  root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-restore-retry-')));root.chmod(0o700)
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1200x900x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd)
  connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,root,out);stack.callback(wayland.close)
  env={k:v for k,v in os.environ.items()if not k.startswith('DOME_')and k not in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']};env.update(HOME=str(root),DISPLAY=screen,WAYLAND_DISPLAY=wayland.socket,XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_CACHE_HOME=str(root/'cache'),XDG_DATA_HOME=str(root/'data'),XR_INPUT_PROFILE=str(root/'profile'),DOME_GRAPHICS_LOSS_TEST='1',LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn,script=debug')
  log=stack.enter_context(log_path.open('w'));child=subprocess.Popen([str(a.binary.resolve()),f'http://127.0.0.1:{server.server_port}/'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1,env=env);stack.callback(stop,child)
  def read():
   for line in child.stdout:
    if line.startswith('XR_INPUT '):replies.put(json.loads(line[len('XR_INPUT '):]))
  threading.Thread(target=read,daemon=True).start()
  # Wait for the document without depending on wall-clock load timing.
  end=time.monotonic()+15
  ready=False
  while time.monotonic()<end:
   assert child.poll()is None,'Test embedder exited during startup'
   try:ready=evaluate('typeof setup')=='function'
   except AssertionError as error:
    if 'Err(InternalError)'not in str(error):raise
   if ready:break
   time.sleep(.03)
  check(ready,'Target document becomes ready before graphics commands')
  for case,(kind,automatic) in enumerate([('webgl',False),('webgl2',True),('webgl',False)]):
   label=kind+(' automatic'if automatic else' explicit');initial=evaluate('setup('+json.dumps(kind)+','+json.dumps(automatic)+')');check(initial.get('available')and initial.get('extension')and initial.get('originalBuffer'),label+' initial resources exist')
   before=log_path.read_text().count('Creation rejected by existing WebGL test preference')
   ask('webgl-test',creation_error=True,context_loss=automatic);evaluate('begin()');wait(lambda s:s['lost']==1)
   ask('webgl-test',creation_error=True,context_loss=False);time.sleep(.8)
   state=snapshot();attempts=log_path.read_text().count('Creation rejected by existing WebGL test preference')-before
   check(child.poll()is None and state['isLost']and state['restored']==0,label+' stays lost and responsive while recreation is unavailable')
   check(2<=attempts<=8,label+' retries are delayed and bounded ('+str(attempts)+')')
   if case==2:break
   ask('webgl-test',creation_error=False,context_loss=False);state=wait(lambda s:s['restored']==1)
   check(state['lost']==1 and not state['isLost']and state['error']is None,label+' restores exactly once without another application request')
   check(state.get('oldBufferInvalid')and state.get('pixel')==[0,255,0,255]and state.get('glError')==0,label+' creates a clean resource generation and renders pixels')
   # New canvas ensures WebGL1/2 each use a fresh context type.
   evaluate('(document.querySelector("canvas").replaceWith(Object.assign(document.createElement("canvas"),{width:64,height:64})),true)')
  ask('quit');check(child.wait(timeout=10)==0,'Shutdown completes with a restoration timer pending')
except BaseException as error:failure=str(error);raise
finally:
 record={'passed':failure is None,'failure':failure,'checks':checks,'binary_sha256':hashlib.sha256(a.binary.read_bytes()).hexdigest(),'fixture_sha256':hashlib.sha256(page).hexdigest(),'script_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'scope':'Private native test embedder; existing testing preferences simulate unavailable creation and backend loss. No driver reset requested.'};(out/'report.json').write_text(json.dumps(record,indent=2)+'\n')
