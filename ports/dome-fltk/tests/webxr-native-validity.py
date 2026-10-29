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
completed=False;checks=[];events=[];logs=[];child=host=wayland=connection=xvfb=site=None;acks=[];page_commands=[]
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
    data=(json.dumps(page_commands.pop(0) if page_commands else None).encode() if self.path=='/command' else (root/'webxr-native-validity.html').read_bytes());self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
   def do_POST(self):
    events.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))));(out/'events.jsonl').write_text('\n'.join(json.dumps(e) for e in events)+'\n');self.send_response(204);self.end_headers()
  site=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=site.serve_forever,daemon=True).start()
  log=(out/'native-host.log').open('w');logs.append(log)
  host=subprocess.Popen([str(fixture),str(runtime/'xr.sock'),'--reference-spaces',*(['--legacy-clock'] if os.environ.get('DOME_INPUT_LEGACY_CLOCK')=='1' else [])],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1)
  assert select.select([host.stdout],[],[],5)[0];assert json.loads(host.stdout.readline())['ready']
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
  with os.fdopen(rd) as f:screen=':'+f.readline().strip()
  connection=display.Display(screen);wayland=PrivateWayland(connection,screen,runtime,out)
  env={**os.environ,'HOME':folder,'XDG_RUNTIME_DIR':folder,'XDG_CONFIG_HOME':folder+'/config','XDG_DATA_HOME':folder+'/data','XDG_STATE_HOME':folder+'/state','XDG_CACHE_HOME':folder+'/cache','FLTK_BACKEND':'wayland','WAYLAND_DISPLAY':wayland.socket,'DOME_WEBXR_SOCKET':str(runtime/'xr.sock'),'LIBGL_ALWAYS_SOFTWARE':'1','DOME_BROWSER_TRACE':'1','RUST_LOG':'warn'}
  for k in ['WAYLAND_SOCKET','DISPLAY','DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE']:env.pop(k,None)
  log=(out/'browser.log').open('w');logs.append(log)
  child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(runtime/'profile'),f'http://127.0.0.1:{site.server_port}/'],env=env,stdout=log,stderr=log)
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
  def near(p,expected):return p is not None and max(abs(a-b)for a,b in zip(p['position'],expected))<.0002
  def oriented(p,expected):return p is not None and max(abs(a-b)for a,b in zip(p['orientation'],expected))<.0002
  data=snapshot_after(0,lambda d:d['local']is not None)
  check(near(data['local'],[0,0,0])and not data['local']['emulated'],'Fully tracked native head begins with a non-emulated viewer pose')
  before=mark();command(head_x=2.,head_flags=15,view_flags=0)
  independent=snapshot_after(before,lambda d:d['input_pose_available']is False)
  check(near(independent['local'],[1,0,0])and not independent['local']['emulated'],'Fresh valid head tracking advances despite unavailable eye tracking')
  check(near(independent['ordinary'],[1,0,0])and not independent['ordinary']['emulated'],'Ordinary viewer pose uses independent valid head tracking')
  check(independent['eyes']==['left','right']and len(independent['views'])==2 and near(independent['views'][0],[-.03,0,0])and near(independent['views'][1],[.03,0,0]),'Unavailable eye tracking retains known viewer-relative stereo calibration')
  before=mark();command(head_x=3.,head_flags=7,view_flags=15)
  estimated=snapshot_after(before,lambda d:near(d['local'],[2,0,0]))
  check(estimated['local']['emulated'],'Valid but untracked native position is marked emulated')
  check(estimated['ordinary']is not None and estimated['ordinary']['emulated'],'Ordinary pose reports reliance on valid but untracked native position')
  check(not estimated['local']['has_velocity'],'Estimated native position does not report measured velocity')
  before=mark();command(head_flags=5,view_flags=0,head_position=[90.,90.,90.],head_yaw=1.5707963267948966)
  partial=snapshot_after(before,lambda d:d['input_pose_available']is False and d['frameCount']>=estimated['frameCount']+6)
  expected_orientation=[0.,2**-.5,0.,2**-.5]
  check(near(partial['local'],[2,0,0])and partial['local']['emulated'],'Orientation-only tracking retains the last meaningful position')
  check(oriented(partial['local'],expected_orientation),'Orientation-only tracking uses the newly valid head orientation')
  check(partial['ordinary']is not None and partial['ordinary']['emulated']and oriented(partial['ordinary'],expected_orientation),'Ordinary pose supports 3DoF tracking with estimated position')
  check(len(partial['world_views'])==2 and near(partial['world_views'][0],[2,0,.03])and near(partial['world_views'][1],[2,0,-.03]),'Retained eye calibration composes with the current orientation')
  before=mark();command(head_flags=0,view_flags=0,head_position=[200.,200.,200.],head_yaw=.3)
  lost=snapshot_after(before,lambda d:d['frameCount']>=partial['frameCount']+6)
  check(lost['ordinary']is None,'Total tracking loss does not invent an ordinary world pose')
  check(near(lost['local'],[2,0,0])and oriented(lost['local'],expected_orientation)and lost['local']['emulated'],'Force emulation retains the last meaningful partial-tracking pose')
  check(near(lost['self'],[0,0,0])and not lost['self']['emulated'],'Total loss preserves a non-emulated static viewer relationship')
  before=mark();command(head_flags=15,view_flags=15,head_position=[4.,1.6,2.],head_yaw=0.,eye_positions=[[3.97,1.6,2.],[4.03,1.6,2.]])
  restored=snapshot_after(before,lambda d:near(d['local'],[3,0,0]))
  check(not restored['local']['emulated']and oriented(restored['local'],[0,0,0,1]),'Fully tracked restoration replaces position, orientation and emulation state')
  page_command('end');wait(lambda:seen('ended'));check(not seen('error'),'Independent and partial native tracking lifecycle completes without script errors')
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
 (out/'report.json').write_text(json.dumps({'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'native_host_sha256':hashlib.sha256(fixture.read_bytes()).hexdigest(),'checks':checks,'events':events,'host_acks':acks,'passed':completed and bool(checks) and all(c['passed'] for c in checks),'failure':None if completed else str(sys.exc_info()[1]),'legacy_clock':os.environ.get('DOME_INPUT_LEGACY_CLOCK')=='1','fixture_sha256':hashlib.sha256((root/'webxr-native-validity.html').read_bytes()).hexdigest(),'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'scope':'Real Servo DOM and native DomeD projection transport with synthetic reference-space state and controllers in a private Wayland compositor; no physical device or live session changed','goal_status':'active'},indent=2)+'\n')

assert completed and all(c["passed"]for c in checks), "Native validity checks failed; inspect report.json"
