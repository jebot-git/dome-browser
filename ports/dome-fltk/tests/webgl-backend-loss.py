#!/usr/bin/env python3
"""Exercise controlled backend loss in the separate software test embedder."""
import argparse,contextlib,hashlib,http.server,json,os,resource,signal,subprocess,tempfile,threading,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);page=Path(__file__).with_suffix('.html').read_bytes();(out/'fixture.html').write_bytes(page)
reports=[];checkpoints=[];failure=None;record={};child=None
class Server(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_GET(self):
  self.send_response(200);self.send_header('Content-Type','text/html;charset=utf-8');self.end_headers();self.wfile.write(page)
 def do_POST(self):
  value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
  (reports if self.path=='/report' else checkpoints).append(value)
  self.send_response(204);self.end_headers()
def stop(proc):
 if proc.poll() is None:
  proc.terminate()
  try:proc.wait(timeout=8)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()
try:
 with contextlib.ExitStack() as stack:
  root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-backend-loss-')));root.chmod(0o700)
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')};env.update(HOME=str(root),XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),XR_INPUT_PROFILE=str(root/'profile'),DOME_GRAPHICS_LOSS_TEST='1',LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn')
  for name in ['DISPLAY','WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(name,None)
  log=stack.enter_context((out/'embedder.log').open('w'));child=subprocess.Popen([str(a.binary.resolve()),f'http://127.0.0.1:{server.server_port}/'],stdin=subprocess.PIPE,stdout=log,stderr=log,text=True,env=env);stack.callback(stop,child)
  deadline=time.monotonic()+140
  while not reports and child.poll() is None and time.monotonic()<deadline:time.sleep(.03)
  assert reports,'No completed report; process exit '+str(child.poll())+'; last checkpoint '+str(checkpoints[-1:] )
  record=reports[-1];assert record['checks'] and all(row['passed'] for row in record['checks']),'Backend lifecycle assertion failed'
  assert len(record['cases'])==64,'Missing synchronous reply cases: '+str(len(record['cases']))
  child.stdin.write(json.dumps({'op':'quit','token':1})+'\n');child.stdin.flush();record['exit_code']=child.wait(timeout=15);assert record['exit_code']==0,'Unclean embedder exit'
except BaseException as error:failure=str(error);raise
finally:
 record.update(passed=failure is None,failure=failure,checkpoints=checkpoints,binary_sha256=hashlib.sha256(a.binary.read_bytes()).hexdigest(),fixture_sha256=hashlib.sha256(page).hexdigest(),script_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),hardware_reset_requested=False,physical_device_validation=False,full_browser_gpu_recovery=False)
 (out/'report.json').write_text(json.dumps(record,indent=2)+'\n')
 print(json.dumps({key:record[key]for key in ['passed','failure','binary_sha256','full_browser_gpu_recovery']},indent=2))
