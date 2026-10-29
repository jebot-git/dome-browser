#!/usr/bin/env python3
"""Measure bounded allocation/release and native UI response in private browser profiles."""
from pathlib import Path
import argparse,hashlib,http.server,json,os,signal,statistics,subprocess,sys,tempfile,threading,time
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_RECLAIM_TEST_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_RECLAIM_TEST_BUS':'1'})
from Xlib import X,display,protocol
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--control',action='store_true');a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False,mode=0o700);checks=[];stages=[];reports=[];samples=[];failure=None;child=xvfb=xd=None
fixture=Path(__file__).with_name('memory-reclaim.html').read_bytes()
class Server(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200);self.send_header('Content-Type','text/html;charset=utf-8');self.send_header('Content-Length',str(len(fixture)));self.send_header('Cache-Control','no-store');self.end_headers();self.wfile.write(fixture)
 def do_POST(self):
  data=json.loads(self.rfile.read(int(self.headers['Content-Length'])));(reports if self.path=='/report' else stages).append(data);self.send_response(204);self.end_headers()
 def log_message(self,*_):pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server);server.daemon_threads=True;threading.Thread(target=server.serve_forever,daemon=True).start()
def check(ok,name):checks.append({'name':name,'passed':bool(ok)});assert ok,name
try:
 with tempfile.TemporaryDirectory(prefix='dome-memory-reclaim-') as directory:
  root=Path(directory);root.chmod(0o700);profile=root/'profile';profile.mkdir();(profile/'filterlists.json').write_text('[]')
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1280x900x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
  with os.fdopen(rd) as pipe:screen=':'+pipe.readline().strip()
  xd=display.Display(screen);env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')};env.update(HOME=directory,DISPLAY=screen,FLTK_BACKEND='x11',LIBGL_ALWAYS_SOFTWARE='1',XDG_RUNTIME_DIR=directory,XDG_CONFIG_HOME=directory+'/config',XDG_DATA_HOME=directory+'/data',XDG_CACHE_HOME=directory+'/cache',XDG_STATE_HOME=directory+'/state',DOME_BROWSER_MEMORY_RECLAIM='0' if a.control else '1',DOME_BROWSER_MEMORY_TRACE='1',RUST_LOG='warn')
  for key in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','MALLOC_ARENA_MAX','GLIBC_TUNABLES']:env.pop(key,None)
  with (out/'browser.log').open('w') as log:
   child=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',str(profile),f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log);start=time.monotonic()
   while not reports and time.monotonic()-start<180:
    assert child.poll() is None,'Browser exited during memory exercise';row={'elapsed':time.monotonic()-start,'stage':stages[-1]['name'] if stages else 'startup'}
    for line in Path(f'/proc/{child.pid}/smaps_rollup').read_text().splitlines():
     fields=line.split()
     if fields and fields[0] in ['Rss:','Pss:','Swap:','Private_Dirty:']:row[fields[0][:-1]+'_kib']=int(fields[1])
    t=time.monotonic()
    try:
     ping=subprocess.run(['gdbus','call','--session','--dest','org.domed.Browser','--object-path','/org/domed/Browser','--method','org.freedesktop.DBus.Peer.Ping'],capture_output=True,env=env,timeout=5);row['ping_ok']=ping.returncode==0
    except subprocess.TimeoutExpired:row['ping_ok']=False
    row['ping_ms']=(time.monotonic()-t)*1000;samples.append(row)
    (out/'observations.json').write_text(json.dumps({'stages':stages,'samples':samples},indent=2)+'\n');time.sleep(.5)
   check(bool(reports),'Bounded allocation/release fixture completes');checks.extend(reports[-1]['checks']);assert all(c['passed'] for c in checks)
   check(reports[-1]['maxGap']<2000,'Animation avoids two-second stalls during reclamation')
   check(all(r['ping_ok'] for r in samples if r['elapsed']>2),'Native UI responds throughout allocation and reclamation')
   for w in xd.screen().root.query_tree().children:
    if w.get_wm_name() and w.get_attributes().map_state==X.IsViewable:w.send_event(protocol.event.ClientMessage(window=w,client_type=xd.intern_atom('WM_PROTOCOLS'),data=(32,[xd.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])))
   xd.sync();check(child.wait(timeout=20)==0,'Browser closes cleanly after reclamation')
except BaseException as e:failure=str(e);raise
finally:
 if child and child.poll() is None:
  child.terminate()
  try:child.wait(timeout=8)
  except subprocess.TimeoutExpired:child.kill();child.wait()
 if xd:xd.close()
 if xvfb:xvfb.terminate();xvfb.wait(timeout=8)
 server.shutdown();server.server_close()
 last=samples[-20:];result={'passed':failure is None and bool(checks),'failure':failure,'control':a.control,'checks':checks,'fixture_report':reports[-1] if reports else None,'peak_resident_plus_swap_kib':max((r.get('Rss_kib',0)+r.get('Swap_kib',0) for r in samples),default=0),'settled_resident_plus_swap_kib':statistics.median([r.get('Rss_kib',0)+r.get('Swap_kib',0) for r in last]) if last else None,'max_native_ping_ms':max((r['ping_ms'] for r in samples if r['elapsed']>2),default=0),'binary_sha256':hashlib.file_digest(a.binary.resolve().open('rb'),'sha256').hexdigest(),'fixture_sha256':hashlib.sha256(fixture).hexdigest(),'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'scope':'Bounded local allocation/release with live data anchors, isolated profile/bus/display. No host memory pressure or global VM changes; resident+swap is measured, not only RSS.'}
 (out/'report.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result),flush=True)
