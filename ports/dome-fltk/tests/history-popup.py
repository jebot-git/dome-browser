#!/usr/bin/env python3
"""Exercise native history captions and selection with an isolated large profile."""
from pathlib import Path
import argparse,contextlib,hashlib,http.server,json,os,resource,signal,subprocess,sys,tempfile,threading,time
from Xlib import X,XK,display
from Xlib.ext import xtest
from private_wayland import PrivateWayland
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_HISTORY_TEST_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_HISTORY_TEST_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True)
p.add_argument('--open-via',choices=['shortcut','desktop-menu'],default='shortcut');p.add_argument('--repetitions',type=int,default=5);a=p.parse_args()
if not 1<=a.repetitions<=1000:p.error('--repetitions must be between 1 and 1000')
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);checks=[];failure=None;child=None;hits=[]
def stop(proc):
 if proc.poll() is None:
  proc.terminate()
  try:proc.wait(timeout=8)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()
def check(value,label):
 checks.append(dict(name=label,passed=bool(value)));print(('PASS ' if value else 'FAIL ')+label,flush=True);assert value,label
def wait(fn):
 deadline=time.monotonic()+20
 while time.monotonic()<deadline:
  if child and child.poll() is not None:raise AssertionError('Browser exited unexpectedly: '+str(child.returncode))
  value=fn()
  if value:return value
  time.sleep(.03)
 raise AssertionError('History fixture timed out')
try:
 with contextlib.ExitStack() as stack:
  root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-history-')));root.chmod(0o700)
  class Server(http.server.BaseHTTPRequestHandler):
   def do_GET(self):
    hits.append(self.path)
    title='History fixture' if self.path.startswith('/seed') else ('Long title '+('世界 / & title '*350) if self.path.startswith('/title') else 'Selected history entry')
    b=('<title>'+title+'</title><body style="background:#234;color:white">History fixture').encode();self.send_response(200);self.send_header('Content-Type','text/html;charset=utf-8');self.send_header('Content-Length',str(len(b)));self.end_headers();self.wfile.write(b)
   def log_message(self,*_):pass
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
  origin=f'http://127.0.0.1:{server.server_port}';target='/entry?text='+('normal-history-value&section=page/'*140)+'&selected=second'
  first=target.replace('selected=second','selected=first')
  profile=root/'profile';profile.mkdir();urls=[origin+'/seed',origin+first,origin+target]+[origin+'/item/'+str(i) for i in range(1997)];(profile/'hs.txt').write_text('\n'.join(urls)+'\n')
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1400x1000x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd)
  connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,root,out);stack.callback(wayland.close)
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')};env.update(HOME=str(root),XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket,LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn')
  for name in ['DISPLAY','WAYLAND_SOCKET']:env.pop(name,None)
  log=stack.enter_context((out/'browser.log').open('w'));child=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',str(profile),origin+'/seed'],env=env,stdout=log,stderr=log);stack.callback(stop,child)
  window=wait(lambda:next((w for w in wayland.windows() if w.value()['client_pid']==child.pid and str(w.get_wm_name()).startswith('History fixture')),None));window.set_input_focus();check(True,'Browser opens a profile with 2000 history entries and long URLs')
  def key(name,ctrl=False):
   if ctrl:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(XK.XK_Control_L))
   code=connection.keysym_to_keycode(XK.string_to_keysym(name));xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
   if ctrl:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(XK.XK_Control_L))
   connection.sync()
  if a.open_via=='desktop-menu':
   import dbus
   bus=dbus.SessionBus();daemon=dbus.Interface(bus.get_object('org.freedesktop.DBus','/org/freedesktop/DBus'),'org.freedesktop.DBus')
   def menu_for_browser():
    for name in bus.list_names():
     if name.startswith(':'):
      try:
       if int(daemon.GetConnectionUnixProcessID(name))==child.pid:
        menu=dbus.Interface(bus.get_object(name,'/org/de_xr/FLTK/Menu1'),'com.canonical.dbusmenu')
        _,layout=menu.GetLayout(0,-1,[])
        def find(node):
         identity,properties,children=node
         if properties.get('label')=='Open History':return identity
         return next((found for row in children if (found:=find(row)) is not None),None)
        identity=find(layout)
        if identity is not None:return menu,identity
      except dbus.DBusException:pass
    return None
   menu,identity=wait(menu_for_browser)
   check(True,'History command is available through the exported desktop menu')
   def open_history():menu.Event(identity,'clicked',dbus.Int32(0,variant_level=1),dbus.UInt32(0))
  else:
   def open_history():key('h',True)
  for iteration in range(a.repetitions):
   open_history();wait(lambda:wayland.scene()['popups']);check(child.poll() is None,'Opening history survives long captions, repetition '+str(iteration));key('Escape');wait(lambda:not wayland.scene()['popups'])
  open_history();wait(lambda:wayland.scene()['popups']);before={p['id'] for p in wayland.scene()['popups']};[key('Down') for _ in range(32)];key('Return')
  wait(lambda:wayland.scene()['popups'] and {p['id'] for p in wayland.scene()['popups']}!=before);check(child.poll() is None,'History pages forward without creating an oversized popup')
  before={p['id'] for p in wayland.scene()['popups']};key('Down');[key('Up') for _ in range(32)];key('Return');wait(lambda:wayland.scene()['popups'] and {p['id'] for p in wayland.scene()['popups']}!=before);check(child.poll() is None,'History returns to its previous page');key('Escape');wait(lambda:not wayland.scene()['popups'])
  open_history();wait(lambda:wayland.scene()['popups']);key('Down');[key('Up') for _ in range(32)];key('Down');key('Down');key('Return');wait(lambda:target in hits)
  check(target in hits and first not in hits,'Identically shortened captions retain separate entries and navigate the full selected URL')
  key('l',True);key('a',True)
  for char in origin+'/title':
   symbol=XK.string_to_keysym(char) or ord(char);code=connection.keysym_to_keycode(symbol);shift=connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
   if shift:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(XK.XK_Shift_L))
   xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
   if shift:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(XK.XK_Shift_L))
  connection.sync();key('Return');window=wait(lambda:next((w for w in wayland.windows() if w.value()['client_pid']==child.pid and str(w.get_wm_name()).startswith('Long title ')),None));check(child.poll() is None,'Long Unicode page titles remain usable in the tab selector')
  open_history();wait(lambda:wayland.scene()['popups']);key('Escape');wait(lambda:not wayland.scene()['popups']);check(child.poll() is None,'History remains usable after loading a long page title')
  wayland.api('/api/action',{'op':'close','id':window.id});check(child.wait(timeout=10)==0,'Browser closes cleanly after history operations');child=None
  saved=(profile/'hs.txt').read_text().splitlines();check(origin+target in saved and origin+first in saved,'Displayed caption shortening preserves both complete stored URLs')
except BaseException as error:failure=str(error);raise
finally:
 (out/'report.json').write_text(json.dumps(dict(passed=failure is None and bool(checks),failure=failure,checks=checks,open_via=a.open_via,repetitions=a.repetitions,binary_sha256=hashlib.sha256(a.binary.read_bytes()).hexdigest(),scope='Private profile, synthetic long URLs, repeated native Wayland history operations; no personal history copied'),indent=2)+'\n')
