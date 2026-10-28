#!/usr/bin/env python3
"""Exercise ordinary failed/cancelled HTTP response bodies in private Wayland."""
import argparse,contextlib,hashlib,http.server,json,os,resource,signal,socket,subprocess,sys,tempfile,threading,time
from pathlib import Path
from urllib.parse import urlsplit
from Xlib import display
from private_wayland import PrivateWayland

signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_FETCH_STREAM_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_FETCH_STREAM_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);page=Path(__file__).with_suffix('.html').read_bytes();(out/'fixture.html').write_bytes(page)
reports=[];hits=[];child=None;failure=None;record={}
def sha(data):return hashlib.sha256(data).hexdigest()
def stop(process):
 if process.poll() is None:
  process.terminate()
  try:process.wait(timeout=8)
  except subprocess.TimeoutExpired:process.kill();process.wait()
class Server(http.server.BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def do_GET(self):
  path=urlsplit(self.path).path;hits.append(path)
  if path=='/unavailable':
   self.connection.shutdown(socket.SHUT_RDWR);self.connection.close();return
  body=b'ordinary content' if path=='/ok' else b'abcd' if path in ['/truncated','/slow'] else page
  self.send_response(200);self.send_header('Content-Type','text/html;charset=utf-8');self.send_header('Content-Length',str(len(body)));self.send_header('Cache-Control','no-store');self.end_headers()
  try:
   if path=='/truncated':self.wfile.write(body[:1]);self.wfile.flush()
   elif path=='/slow':self.wfile.write(body[:1]);self.wfile.flush();time.sleep(.1);self.wfile.write(body[1:])
   else:self.wfile.write(body)
  except(BrokenPipeError,ConnectionResetError):pass
 def do_POST(self):
  if self.path=='/report':reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
  self.send_response(204);self.end_headers()
try:
 with contextlib.ExitStack()as stack:
  root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-fetch-stream-')));root.chmod(0o700)
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1400x1000x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd)
  connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,root,out);stack.callback(wayland.close)
  env={k:v for k,v in os.environ.items()if not k.startswith('DOME_')};env.update(HOME=str(root),XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket,LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn',RUST_BACKTRACE='1')
  for name in ['DISPLAY','WAYLAND_SOCKET']:env.pop(name,None)
  log_path=out/'browser.log';log=stack.enter_context(log_path.open('w'));child=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',str(root/'profile'),f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log);stack.callback(stop,child)
  deadline=time.monotonic()+30
  while not reports and child.poll()is None and time.monotonic()<deadline:
   assert 'panicked at'not in log_path.read_text(),'Script thread assertion during native fetch lifecycle'
   time.sleep(.03)
  assert reports,'No completed fetch lifecycle report; browser exit '+str(child.poll())
  record=reports[-1];assert record['checks']and all(c['passed']for c in record['checks']),'Fetch lifecycle assertion failed'
  assert all(hits.count(path)>=count for path,count in [('/unavailable',8),('/truncated',4),('/slow',4)]),'Missing response lifecycle coverage'
  windows=[w for w in wayland.windows()if w.value()['client_pid']==child.pid];assert windows,'Missing live browser window'
  for window in windows:wayland.api('/api/action',{'op':'close','id':window.id})
  record['exit_code']=child.wait(timeout=10);assert child.returncode==0,'Unclean browser close'
except BaseException as error:failure=str(error);raise
finally:
 record.update(passed=failure is None,failure=failure,binary_sha256=sha(a.binary.read_bytes()),fixture_sha256=sha(page),script_sha256=sha(Path(__file__).read_bytes()),checks=record.get('checks',[]),server_hits={path:hits.count(path)for path in sorted(set(hits))},scope='Private profile and loopback HTTP lifecycle cases; native Wayland and software rendering')
 (out/'report.json').write_text(json.dumps(record,indent=2)+'\n');print(json.dumps({k:record[k]for k in ['passed','failure','binary_sha256']},indent=2))
