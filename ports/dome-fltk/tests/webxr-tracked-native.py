#!/usr/bin/env python3
"""Actual Servo DOM + native host transport, private Wayland, synthetic input only."""
from pathlib import Path
import os,sys,json,time,signal,tempfile,subprocess,http.server,threading,hashlib,resource,select
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_INPUT_TEST_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_INPUT_TEST_BUS':'1'})
from private_wayland import PrivateWayland
import importlib.util
_spec=importlib.util.spec_from_file_location('role_proxy',Path(__file__).with_name('webxr-native-role-proxy.py'));_module=importlib.util.module_from_spec(_spec);_spec.loader.exec_module(_module)
RoleProxy=_module.RoleProxy
from PIL import Image
from Xlib import X,XK,display
from Xlib.ext import xtest
root=Path(__file__).resolve().parent;binary=Path(sys.argv[1]).resolve();fixture=Path(sys.argv[2]).resolve();out=Path(sys.argv[3]).resolve();out.mkdir(exist_ok=False,parents=True,mode=0o700)
completed=False;checks=[];events=[];logs=[];child=host=wayland=connection=xvfb=site=proxy=None;acks=[];page_commands=[]
def check(ok,name):
 checks.append({'name':name,'passed':bool(ok)});print(('PASS ' if ok else 'FAIL ')+name,flush=True)

def wait(fn,seconds=20):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  result=fn()
  if result:return result
  if child is not None and child.poll() is not None:raise AssertionError('Browser exited: '+str(child.returncode))
  time.sleep(.05)
 raise AssertionError('Private reference-space fixture timed out')
def seen(kind):return sorted((e for e in events if e['type']==kind),key=lambda e:e['order'])
def latest():return seen('snapshot')[-1]['data']['sources'][0] if seen('snapshot') and seen('snapshot')[-1]['data']['sources'] else None

def command(**data):

 if 'primary_action' in data:proxy.set_role(data['primary_action'])
 data['ack']=len(acks)+1;host.stdin.write(json.dumps(data)+'\n');host.stdin.flush()
 check(bool(select.select([host.stdout],[],[],3)[0]),'Native host acknowledges command '+str(data['ack']))
 response=json.loads(host.stdout.readline());assert response['ack']==data['ack'];acks.append(response)

def synchronized(after=0):
 values=seen('heartbeat')
 if not values:return None
 event=values[-1]
 return event if event['order']>after and set(range(1,event['order']+1)).issubset({e['order'] for e in events}) else None
def mark():
 wait(synchronized)
 return max(e['order'] for e in events)
def page_command(op):
 before=mark();page_commands.append(op)
 wait(lambda:any(e['order']>before and e['data']['op']==op for e in seen('command')))
def mul(a,b):return [sum(a[k*4+r]*b[c*4+k] for k in range(4)) for c in range(4) for r in range(4)]
def inverse(a):
 b=[0.]*16;b[15]=1.
 for c in range(3):
  for r in range(3):b[c*4+r]=a[r*4+c]
 for r in range(3):b[12+r]=-sum(b[k*4+r]*a[12+k] for k in range(3))
 return b
