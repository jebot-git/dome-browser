#!/usr/bin/env python3
"""Actual Servo DOM + native host transport, private Wayland, synthetic input only."""
from pathlib import Path
import os,sys,json,time,signal,tempfile,subprocess,http.server,threading,hashlib,resource,select
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_INPUT_TEST_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_INPUT_TEST_BUS':'1'})
from private_wayland import PrivateWayland
from PIL import Image
from Xlib import X,XK,display
from Xlib.ext import xtest
root=Path(__file__).resolve().parent;binary=Path(sys.argv[1]).resolve();fixture=Path(sys.argv[2]).resolve();out=Path(sys.argv[3]).resolve();out.mkdir(exist_ok=False,parents=True,mode=0o700)
completed=False;checks=[];events=[];logs=[];child=host=wayland=connection=xvfb=site=None;acks=[]
def check(ok,name):
 checks.append({'name':name,'passed':bool(ok)});print(('PASS ' if ok else 'FAIL ')+name,flush=True);assert ok,name

def wait(fn,seconds=20):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  result=fn()
  if result:return result
  if child is not None and child.poll() is not None:raise AssertionError('Browser exited: '+str(child.returncode))
  time.sleep(.05)
 raise AssertionError('Private controller fixture timed out')
def seen(kind):return sorted((e for e in events if e['type']==kind),key=lambda e:e['order'])
def latest():return seen('snapshot')[-1]['data']['sources'][0] if seen('snapshot') and seen('snapshot')[-1]['data']['sources'] else None

def command(**data):
 data['ack']=len(acks)+1;host.stdin.write(json.dumps(data)+'\n');host.stdin.flush()
 check(bool(select.select([host.stdout],[],[],3)[0]),'Native host acknowledges command '+str(data['ack']))
 response=json.loads(host.stdout.readline());assert response['ack']==data['ack'];acks.append(response)

def key(name,mods=()):
 for mod in mods:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(XK.string_to_keysym(mod)))
 code=connection.keysym_to_keycode(XK.string_to_keysym(name));xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
 for mod in reversed(mods):xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(XK.string_to_keysym(mod)))
 connection.sync();time.sleep(.15)
def click(w,x,y):
 w.set_input_focus();g=w.get_geometry();xtest.fake_input(connection,X.MotionNotify,x=g.x+x,y=g.y+y);connection.sync();time.sleep(.05)
 xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync();time.sleep(.15)
