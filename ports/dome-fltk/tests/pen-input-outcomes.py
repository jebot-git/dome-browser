#!/usr/bin/env python3
"""Exercise ordered native pen completion, internal retries and bounded overflow."""
import argparse,http.server,json,os,queue,resource,signal,subprocess,sys,tempfile,threading,time
from pathlib import Path
from urllib.parse import quote
from wayland_touch_fixture import TouchWayland
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_PEN_OUTCOME_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_PEN_OUTCOME_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--compositor',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);checks=[];messages=[];snapshots=[];replies=queue.Queue();token=0;child=fixture=server=log=None;failure=None
def check(v,label):
 checks.append({'name':label,'passed':bool(v)});print(('PASS ' if v else 'FAIL ')+label,flush=True);assert v,label
def wait(fn):
 end=time.monotonic()+20
 while time.monotonic()<end:
  assert child.poll() is None,'Embedder exited unexpectedly'
  v=fn()
  if v:return v
  time.sleep(.025)
 raise AssertionError('Pen outcome fixture timed out')
def receive():
 end=time.monotonic()+20
 while time.monotonic()<end:
  assert child.poll() is None,'Embedder exited unexpectedly'
  try:return replies.get(timeout=.1)
  except queue.Empty:pass
 raise AssertionError('Missing native input completion')
def ask(op,**kwargs):
 global token
 token+=1;child.stdin.write(json.dumps(dict(op=op,token=token,**kwargs))+'\n');child.stdin.flush();r=receive();assert r['token']==token and 'error' not in r,r;return r
def evaluate(expression):return json.loads(ask('eval',script='JSON.stringify('+expression+')')['value'])
def latest():return max(snapshots,key=lambda s:s['frames']) if snapshots else None
def settle():
 s=latest();f=s['frames'];c=s['child']['frames'];wait(lambda:latest()['frames']>f+4 and latest()['child']['frames']>c+4)
def batch(samples):
 global token
 tokens=[]
 for sample in samples:token+=1;sample['token']=token;tokens.append(token)
 ask('pen-batch',samples=samples);results=[receive() for _ in tokens]
 assert sorted(r['token'] for r in results)==tokens,results
 settle();assert replies.empty(),'Internal or duplicate completion leaked to embedder';return results
def sample(x=80,y=100,buttons=0,phase='sample'):return dict(x=x,y=y,buttons=buttons,phase=phase)
try:
 with tempfile.TemporaryDirectory(prefix='dome-pen-outcomes-') as folder:
  runtime=Path(folder);runtime.chmod(0o700);fixture=TouchWayland(a.compositor.resolve(),runtime,out)
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*_):pass
   def do_GET(self):
    body=b'[]' if self.path=='/commands' else Path(__file__).with_name('native-pen-frames.html').read_bytes();self.send_response(200);self.send_header('Content-Type','application/json' if self.path=='/commands' else 'text/html');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
   def do_POST(self):
    value=json.loads(self.rfile.read(int(self.headers['Content-Length'])));
    if self.path=='/snapshot':snapshots.append(value)
    self.send_response(204);self.end_headers()
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=server.serve_forever,daemon=True).start();base=f'http://127.0.0.1:{server.server_port}/'
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')};env.update(HOME=folder,XDG_RUNTIME_DIR=folder,XDG_CONFIG_HOME=folder+'/config',XDG_DATA_HOME=folder+'/data',XDG_STATE_HOME=folder+'/state',XDG_CACHE_HOME=folder+'/cache',WAYLAND_DISPLAY=fixture.wayland.socket,DISPLAY=fixture.screen,XR_INPUT_PROFILE=folder+'/profile',LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn');env.pop('WAYLAND_SOCKET',None)
  log=(out/'embedder.log').open('w');child=subprocess.Popen([str(a.binary.resolve()),base+'?frame='+quote(base+'?child=1',safe='')],env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1)
  def read():
   for line in child.stdout:
    messages.append(line)
    if line.startswith('XR_INPUT '):replies.put(json.loads(line[len('XR_INPUT '):]))
  threading.Thread(target=read,daemon=True).start();wait(lambda:latest() and latest()['child']['frames']>4);settle()
  evaluate('(release=true)');batch([sample(),sample(buttons=1)]);evaluate('area.setPointerCapture(lastId) ?? true');start=len(latest()['child']['events']);results=batch([sample(444,144,1),sample(444,144)])
  check(all(not r['failed'] for r in results),'Captured cross-frame samples each complete once')
  ce=latest()['child']['events'][start:];check(any(e['name']=='pointermove' and e['buttons']==0 for e in ce) and not any(e['name'] in ['pointerdown','pointerup','click'] for e in ce),'Internal release hover reaches the actual frame without a public completion')
  batch([sample(phase='leave')]);evaluate('(release=true)');start=len(latest()['child']['events']);results=batch([sample(),sample(buttons=1),sample(444,144,1),sample(444,144),sample(phase='leave')])
  check(all(not r['failed'] for r in results),'Cross-document rejection and retry retain one completion per external sample')
  ce=latest()['child']['events'][start:];check(any(e['name']=='pointermove' and e['buttons']==1 for e in ce) and not any(e['name'] in ['pointerdown','click'] for e in ce),'Queued held motion does not create a new down or click in the next document')
  evaluate('(release=false)');batch([sample(),sample(buttons=1)]);start=len(latest()['events']);results=batch([sample(81+i%30,100,1) for i in range(400)])
  check(len(results)==400 and any(r['failed'] for r in results),'Overflow reports every queued or rejected input once')
  events=latest()['events'][start:];check(sum(e['name']=='pointercancel' for e in events)==1 and not any(e['name'] in ['pointerup','click'] for e in events),'Overflow cancels the active gesture once without false activation')
  start=len(latest()['events']);results=batch([sample(),sample(buttons=1),sample(),sample(phase='leave')]);events=latest()['events'][start:]
  check(all(not r['failed'] for r in results) and sum(e['name']=='click' for e in events)==1,'A released tool can start a normal gesture after overflow')
  batch([sample(444,144),sample(444,144,1)]);evaluate('document.querySelector("iframe").remove() ?? true')
  # The child no longer produces frames; wait for parent painting rather than settle().
  f=latest()['frames'];wait(lambda:latest()['frames']>f+5)
  token+=1;t=token;ask('pen-batch',samples=[dict(sample(90,100,1),token=t)]);r=receive();check(r['token']==t and r['failed'],'Removing the owner frame completes its next native sample with dispatch failure')
  ask('quit');child.wait(timeout=10);check(child.returncode==0,'The embedder exits cleanly after all native completion checks')
except BaseException as ex:failure=str(ex);raise
finally:
 if child and child.poll() is None:child.terminate();child.wait(timeout=8)
 if server:server.shutdown();server.server_close()
 if log:log.close()
 if fixture:fixture.close()
 (out/'messages.log').write_text(''.join(messages));(out/'snapshots.json').write_text(json.dumps(snapshots));(out/'report.json').write_text(json.dumps({'checks':checks,'passed':failure is None and bool(checks),'failure':failure,'scope':'Separate uninstalled embedder uses the production input pipeline; ordered burst and frame-lifecycle tests'},indent=2))