def translated(x,y,z):return [1,0,0,0,0,1,0,0,0,0,1,0,x,y,z,1]
def validate_reset(before,delta,labels,bases,new_viewer_x=None):
 wait(lambda:len([e for e in seen('reset') if e['order']>before])>=len(labels) or seen('error'))
 check(not seen('error'),'Reset dispatch completes without script errors')
 last=max(e['order'] for e in seen('reset') if e['order']>before)
 wait(lambda:synchronized(last))
 resets=[e for e in seen('reset') if e['order']>before]
 check(sorted(e['data']['name'] for e in resets)==sorted(labels),'Every matching base and offset space receives one reset')
 for e in resets:
  v=e['data'];expected=None if delta is None else mul(mul(inverse(bases[v['name']]),delta),bases[v['name']])
  check((v['matrix'] is None) if expected is None else v['matrix'] is not None and max(abs(x-y) for x,y in zip(v['matrix'],expected))<.0002,'Correct reset transform for '+v['name'])
 check(all(e['data']['trusted'] and e['data']['sameSpace'] and e['data']['sameTransform'] and not e['data']['bubbles'] and not e['data']['cancelable'] for e in resets),'Reset event identity, trust and flags follow the event contract')
 serial=max(e['data']['serial'] for e in resets)
 wait(lambda:any(e['order']>last and e['data']['resetSerial']>=serial for e in seen('frame')))
 if new_viewer_x is not None:
  changed=wait(lambda:next((e for e in seen('frame') if e['order']>before and abs(e['data']['viewer'][12]-new_viewer_x)<.0001),None))
  check(changed['order']>last and changed['data']['resetSerial']>=serial,'The first animation frame with changed tracking sees every reset event')
 else:check(True,'Reset events precede subsequent animation callbacks')
 return resets

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
 with tempfile.TemporaryDirectory(prefix='dome-reset-dom-') as folder:
  runtime=Path(folder);runtime.chmod(0o700)
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*a):pass
   def do_GET(self):
    data=(json.dumps(page_commands.pop(0) if page_commands else None).encode() if self.path=='/command' else (root/'webxr-tracked-native.html').read_bytes());self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
   def do_POST(self):
    events.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))));(out/'events.jsonl').write_text('\n'.join(json.dumps(e) for e in events)+'\n');self.send_response(204);self.end_headers()
  site=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=site.serve_forever,daemon=True).start()
  log=(out/'native-host.log').open('w');logs.append(log)
  host=subprocess.Popen([str(fixture),str(runtime/'xr.sock'),'--reference-spaces',*(['--legacy-clock'] if os.environ.get('DOME_INPUT_LEGACY_CLOCK')=='1' else [])],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1)
  assert select.select([host.stdout],[],[],5)[0];assert json.loads(host.stdout.readline())['ready']
  proxy=RoleProxy(runtime/'proxy.sock',runtime/'xr.sock')
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
  with os.fdopen(rd) as f:screen=':'+f.readline().strip()
  connection=display.Display(screen);wayland=PrivateWayland(connection,screen,runtime,out)
  env={**os.environ,'HOME':folder,'XDG_RUNTIME_DIR':folder,'XDG_CONFIG_HOME':folder+'/config','XDG_DATA_HOME':folder+'/data','XDG_STATE_HOME':folder+'/state','XDG_CACHE_HOME':folder+'/cache','FLTK_BACKEND':'wayland','WAYLAND_DISPLAY':wayland.socket,'DOME_WEBXR_SOCKET':str(runtime/'proxy.sock'),'LIBGL_ALWAYS_SOFTWARE':'1','DOME_BROWSER_TRACE':'1','RUST_LOG':'warn'}
  for k in ['WAYLAND_SOCKET','DISPLAY','DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE']:env.pop(k,None)
  log=(out/'browser.log').open('w');logs.append(log)
  child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(runtime/'profile'),f'http://127.0.0.1:{site.server_port}/?grant='+os.environ.get('DOME_TRACKED_GRANT','1')],env=env,stdout=log,stderr=log)
  wait(lambda:seen('ready'));w=wait(lambda:next((w for w in wayland.windows() if w.value()['client_pid']==child.pid and 'WebXR reference-space integration' in w.get_wm_name()),None))
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
  wait(lambda:seen('running') or seen('error'));check(not seen('error'),'Immersive browser session starts through the native transport')
  def snapshot_after(before, predicate=lambda d:True):
   return wait(lambda:next((e['data']for e in reversed(seen('frame'))if e['order']>before and predicate(e['data'])),None))
  def change_names(before):return [e['data']['name']for e in seen('change')if e['order']>before]
  def action_names(before):return [e['data']['name']for e in seen('input')if e['order']>before]
  def sync_frames(before):
   data=snapshot_after(before);snapshot_after(before,lambda value:value['frameCount']>data['frameCount']+4)
  granted=os.environ.get('DOME_TRACKED_GRANT','1')!='0'
  data=snapshot_after(0,lambda d:len(d['primary'])==1)
  original=data['primary'][0]['source'];check(not data['tracked'],'Legacy native metadata remains primary input')
  check(('tracked-sources'in data['enabled'])==granted,'Native backend negotiates the tracked-source grant')
  before=mark();command(select=True)
  wait(lambda:action_names(before)==['selectstart']);check(True,'Native primary selection starts before classification changes')
  before=mark();command(primary_action=False)
  data=snapshot_after(before,lambda d:not d['primary'] and len(d['tracked'])==int(granted));sync_frames(before)
  check(action_names(before)==['selectend'],'Native demotion cancels the held primary action without completion')
  ordered=[e['type']+':'+e['data'].get('name','')for e in sorted(events,key=lambda e:e['order'])if e['order']>before and e['type']in ['change','input']]
  expected=['input:selectend','change:inputsourceschange']+(['change:trackedsourceschange']if granted else [])
  check(ordered==expected,'Native cancellation precedes primary removal and any granted tracked addition')
  retained=next(value for value in data['retained']if value['source']==original)
  check(retained['aim']is None and not retained['gamepad']['connected'],'Native demotion disconnects old source poses and gamepad')
  if granted:
   tracking=data['tracked'][0];tracked_id=tracking['source']
   check(tracked_id!=original and tracking['aim']is not None and tracking['grip']is not None,'Granted native tracking carries fresh identity and provider poses')
   check(tracking['gamepad']['mapping']=='','Native tracked auxiliary controls are not advertised as a primary-action mapping')
  before=mark();command(select=False);command(select=True);sync_frames(before)
  check(not action_names(before),'Raw native tracker control changes produce no primary action events')
  before=mark();command(primary_action=False);sync_frames(before);check(not change_names(before),'Identical native role metadata does not replace the source')
  before=mark();command(profile='role-tracker-v2');data=snapshot_after(before,lambda d:any(value['profile']=='role-tracker-v2'for value in d['tracked']) if granted else not d['primary']);sync_frames(before)
  check(change_names(before)==(['trackedsourceschange']if granted else []),'Native tracker metadata replacements respect the feature grant')
  if granted:
   check(data['tracked'][0]['source']!=tracked_id,'Native tracker profile replacement creates new identity')
   check(next(value for value in data['retained']if value['source']==tracked_id)['aim']is None,'A replaced native tracker cannot regain poses through its device ID')
  before=mark();command(primary_action=True);data=snapshot_after(before,lambda d:len(d['primary'])==1 and not d['tracked']);sync_frames(before)
  promoted=data['primary'][0]['source']
  check(change_names(before)==['inputsourceschange']+(['trackedsourceschange']if granted else []),'Native promotion adds primary input before removing old tracked input')
  check(promoted!=original and not action_names(before),'Native promotion creates fresh identity without manufacturing a held action')
  before=mark();command(select=False);sync_frames(before);check(not action_names(before),'Releasing retained state after promotion does not complete an absent action')
  before=mark();command(select=True);wait(lambda:action_names(before)==['selectstart']);command(select=False);wait(lambda:action_names(before)==['selectstart','select','selectend']);check(True,'Fresh native primary actions work after promotion')
  before=mark();command(primary_action=False);data=snapshot_after(before,lambda d:not d['primary']);sync_frames(before)
  old_tracked=data['tracked'][0]['source']if granted else None
  before=mark();command(connected=False);data=snapshot_after(before,lambda d:not d['primary']and not d['tracked']);sync_frames(before)
  check(change_names(before)==(['trackedsourceschange']if granted else []),'Native tracker disconnect emits only its granted collection removal')
  before=mark();command(connected=True);data=snapshot_after(before,lambda d:len(d['tracked'])==1 if granted else not d['primary']);sync_frames(before)
  check(change_names(before)==(['trackedsourceschange']if granted else []),'Native tracker reconnect remains outside primary input')
  if granted:check(data['tracked'][0]['source']!=old_tracked,'Native tracker reconnect cannot reuse a retained source object')
  check(all(e['data']['trusted']and not e['data']['cancelable']for e in seen('change')),'Native collection events are trusted and noncancelable')
  check(not proxy.errors,'Synthetic role adapter preserves the native bridge transport without errors')
  page_command('end');wait(lambda:seen('ended'));check(not seen('error'),'Native tracked-source lifecycle completes without script errors');check(seen('ended')[-1]['data']['primary']==0 and seen('ended')[-1]['data']['tracked']==0,'Native session end clears both live collections')
  wayland.api('/api/action',{'op':'close','id':w.id});check(child.wait(timeout=10)==0,'Browser exits cleanly');child=None;completed=True

finally:
 if child is not None and child.poll() is None:child.terminate();child.wait(timeout=10)
 if host is not None:
  if host.poll() is None:host.stdin.write('{"quit":true}\n');host.stdin.flush();host.wait(timeout=5)
 if proxy:proxy.close()
 if wayland:wayland.close()
 if connection:connection.close()
 if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
 if site:site.shutdown();site.server_close()
 for log in logs:log.close()
 (out/'report.json').write_text(json.dumps({'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'native_host_sha256':hashlib.sha256(fixture.read_bytes()).hexdigest(),'checks':checks,'events':events,'host_acks':acks,'passed':completed and bool(checks) and all(c['passed'] for c in checks),'failure':None if completed else str(sys.exc_info()[1]),'legacy_clock':os.environ.get('DOME_INPUT_LEGACY_CLOCK')=='1','fixture_sha256':hashlib.sha256((root/'webxr-tracked-native.html').read_bytes()).hexdigest(),'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'scope':'Real Servo DOM and native DomeD bridge with synthetic provider classification metadata; no physical tracker or live desktop session changed','goal_status':'active'},indent=2)+'\n')

assert completed and all(c["passed"]for c in checks), "Native input emulation checks failed; inspect report.json"
