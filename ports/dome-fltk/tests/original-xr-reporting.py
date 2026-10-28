#!/usr/bin/env python3
"""Run the two original WebXR permissions-policy reporting WPT cases locally."""
from pathlib import Path
import argparse,contextlib,hashlib,http.server,json,mimetypes,os,resource,signal,subprocess,sys,tempfile,threading,time
from urllib.parse import urlsplit
from Xlib import display
parser=argparse.ArgumentParser()
parser.add_argument('--binary',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
HERE=Path(__file__).resolve().parent
W=next((path for path in HERE.parents if (path/'components/script').is_dir()),Path.cwd())
assert (W/'ports/dome-fltk/tests/private_wayland.py').is_file(),'Run from the Servo checkout root'
sys.path.insert(0,str(W/'ports/dome-fltk/tests'))
from private_wayland import PrivateWayland
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_ORIGINAL_XR_REPORTING_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_ORIGINAL_XR_REPORTING_BUS':'1'})
T=W/'tests/wpt/tests';B=args.binary.resolve();out=args.output.resolve();out.mkdir(mode=0o700,parents=True,exist_ok=False)
reports={};hashes={};served_hashes={};runs=[]
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def stop(proc):
 if proc.poll()is None:
  proc.terminate()
  try:proc.wait(timeout=8)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()
class Handler(http.server.BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def do_GET(self):
  path=urlsplit(self.path).path;file=(T/path.lstrip('/')).resolve()
  if not file.is_relative_to(T.resolve())or not file.is_file():self.send_error(404);return
  body=file.read_bytes();hashes[str(file.relative_to(W))]=sha(file)
  if file.name in ['xr-reporting.https.html','xr-report-only.https.html']:
   needle=b"<script src='/resources/testharness.js'></script>";assert body.count(needle)==1
   callback=b'<script>add_completion_callback((tests,status)=>fetch("/__report",{method:"POST",body:JSON.stringify({url:location.pathname,harness:{status:status.status,message:status.message},tests:tests.map(t=>({name:t.name,status:t.status,message:t.message}))})}));</script>'
   body=body.replace(needle,needle+callback)
  served_hashes[path]=hashlib.sha256(body).hexdigest()
  self.send_response(200);self.send_header('Content-Type',mimetypes.guess_type(str(file))[0]or'application/octet-stream');self.send_header('Content-Length',str(len(body)))
  headers=Path(str(file)+'.headers')
  if headers.exists():
   hashes[str(headers.relative_to(W))]=sha(headers)
   for line in headers.read_text().splitlines():
    if line.strip():name,value=line.split(':',1);self.send_header(name,value.strip())
  self.end_headers();self.wfile.write(body)
 def do_POST(self):
  row=json.loads(self.rfile.read(int(self.headers['Content-Length'])));reports[row['url']]=row;self.send_response(204);self.end_headers()
with contextlib.ExitStack()as stack:
 root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-reporting-baseline-')));root.chmod(0o700)
 rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1200x900x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd)
 connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,root,out);stack.callback(wayland.close)
 server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
 env={k:v for k,v in os.environ.items()if not k.startswith('DOME_')and k not in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']};env.update(HOME=str(root),DISPLAY=screen,WAYLAND_DISPLAY=wayland.socket,XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_CACHE_HOME=str(root/'cache'),XDG_DATA_HOME=str(root/'data'),LIBGL_ALWAYS_SOFTWARE='1',FONTCONFIG_FILE='/etc/fonts/fonts.conf',RUST_LOG='warn')
 for index,name in enumerate(['xr-reporting','xr-report-only']):
  path='/permissions-policy/reporting/'+name+'.https.html';env['XR_INPUT_PROFILE']=str(root/f'profile-{index}');log=(out/(name+'.log')).open('w');stack.callback(log.close)
  proc=subprocess.Popen([str(B),f'http://127.0.0.1:{server.server_port}'+path],stdin=subprocess.PIPE,stdout=log,stderr=log,text=True,env=env);stack.callback(stop,proc)
  deadline=time.monotonic()+30
  while path not in reports and proc.poll()is None and time.monotonic()<deadline:time.sleep(.05)
  row=reports.get(path,{'url':path,'harness':{'status':2,'message':'No completion within 30 seconds'},'tests':[]})
  was_live=proc.poll()is None
  if was_live:proc.stdin.write('{"op":"quit","token":1}\n');proc.stdin.flush()
  try:proc.wait(timeout=10)
  except subprocess.TimeoutExpired:stop(proc)
  row.update(browser_live_at_completion=was_live,exit_code=proc.returncode);row['passed']=was_live and proc.returncode==0 and row['harness']['status']==0 and len(row['tests'])==1 and all(test['status']==0 for test in row['tests']);runs.append(row);print(name,row['harness'],row['tests'],flush=True)
record={'binary':str(B),'binary_sha256':sha(B),'source_sha256':hashes,'served_source_sha256':served_hashes,'adapter_sha256':sha(Path(__file__)),'runs':runs,'assertion_bodies_changed':False,'headers_preserved':True,'adapter':'Original testharness/resources over loopback with completion reporter injected after testharness; private profile/bus/compositor and inline-only test registry. No immersive device acquired.','full_wpt_runner':False,'passed':all(row['passed']for row in runs),'scope':'Two original WebXR ReportingObserver assertion bodies and headers, served by an isolated loopback adapter. Current Reporting dictionaries are used; no full policy/WebXR/WPT conformance claim.'}
(out/'report.json').write_text(json.dumps(record,indent=2)+'\n');assert len(runs)==2 and all(row['passed']for row in runs),'Original XR reporting assertions failed'