def button_pressed(index=0):return latest() and latest()['input']['gamepad'] and latest()['input']['gamepad']['buttons'][index]['pressed']
try:
 with tempfile.TemporaryDirectory(prefix='dome-controller-dom-') as folder:
  runtime=Path(folder);runtime.chmod(0o700)
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*a):pass
   def do_GET(self):
    data=(root/'webxr-input.html').read_bytes();self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
   def do_POST(self):
    events.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))));(out/'events.jsonl').write_text('\n'.join(json.dumps(e) for e in events)+'\n');self.send_response(204);self.end_headers()
  site=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=site.serve_forever,daemon=True).start()
  log=(out/'native-host.log').open('w');logs.append(log)
  host=subprocess.Popen([str(fixture),str(runtime/'xr.sock'),*(['--legacy-clock'] if os.environ.get('DOME_INPUT_LEGACY_CLOCK')=='1' else [])],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1)
  assert select.select([host.stdout],[],[],5)[0];assert json.loads(host.stdout.readline())['ready']
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
  with os.fdopen(rd) as f:screen=':'+f.readline().strip()
  connection=display.Display(screen);wayland=PrivateWayland(connection,screen,runtime,out)
  env={**os.environ,'HOME':folder,'XDG_RUNTIME_DIR':folder,'XDG_CONFIG_HOME':folder+'/config','XDG_DATA_HOME':folder+'/data','XDG_STATE_HOME':folder+'/state','XDG_CACHE_HOME':folder+'/cache','FLTK_BACKEND':'wayland','WAYLAND_DISPLAY':wayland.socket,'DOME_WEBXR_SOCKET':str(runtime/'xr.sock'),'LIBGL_ALWAYS_SOFTWARE':'1','DOME_BROWSER_TRACE':'1','RUST_LOG':'warn'}
  for k in ['WAYLAND_SOCKET','DISPLAY','DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE']:env.pop(k,None)
  log=(out/'browser.log').open('w');logs.append(log)
  child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(runtime/'profile'),f'http://127.0.0.1:{site.server_port}/'],env=env,stdout=log,stderr=log)
  wait(lambda:seen('ready'));w=wait(lambda:next((w for w in wayland.windows() if w.value()['client_pid']==child.pid and 'WebXR controller integration' in w.get_wm_name()),None))
  def capture():
   g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
   return Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
  def painted():
   image=capture()
   return sum(1 for r,g,b in image.crop((0,90,image.width,image.height-28)).getdata() if b-r>50 and b-g>40)>500
  check(wait(painted),'Native document pixels are painted before activation')
  capture().save(out/'before-activation.png')
  def html_button(name):
   data=seen('ready')[-1]['data'];g=w.get_geometry();ratio=data['ratio']
   assert abs(data['viewport'][0]*ratio-g.width)<2,'Fixture viewport changed unexpectedly'
   top=g.height-data['viewport'][1]*ratio-28;rect=data[name]
   click(w,int((rect['x']+rect['width']/2)*ratio),int(top+(rect['y']+rect['height']/2)*ratio))
  html_button('start')
  consent=wait(lambda:next((w for w in wayland.windows() if w.get_wm_name()=='Enter immersive WebXR'),None));consent.set_input_focus();key('Return')
  wait(lambda:seen('running') or seen('error'));check(not seen('error'),'Immersive browser session starts through the native transport');wait(latest)
  gp=latest()['input']['gamepad'];check(gp['index']==-1 and gp['id']=='' and gp['mapping']=='xr-standard' and gp['connected'] and len(gp['buttons'])==7 and len(gp['axes'])==4,'DOM exposes the actual seven-button/four-axis XR controller layout')
  check(latest()['input']['profilesFrozen'],'Controller profile list is frozen')
  timing=seen('running')[-1]['data'];check(abs(timing['predictedDisplayTime']-timing['now'])<150 and 0<=timing['now']-timing['callbackTime']<150,'XR frame and callback timestamps use the document performance timeline')
  check(abs(latest()['pose']-.5)<.001,'Aim pose shares the initial headset-relative local anchor')
  timestamp=latest()['timestamp'];source_events=len(seen('sources'))
  command(touchpad=True);wait(lambda:latest()['input']['gamepad']['axes'][:2]==[.5,-.5]);check(latest()['input']['gamepad']['buttons'][2]['touched'] and not latest()['input']['gamepad']['buttons'][2]['pressed'],'Touchpad contact and press are independent')
  check(latest()['timestamp']>timestamp and len(seen('sources'))==source_events,'Gamepad timestamp advances without replacing the input source')
  command(burst=[{'select':True,'aim_x':3.,'event_age_ms':30},{'select':False,'aim_x':1.5,'event_age_ms':20}]);wait(lambda:seen('selectend'))
  select_events=sorted((e for e in events if e['type'] in ('selectstart','select','selectend')),key=lambda e:e['order'])
  check([e['type'] for e in select_events]==['selectstart','select','selectend'],'A short trigger press produces one complete ordered gesture')
  check(select_events[0]['data']['input']['gamepad']['buttons'][0]['pressed'] and not select_events[1]['data']['input']['gamepad']['buttons'][0]['pressed'],'Gamepad state is applied before event handlers run')
  check(abs(select_events[0]['data']['pose'][12]-2.)<.001,'Selection event retains its transition pose')
  first=select_events[0]['data'];last=select_events[1]['data'];check(10<first['now']-first['eventFrameTime']<500 and abs(last['eventFrameTime']-first['eventFrameTime']-10)<.1,'Selection retains action-time clocks instead of the later display frame time')
  command(select=True);wait(lambda:len(seen('selectstart'))==2);command(focused=False);wait(lambda:len(seen('selectend'))==2)
  check(len(seen('select'))==1,'Native focus loss cancels without a select action')
  command(focused=True);time.sleep(.25);check(len(seen('selectstart'))==2,'A held trigger does not restart when native focus returns')
  command(select=False);time.sleep(.1)
  key('l',('Control_L',));wait(lambda:any(e['data']['state']=='visible-blurred' for e in seen('visibility')))
  starts=len(seen('selectstart'));command(select=True);time.sleep(.3);check(len(seen('selectstart'))==starts,'Address-bar focus prevents page controller input')
  click(w,400,350);time.sleep(.2);check(len(seen('selectstart'))==starts,'Returning from browser chrome does not synthesize a held press')
  command(select=False);time.sleep(.1);command(select=True);wait(lambda:len(seen('selectstart'))==starts+1)
  command(aim_valid=False);wait(lambda:len(seen('selectend'))==starts+1);check(len(seen('select'))==1,'Tracking loss cancels a gesture')
  command(aim_valid=True,select=False);time.sleep(.1)
  starts=len(seen('selectstart'));command(select=True);wait(lambda:len(seen('selectstart'))==starts+1);command(select_active=False);wait(lambda:len(seen('selectend'))==starts+1)
  check(len(seen('select'))==1 and not seen('selectend')[-1]['data']['input']['gamepad']['buttons'][0]['pressed'],'Runtime action deactivation cancels and neutralizes the primary control')
  command(select_active=True);time.sleep(.2);check(len(seen('selectstart'))==starts+1,'Runtime action reactivation does not synthesize a held press');command(select=False);time.sleep(.1)
  starts=len(seen('selectstart'));command(select=True);wait(lambda:len(seen('selectstart'))==starts+1)
  last_tracked_viewer=latest()['viewer_position']
  command(head_valid=False);wait(lambda:len(seen('selectend'))==starts+1);wait(lambda:latest() and latest()['pose'] is None and latest()['viewer'] is True and latest()['viewer_emulated'] is True)
  check(latest()['viewer_position'] is not None and max(abs(a-b)for a,b in zip(latest()['viewer_position'],last_tracked_viewer))<.0001 and latest()['viewer_has_velocity'] is False,'Viewer force emulation retains its known position without restoring controller tracking or reporting measured velocity')
  check(len(seen('select'))==1,'Headset tracking loss cancels input and keeps the browser session running')
  command(head_valid=True,select=False);wait(lambda:latest() and latest()['viewer'] is True and latest()['viewer_emulated'] is False)
  command(visible=False);hidden=wait(lambda:next((e for e in reversed(seen('visibility')) if e['data']['state']=='hidden'),None));wait(lambda:seen('hidden_sample'))
  check(seen('hidden_sample')[-1]['data']['frameCount']==hidden['data']['frameCount'] and not seen('unexpected_hidden_frame'),'Hidden XR sessions suspend animation callbacks until visibility returns')
  visibility_count=len(seen('visibility'));command(visible=True,focused=True)
  wait(lambda:len(seen('visibility'))>visibility_count and seen('visibility')[-1]['data']['state']=='visible');wait(lambda:latest() and latest()['viewer'] is True)
  starts=len(seen('selectstart'));visibility_count=len(seen('visibility'));key('t',('Control_L',));wait(lambda:len(seen('visibility'))>visibility_count and seen('visibility')[-1]['data']['state']=='hidden')
  check(True,'Switching to another tab hides the immersive session')
  command(burst=[{'select':True},{'select':False}]);time.sleep(.25)
  check(len(seen('selectstart'))==starts,'A different browser tab cannot feed controllers to the immersive page')
  key('Tab',('Control_L','Shift_L'));click(w,400,350);time.sleep(.2)
  command(squeeze=True);wait(lambda:seen('squeezestart'));command(squeeze=False);wait(lambda:seen('squeezeend'));check(len(seen('squeeze'))==1,'Squeeze has its own complete event lifecycle')
  old_sources=len(seen('sources'));command(connected=False);wait(lambda:len(seen('sources'))>old_sources)
  removed=seen('sources')[-1]['data']['removed'];check(removed and not removed[0]['gamepad']['connected'],'Removed controller Gamepad becomes disconnected')
  wait(lambda:seen('snapshot')[-1]['data'].get('retired'))
  check(all(seen('snapshot')[-1]['data']['retired']),'Retained spaces return null after their controller disconnects')
  command(connected=True,simple=True,grip=False);wait(lambda:latest() and latest()['input']['gamepad'] is None);check(latest()['input']['grip'] is False,'Single-button controller without a grip has a null Gamepad and grip space')
  command(connected=False);time.sleep(.1);command(connected=True);wait(lambda:latest() and latest()['input']['gamepad'] is not None)
  check(all(seen('snapshot')[-1]['data']['retired']),'Retired spaces remain null after the hand identifier is reused')
  starts=len(seen('selectstart'));selections=len(seen('select'));command(select=True);wait(lambda:len(seen('selectstart'))==starts+1)
  html_button('end');wait(lambda:seen('ended'));wait(lambda:len(seen('selectend'))==starts+1);check(len(seen('selectend'))==starts+1 and len(seen('select'))==selections,'Ending an active session cancels its held gesture without selecting');check(bool(seen('ended')[-1]['data']['retained']) and all(s['gamepad'] is None or not s['gamepad']['connected'] for s in seen('ended')[-1]['data']['retained']),'Session shutdown disconnects retained Gamepads')
  check(not seen('error'),'No script errors during the controller lifecycle')
  wayland.api('/api/action',{'op':'close','id':w.id});check(child.wait(timeout=10)==0,'Browser exits cleanly');child=None;completed=True
finally:
 if child is not None and child.poll() is None:child.terminate();child.wait(timeout=10)
 if host is not None:
  if host.poll() is None:host.stdin.write('{"quit":true}\n');host.stdin.flush();host.wait(timeout=5)
 if wayland:wayland.close()
 if connection:connection.close()
 if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
 if site:site.shutdown();site.server_close()
 for log in logs:log.close()
 (out/'report.json').write_text(json.dumps({'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'native_host_sha256':hashlib.sha256(fixture.read_bytes()).hexdigest(),'checks':checks,'events':events,'host_acks':acks,'passed':completed and bool(checks) and all(c['passed'] for c in checks),'failure':None if completed else str(sys.exc_info()[1]),'legacy_clock':os.environ.get('DOME_INPUT_LEGACY_CLOCK')=='1','fixture_sha256':hashlib.sha256((root/'webxr-input.html').read_bytes()).hexdigest(),'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'scope':'Real Servo DOM and native DomeD projection transport with synthetic controllers in a private Wayland compositor; no physical device or live session changed','goal_status':'active'},indent=2)+'\n')
