#!/usr/bin/env python3
"""Exercise native WebXR activation timing using trusted input in a private compositor."""
from pathlib import Path
import contextlib,hashlib,http.server,json,os,resource,signal,subprocess,sys,tempfile,threading,time,traceback
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_ACTIVATION_TEST_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_ACTIVATION_TEST_BUS':'1'})
from Xlib import X,XK,display
from Xlib.ext import xtest
from private_wayland import PrivateWayland
root=Path(__file__).resolve().parent;binary=Path(sys.argv[1]).resolve();fixture=Path(sys.argv[2]).resolve();out=Path(sys.argv[3]).resolve();out.mkdir(parents=True,exist_ok=False,mode=0o700)
checks=[];events=[];commands=[];child=None;failure=None;host_exit=None
sha=lambda p:hashlib.sha256(Path(p).read_bytes()).hexdigest()
def check(ok,name):
 checks.append({'name':name,'passed':bool(ok)})
 assert ok,name
def stop(proc):
 if proc.poll()is None:
  proc.terminate()
  try:proc.wait(timeout=8)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()
def seen(kind):return sorted((e for e in events if e['type']==kind),key=lambda e:e['order'])
def result(kind):return next((e['data']for e in seen('case_result')if e['data']['kind']==kind),None)
def wait(fn,seconds=20):
 deadline=time.monotonic()+seconds
 while time.monotonic()<deadline:
  if child is not None and child.poll()is not None:raise AssertionError('Private browser exited: '+str(child.returncode))
  if seen('error'):raise AssertionError('Fixture error: '+str(seen('error')[-1]['data']))
  value=fn()
  if value:return value
  time.sleep(.025)
 raise AssertionError('Activation fixture observation timed out')
