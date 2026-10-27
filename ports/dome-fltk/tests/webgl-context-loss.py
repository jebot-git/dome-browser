#!/usr/bin/env python3
"""Exercise native WebGL context loss, restoration and upstream Khronos pages."""
from pathlib import Path
import argparse,contextlib,hashlib,http.server,json,mimetypes,os,resource,signal,subprocess,sys,tempfile,threading,time
from urllib.parse import urlsplit
from Xlib import X,display
from PIL import Image
from private_wayland import PrivateWayland
signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_GL_LIFECYCLE_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_GL_LIFECYCLE_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
here=Path(__file__).resolve().parent;source=here.parents[2];out=a.output.resolve();out.mkdir(parents=True,mode=0o700,exist_ok=False)
reports=[];hashes={};child=None;failure=None;record={};page=Path(__file__).with_suffix('.html').read_bytes();(out/'fixture.html').write_bytes(page)
def sha(data):return hashlib.sha256(data).hexdigest()
def stop(proc):
 if proc.poll() is None:
  proc.terminate()
  try:proc.wait(timeout=8)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()
class Server(http.server.BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def do_GET(self):
  path=urlsplit(self.path).path
  if path=='/':body=page;kind='text/html'
  else:
   root=source/'tests/wpt/webgl/tests' if path.startswith('/webgl/') else source/'tests/wpt/tests'
   relative=path.removeprefix('/webgl/') if path.startswith('/webgl/') else path.lstrip('/')
   target=(root/relative).resolve()
   if not target.is_relative_to(root.resolve()) or not target.is_file():self.send_error(404);return
   body=target.read_bytes();hashes[str(target.relative_to(source))]=sha(body);kind=mimetypes.guess_type(str(target))[0] or 'application/octet-stream'
   if target.name in ['context-lost.html','context-lost-restored.html']:
    tag=b'<script src=/resources/testharness.js></script>';assert body.count(tag)==1
    reporter=b'''<script>add_completion_callback((tests,status)=>parent.postMessage({tag:new URL(location.href).searchParams.get('fixtureTag'),status:status.status,contextLost:typeof contextLostEventFired==='boolean'?contextLostEventFired:gl.isContextLost(),restored:typeof contextRestoredEventFired==='boolean'?contextRestoredEventFired:false,results:tests.map(t=>({name:t.name,status:t.status,message:t.message}))},location.origin));</script>'''
    body=body.replace(tag,tag+reporter);hashes['served:'+str(target.relative_to(source))]=sha(body)
  self.send_response(200);self.send_header('Content-Type',kind+';charset=utf-8');self.send_header('Content-Length',str(len(body)));self.end_headers()
  try:self.wfile.write(body)
  except (BrokenPipeError,ConnectionResetError):pass
 def do_POST(self):
  data=self.rfile.read(int(self.headers.get('Content-Length','0')))
  if self.path=='/report':reports.append(json.loads(data))
  self.send_response(204);self.end_headers()
try:
 with contextlib.ExitStack() as stack:
  root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-gl-lifecycle-')));root.chmod(0o700)
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1400x1000x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd)
  connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,root,out);stack.callback(wayland.close)
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')};env.update(HOME=str(root),XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket,LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn')
  for name in ['DISPLAY','WAYLAND_SOCKET']:env.pop(name,None)
  log=stack.enter_context((out/'browser.log').open('w'));child=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',str(root/'profile'),f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log);stack.callback(stop,child)
  deadline=time.monotonic()+100
  while not reports and child.poll() is None and time.monotonic()<deadline:time.sleep(.03)
  assert reports,'No completed fixture report; browser exit '+str(child.poll())
  record=reports[-1];windows=[w for w in wayland.windows() if w.value()['client_pid']==child.pid];assert windows,'Missing test browser window'
  if all(c['passed'] for c in record['checks']):
   window=windows[0];geometry=window.get_geometry();raw=window.get_image(0,0,geometry.width,geometry.height,X.ZPixmap,0xffffffff)
   pixels=Image.frombytes('RGB',(geometry.width,geometry.height),raw.data,'raw','BGRX');pixels.save(out/'restored-canvas.png')
   green=sum(g>240 and r<15 and b<15 for r,g,b in pixels.getdata())
   record['native_green_pixels']=green;record['checks'].append(dict(name='Native Wayland surface displays restored green GPU canvas pixels',passed=green>7000))
  for w in windows:wayland.api('/api/action',{'op':'close','id':w.id})
  record['exit_code']=child.wait(timeout=10);record['clean_close']=child.returncode==0
  checks=record['checks'];assert checks and all(c['passed'] for c in checks),'Lifecycle assertion failed'
  assert len(record['suites'])==4 and all(s['status']==0 and any(t['name']=='Overall test' for t in s['results']) and s['contextLost'] and s['restored']==s['tag'].startswith('context-lost-restored') and all(t['status']==0 for t in s['results']) for s in record['suites']),'Original Khronos assertions failed or missing'
  assert record['clean_close'],'Browser did not close cleanly'
except BaseException as error:failure=str(error);raise
finally:
 record.update(passed=failure is None,failure=failure,binary_sha256=sha(a.binary.read_bytes()),fixture_sha256=sha(page),source_sha256=hashes,upstream_assertions_changed=False,physical_device_validation=False,adapter_migration=False)
 (out/'report.json').write_text(json.dumps(record,indent=2)+'\n')
 for check in record.get('checks',[]):print(('PASS ' if check['passed'] else 'FAIL ')+check['name']+(' '+check['error'] if 'error' in check else ''),flush=True)
 for suite in record.get('suites',[]):print(suite['tag'],len(suite['results']),'cases;',sum(t['status']!=0 for t in suite['results']),'failed',flush=True)
