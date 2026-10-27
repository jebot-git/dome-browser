#!/usr/bin/env python3
"""Exercise document and iframe Permissions Policy through the native mock embedder."""
from urllib.parse import urlsplit,parse_qs
import argparse,contextlib,hashlib,http.server,json,math,os,queue,resource,signal,subprocess,sys,threading,time,tempfile,traceback
signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get("DOME_TOUCH_TEST_BUS")!="1":
 os.execvpe("dbus-run-session",["dbus-run-session","--",sys.executable,*sys.argv],{**os.environ,"DOME_TOUCH_TEST_BUS":"1"})
from pathlib import Path
from Xlib import display
from private_wayland import PrivateWayland

p=argparse.ArgumentParser(description=__doc__);p.add_argument('binary',type=Path);p.add_argument('output',type=Path);p.add_argument('--history-eviction',action='store_true',help='Force discarded child-history reloads in the separate test embedder');p.add_argument('--upstream',action='store_true',help='Run the original WPT policy page, recording its outdated compatibility expectation separately');a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
binary=a.binary.resolve();binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest()
checks=[];child=None;token=0;replies=queue.Queue();messages=[];last=None;thread_samples=[];frame_requests=[]

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
 deadline=time.monotonic()+150
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
    path=urlsplit(self.path);query=parse_qs(path.query);kind='text/html';headers={'Cache-Control':'no-store'};code=200
    if path.path=='/frame':frame_requests.append(self.path)
    if a.upstream and path.path.startswith(('/webxr/','/resources/')):
     wpt=Path(__file__).resolve().parents[3]/'tests/wpt/tests';resource_path=(wpt/path.path.lstrip('/')).resolve();assert resource_path.is_relative_to(wpt)
     body=resource_path.read_bytes();kind='text/javascript'if resource_path.suffix=='.js'else'text/html'
     if path.path=='/webxr/webxr_permissions_policy.https.html':
      reporter=b'<script>add_completion_callback((tests,status)=>parent.postMessage({policyUpstream:{status:status.status,message:status.message,stack:status.stack,results:tests.map(t=>({name:t.name,status:t.status,message:t.message}))}},"*"));</script>'
      body=body.replace(b'<script src="/resources/testharnessreport.js"></script>',reporter);headers['Permissions-Policy']='xr-spatial-tracking=()'
    elif a.upstream:
     body=b'''<!doctype html><script>const state=window.test={error:null,finished:false,checks:[],suites:[]};state.snapshot=()=>({...state,snapshot:undefined});addEventListener('message',event=>{if(event.source===document.querySelector('iframe').contentWindow&&event.data.policyUpstream){state.suites.push(event.data.policyUpstream);state.finished=true}});</script><iframe src="/webxr/webxr_permissions_policy.https.html"></iframe>'''
    elif path.path=='/config.json':body=json.dumps({'crossOrigin':f'http://127.0.0.1:{foreign.server_port}','historyEviction':a.history_eviction}).encode();kind='application/json'
    elif path.path=='/history-requests':body=json.dumps(frame_requests).encode();kind='application/json'
    elif path.path=='/probe.js':body=Path(__file__).with_name('webxr-permissions-policy-probe.js').read_bytes();kind='text/javascript';headers['Access-Control-Allow-Origin']='*'
    elif path.path=='/frame':
     if 'redirect'in query:code=302;headers['Location']=query['redirect'][0];body=b''
     elif 'selfnav'in query:
      target='/frame?'+path.query.replace('selfnav=1','selfnav_done=1');body=('<script>location.replace('+json.dumps(target)+')</script>').encode()
     elif 'nested'in query:
      cfg=json.loads(query['nested'][0]);target=cfg['origin']+'/frame?policy='+__import__('urllib.parse',fromlist=['quote']).quote(cfg.get('header',''));body=('<script>addEventListener("message",e=>{if(e.source===document.querySelector("iframe").contentWindow)parent.postMessage(e.data,"*")})</script><iframe allow='+json.dumps(cfg.get('allow',''))+' src='+json.dumps(target)+'></iframe>').encode()
     else:
      gl='true'if query.get('gl')==['1']else'false';body=('<script src="/probe.js"></script><script>async function send(){parent.postMessage({policyProbe:await policyProbe('+gl+')},"*")}addEventListener("message",e=>{if(e.data.probe)send();if(e.data.navigate)location.reload();if(e.data.navigateTo)location.assign(e.data.navigateTo);if(e.data.historyBack)history.go(-e.data.historyBack)});send();</script>').encode()
     if 'policy'in query:headers['Permissions-Policy']=query['policy'][0]
    else:body=Path(__file__).with_suffix('.html').read_bytes()
    self.send_response(code);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(body)))
    for key,value in headers.items():self.send_header(key,value)
    self.end_headers();self.wfile.write(body)
   def log_message(self,*_):pass
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);foreign=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
  for site in [server,foreign]:threading.Thread(target=site.serve_forever,daemon=True).start();stack.callback(site.server_close);stack.callback(site.shutdown)
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_') and k not in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']}
  env.update(XR_INPUT_MOCK_XR='1',XR_INPUT_POLICY_TEST='1',HOME=str(runtime),DISPLAY=screen,WAYLAND_DISPLAY=wayland.socket,XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME=str(runtime/'config'),XDG_CACHE_HOME=str(runtime/'cache'),XDG_DATA_HOME=str(runtime/'data'),XDG_STATE_HOME=str(runtime/'state'),XR_INPUT_PROFILE=str(runtime/'profile'),LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn')
  log=stack.enter_context((out/'embedder.log').open('w'))
  child=subprocess.Popen([str(a.binary.resolve()),f'http://127.0.0.1:{server.server_port}/'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1,env=env);stack.callback(stop,child)
  def read():
   for line in child.stdout:
    messages.append(line)
    if line.startswith('XR_INPUT '):replies.put(json.loads(line[len('XR_INPUT '):]))
  threading.Thread(target=read,daemon=True).start()
  wait(lambda s:s['finished'],'document policy checks finished')
  check(True,'Test embedder completes document policy fixture')
  result=snap();checks.extend(result['checks'])
  upstream_delta=None
  if a.upstream:
   suites=result['suites'];assert len(suites)==1 and suites[0]['status']==0,suites
   tests=suites[0]['results'];assert len(tests)==4 and all(t['status']==0 for t in tests[:3]),tests
   delta=tests[3];assert delta['name']=='Validate xr compatibility requests without xr-spatial-tracking policy' and delta['status']==1 and delta['message']=='assert_unreached: Should have rejected: makeXRCompatible should reject without permissions policy Reached unreachable code',delta
   upstream_delta={'all_original_assertions_passed':False,'passed_cases':3,'obsolete_expectation_cases':1,'case':delta,'current_spec':'https://www.w3.org/TR/2026/CRD-webxr-20260609/#dom-webglrenderingcontextbase-makexrcompatible','current_spec_behavior_verified_by':'webxr-context-compatibility.py --policy-denied'}
  ask('quit');child.wait(timeout=15);check(child.returncode==0,'Policy test embedder shuts down cleanly')
  (out/'report.json').write_text(json.dumps({'passed':all(not isinstance(c,dict) or c['passed'] for c in checks),'checks':checks,'count':len(checks),'events':last,'exit':child.returncode,'physical_device':False,'upstream_spec_delta':upstream_delta,'binary':str(binary),'binary_sha256':binary_sha256},indent=2)+'\n')
  assert all(not isinstance(c,dict) or c['passed'] for c in checks), 'A policy behavior check failed; inspect report.json'
except BaseException:
 (out/'report.json').write_text(json.dumps({'passed':False,'checks':checks,'count':len(checks),'events':last,'binary_sha256':binary_sha256,'exit':None if child is None else child.poll(),'error':traceback.format_exc()},indent=2)+'\n')
 raise
finally:(out/'stdout.log').write_text(''.join(messages))
print(json.dumps({'passed':True,'checks':len(checks),'report':str(out/'report.json')}))