try:
 with contextlib.ExitStack()as stack:
  folder=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-xr-activation-')));folder.chmod(0o700)
  host_log=stack.enter_context((out/'native-host.log').open('w'));host=subprocess.Popen([str(fixture),str(folder/'xr.sock'),'--reference-spaces',*(['--legacy-clock']if os.environ.get('DOME_INPUT_LEGACY_CLOCK')=='1'else[])],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=host_log,text=True,bufsize=1);stack.callback(stop,host)
  import select
  assert select.select([host.stdout],[],[],5)[0]and json.loads(host.stdout.readline())['ready']
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd)
  connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,folder,out);stack.callback(wayland.close)
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*_):pass
   def do_GET(self):
    body=json.dumps(commands.pop(0)if commands else None).encode()if self.path=='/command'else(root/'webxr-activation-native.html').read_bytes();self.send_response(200);self.send_header('Content-Type','application/json'if self.path=='/command'else'text/html');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
   def do_POST(self):
    events.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))));self.send_response(204);self.end_headers()
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
  env={**os.environ,'HOME':str(folder),'XDG_RUNTIME_DIR':str(folder),'XDG_CONFIG_HOME':str(folder/'config'),'XDG_DATA_HOME':str(folder/'data'),'XDG_STATE_HOME':str(folder/'state'),'XDG_CACHE_HOME':str(folder/'cache'),'FLTK_BACKEND':'wayland','WAYLAND_DISPLAY':wayland.socket,'DOME_WEBXR_SOCKET':str(folder/'xr.sock'),'LIBGL_ALWAYS_SOFTWARE':'1','RUST_LOG':'warn'}
  for key in ['DISPLAY','WAYLAND_SOCKET','DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE']:env.pop(key,None)
  log=stack.enter_context((out/'browser.log').open('w'));child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(folder/'profile'),f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log);stack.callback(stop,child)
  wait(lambda:seen('ready'));window=wait(lambda:next((w for w in wayland.windows()if w.value()['client_pid']==child.pid and'WebXR activation timing integration'in w.get_wm_name()),None))
  check(result('startup')['outcome']=='rejected'and result('startup')['name']=='SecurityError'and not result('startup')['atCall'],'A fresh document cannot enter immersive XR without activation')
  check(result('synthetic')['outcome']=='rejected'and result('synthetic')['name']=='SecurityError'and not result('synthetic')['trusted']and not result('synthetic')['atCall'],'A synthetic click does not grant immersive activation')
  def consent():return next((w for w in wayland.windows()if w.get_wm_name()=='Enter immersive WebXR'),None)
  check(consent()is None,'Unactivated requests do not open a consent dialog')
  def page_command(value):
   before=len(seen('command'));commands.append(value);wait(lambda:len(seen('command'))>before and seen('command')[-1]['data']['value']==value)
  def key(window,name):
   window.set_input_focus();code=connection.keysym_to_keycode(XK.string_to_keysym(name));xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code);connection.sync();time.sleep(.15)
  def click_start(kind):
   page_command('mode:'+kind);window.set_input_focus();data=seen('ready')[-1]['data'];geometry=window.get_geometry();ratio=data['ratio'];rect=data['start'];assert abs(data['viewport'][0]*ratio-geometry.width)<2
   top=geometry.height-data['viewport'][1]*ratio-28;xtest.fake_input(connection,X.MotionNotify,x=geometry.x+int((rect['x']+rect['width']/2)*ratio),y=geometry.y+int(top+(rect['y']+rect['height']/2)*ratio));connection.sync();time.sleep(.05);xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync()
  for kind in ['timer','microtask']:
   click_start(kind);wait(lambda:result(kind)or consent());attempt=next(e['data']for e in seen('attempt')if e['data']['kind']==kind)
   check(attempt['trusted']and attempt['atClick']and attempt['atCall'],kind+': trusted activation remains available at deferred request time')
   dialog=consent();check(dialog is not None,kind+': deferred request reaches native consent');key(dialog,'Return');data=wait(lambda:result(kind));check(data['outcome']=='fulfilled'and data['pose']and data['frames']>=2 and data['eyes']==['left','right'],kind+': accepted request renders stereo XR frames');page_command('end');check(any(e['data']['kind']==kind for e in seen('ended')),kind+': accepted session ends cleanly')
  click_start('expired');wait(lambda:result('expired')or consent(),seconds=12);data=result('expired');check(data is not None and data['trusted']and data['atClick']and not data['atCall']and data['outcome']=='rejected'and data['name']=='SecurityError','Expired transient activation cannot enter immersive XR');check(consent()is None,'Expiry does not open native consent')
  click_start('deny');dialog=wait(lambda:consent()or result('deny'));check(consent()is not None,'Immediate trusted request still requires native consent');key(dialog,'Escape');data=wait(lambda:result('deny'));check(data['outcome']=='rejected','Declining native consent rejects the request')
  click_start('recovery');dialog=wait(lambda:consent()or result('recovery'));check(consent()is not None,'A new trusted request can retry after refusal');key(dialog,'Return');data=wait(lambda:result('recovery'));check(data['outcome']=='fulfilled'and data['pose'],'Retry after refusal renders successfully');page_command('end');check(any(e['data']['kind']=='recovery'for e in seen('ended')),'Retry session ends cleanly')
  check(not seen('error'),'Activation, refusal and retry lifecycle has no script errors');wayland.api('/api/action',{'op':'close','id':window.id});check(child.wait(timeout=10)==0,'Browser closes cleanly after activation timing tests');child=None
  host.stdin.write('{"quit":true}\n');host.stdin.flush();host_exit=host.wait(timeout=5);check(host_exit==0,'Synthetic host closes cleanly')
except BaseException:
 failure=traceback.format_exc();raise
finally:
 (out/'report.json').write_text(json.dumps({'passed':failure is None and bool(checks)and all(c['passed']for c in checks),'checks':checks,'count':len(checks),'events':events,'failure':failure,'binary_sha256':sha(binary),'fixture_sha256':sha(fixture),'source_sha256':{'webxr-activation-native.py':sha(Path(__file__)),'webxr-activation-native.html':sha(root/'webxr-activation-native.html')},'legacy_clock':os.environ.get('DOME_INPUT_LEGACY_CLOCK')=='1','host_exit_code':host_exit,'scope':'Trusted input through a private native Wayland browser and synthetic host; production consent/activation defaults, isolated profile and bus; no physical XR device or live desktop session changed','geolocation_excluded':True,'full_goal_achieved':False},indent=2)+'\n')
