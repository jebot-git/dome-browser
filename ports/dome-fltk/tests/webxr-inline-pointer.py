#!/usr/bin/env python3
"""Native mouse input into real inline XR canvases in a private Wayland session."""
from pathlib import Path
import argparse,contextlib,hashlib,http.server,json,math,os,resource,select,signal,subprocess,sys,tempfile,threading,time
signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_POINTER_TEST_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_POINTER_TEST_BUS':'1'})
from private_wayland import PrivateWayland
from Xlib import X,XK,display
from Xlib.ext import xtest
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--host-fixture',type=Path);p.add_argument('--diagnostic',action='store_true',help='Record assertion failures and continue collecting evidence; failures still fail the run');args=p.parse_args()
binary=args.binary.resolve();out=args.output.resolve();out.mkdir(parents=True,exist_ok=False)
root=Path(__file__).resolve().parent;fixture=root/'webxr-inline-pointer.html'
events=[];checks=[];commands=[];logs=[];thread_samples=[];child=host=wayland=xvfb=connection=site=None;completed=False
paths=contextlib.ExitStack()
def check(ok,name):
 checks.append({'name':name,'passed':bool(ok)});print(('PASS ' if ok else 'FAIL ')+name,flush=True)
 if not ok and not args.diagnostic:raise AssertionError(name)
def seen(kind):return sorted((e for e in events if e['type']==kind),key=lambda e:e['order'])
def wait(fn,seconds=20):
 deadline=time.monotonic()+seconds
 while time.monotonic()<deadline:
  if seen('error'):raise AssertionError(seen('error')[-1]['data']['message'])
  if child is not None and child.poll() is not None:raise AssertionError('Browser exited: '+str(child.returncode))
  value=fn()
  if value:return value
  time.sleep(.05)
 raise AssertionError('Inline pointer fixture timed out')
def latest():return seen('heartbeat')[-1]['data'] if seen('heartbeat') else None
def selection(index,name,after=0):return [e for e in seen('selection') if e['order']>after and e['data']['index']==index and e['data']['name']==name]
def mark():return max((e['order'] for e in events),default=0)
def synced_heartbeat(after=0):
 values=seen('heartbeat')
 if not values:return None
 event=values[-1]
 return event if event['order']>after and set(range(1,event['order']+1)).issubset({e['order'] for e in events}) else None
def command(op):
 before=mark();commands.append({'op':op});ack=wait(lambda:next((e for e in seen('command') if e['order']>before and e['data']['op']==op),None));wait(lambda:synced_heartbeat(ack['order']))
def key(name,mods=()):
 for mod in mods:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(XK.string_to_keysym(mod)))
 code=connection.keysym_to_keycode(XK.string_to_keysym(name));xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
 for mod in reversed(mods):xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(XK.string_to_keysym(mod)))
 connection.sync()
def move(fraction=.5,vertical=.5,which='rect'):
 data=latest();g=window.get_geometry();rect=data[which];ratio=data['ratio'];top=g.height-data['viewport'][1]*ratio-28
 xtest.fake_input(connection,X.MotionNotify,x=g.x+int((rect['x']+rect['width']*fraction)*ratio),y=g.y+int(top+(rect['y']+rect['height']*vertical)*ratio));connection.sync();time.sleep(.08)
def button(pressed,number=1):xtest.fake_input(connection,X.ButtonPress if pressed else X.ButtonRelease,number);connection.sync()
def descriptor(d):return d['mode']=='screen' and d['handedness']=='none' and d['grip'] and d['gamepad'] and d['frozen']
def direction_close(value,expected):
 length=math.sqrt(sum(x*x for x in expected));return value and max(abs(a-b/length) for a,b in zip(value['direction'],expected))<.04
try:
 folder=paths.enter_context(tempfile.TemporaryDirectory(prefix='dome-inline-pointer-'));runtime=Path(folder);runtime.chmod(0o700)
 class Site(http.server.BaseHTTPRequestHandler):
  def log_message(self,*a):pass
  def do_GET(self):
   body=fixture.read_bytes();self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
  def do_POST(self):
   e=json.loads(self.rfile.read(int(self.headers['Content-Length'])));events.append(e)
   (out/'events.jsonl').write_text('\n'.join(json.dumps(x) for x in events)+'\n')
   body=json.dumps(commands.pop(0) if commands and e['type']=='heartbeat' else None).encode();self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers()
   try:self.wfile.write(body)
   except (BrokenPipeError,ConnectionResetError):pass
 site=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=site.serve_forever,daemon=True).start()
 if args.host_fixture:
  log=(out/'native-host.log').open('w');logs.append(log)
  host=subprocess.Popen([str(args.host_fixture.resolve()),str(runtime/'xr.sock')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1)
  assert select.select([host.stdout],[],[],5)[0] and json.loads(host.stdout.readline())['ready']
 rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
 with os.fdopen(rd) as stream:screen=':'+stream.readline().strip()
 connection=display.Display(screen);wayland=PrivateWayland(connection,screen,runtime,out)
 env={**os.environ,'HOME':folder,'XDG_RUNTIME_DIR':folder,'XDG_CONFIG_HOME':folder+'/config','XDG_DATA_HOME':folder+'/data','XDG_STATE_HOME':folder+'/state','XDG_CACHE_HOME':folder+'/cache','FLTK_BACKEND':'wayland','WAYLAND_DISPLAY':wayland.socket,'DOME_WEBXR_SOCKET':str(runtime/('xr.sock' if host else 'no-xr.sock')),'LIBGL_ALWAYS_SOFTWARE':'1','RUST_LOG':'warn'}
 for k in ('WAYLAND_SOCKET','DISPLAY','DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE'):env.pop(k,None)
 log=(out/'browser.log').open('w');logs.append(log);child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(runtime/'profile'),f'http://127.0.0.1:{site.server_port}/'],env=env,stdout=log,stderr=log)
 wait(lambda:seen('ready') and latest() and len(latest()['latest'])==3 and all(x and x['frames']>1 for x in latest()['latest']))
 check(all(x['length']==0 for x in latest()['latest']),'Inline canvases initially have no screen input source')
 window=wait(lambda:next((w for w in wayland.windows() if w.value()['client_pid']==child.pid and w.get_wm_name()=='Inline pointer integration — Dome Browser'),None));window.set_input_focus()
 before=mark();move();button(True)
 first=wait(lambda:selection(0,'selectstart',before))[-1];wait(lambda:selection(1,'selectstart',before))
 check(descriptor(first['data']['source']),'A native canvas press creates a screen source without controller capabilities')
 check(direction_close(first['data']['pose'],[0,0,-1]),'Canvas center produces a forward viewer ray')
 check(not selection(2,'selectstart',before),'Other inline output canvases do not receive this pointer')
 check(first['data']['trusted'] and first['data']['frameSession'] and first['data']['viewerError']=='InvalidStateError','Selection events are trusted with an active non-animation frame')
 check(math.isfinite(first['data']['time']) and first['data']['time']>0,'Input frame time uses the document performance timeline')
 wait(lambda:all(x['length']==1 for x in latest()['latest'][:2]));check(True,'Held pointers are available in inline animation frames')
 move(.75);sample=wait(lambda:latest()['latest'][0]['poses'][0]['pose'] if direction_close(latest()['latest'][0]['poses'][0]['pose'],[1.133333333,0,-1]) else None)
 # Border/padding means 75% of the outer box lies beyond 75% of content.
 check(bool(sample),'Held pointer motion updates the content-space ray')
 move();button(False);end=wait(lambda:selection(0,'selectend',before))[-1]
 wait(lambda:latest()['latest'][0]['length']==0 and latest()['latest'][1]['length']==0)
 wait(lambda:synced_heartbeat(end['order']))
 check(len(selection(0,'select',before))==1 and len(selection(0,'selectend',before))==1,'A completed mouse gesture selects and ends exactly once')
 selected=selection(0,'select',before)[0]['data'];ended=end['data']
 check(selected['frameId']==ended['frameId'] and selected['time']==ended['time'],'Selection completion events share their action frame and timestamp')
 ids=first['data']['source']['id'];ordered=[e['type']+':'+e['data'].get('name','') for e in sorted(events,key=lambda e:e['order']) if e['order']>before and (e['type']=='dom' or e['type']=='selection' and e['data']['index']==0 or e['type']=='sources' and e['data']['index']==0)]
 expected=['dom:pointerdown','sources:','selection:selectstart','selection:select','dom:click','selection:selectend','sources:','dom:pointerup']
 check(ordered==expected,'Transient XR selection brackets click and removes its source before pointerup')
 check(all(v is None for v in latest()['latest'][0]['retired']),'Retired target-ray spaces stop producing poses')
 before=mark();command('synthetic');time.sleep(.2);check(not [e for e in seen('selection') if e['order']>before],'Script-dispatched pointer events and click do not create XR input')
 before=mark();move();button(True,2);button(False,2);time.sleep(.2);check(not [e for e in seen('selection') if e['order']>before],'Auxiliary mouse buttons do not activate the inline primary action')
 before=mark();button(True);again=wait(lambda:selection(0,'selectstart',before))[-1];check(again['data']['source']['id']!=ids,'A new gesture gets a new source object')
 move(.5,.5,'other');wait(lambda:selection(0,'selectend',before));button(False)
 check(not selection(0,'select',before),'Leaving the output canvas cancels without selecting')
 wait(lambda:synced_heartbeat(selection(0,'selectend',before)[-1]['order']))
 before=mark();move(.5,.5,'other');button(True);alternate=wait(lambda:selection(2,'selectstart',before))[-1]
 check(descriptor(alternate['data']['source']) and direction_close(alternate['data']['pose'],[0,0,-1]),'A different output canvas creates its own valid screen source')
 check(not selection(0,'selectstart',before) and not selection(1,'selectstart',before),'A press on the alternate canvas stays isolated from the original sessions')
 button(False);wait(lambda:selection(2,'selectend',before));wait(lambda:synced_heartbeat(selection(2,'selectend',before)[-1]['order']))
 # HTTP delivery may complete before the next inline animation callback.
 wait(lambda:latest()['latest'][2]['length']==0)
 check(len(selection(2,'select',before))==1 and latest()['latest'][2]['length']==0,'The alternate canvas completes and retires its own gesture')
 command('rotate');wait(lambda:latest()['rect']['width']>0);move();before=mark();button(True);rotated=wait(lambda:selection(0,'selectstart',before))[-1]
 check(direction_close(rotated['data']['pose'],[0,0,-1]),'CSS rotation and scale preserve the canvas-center ray');button(False);wait(lambda:selection(0,'selectend',before));command('reset')
 before=mark();move(.75);button(True);wait(lambda:selection(0,'selectstart',before));command('fov')
 wait(lambda:latest()['latest'][0]['poses'] and direction_close(latest()['latest'][0]['poses'][0]['pose'],[1.133333333/math.sqrt(3),0,-1]))
 check(direction_close(latest()['latest'][1]['poses'][0]['pose'],[1.133333333,0,-1]),'Each inline session uses its own field of view for the same pointer')
 command('swap');wait(lambda:selection(0,'selectend',before));check(not selection(0,'select',before),'Changing the output canvas cancels its old gesture')
 check(latest()['latest'][1]['length']==1,'Another session on the original canvas retains its gesture')
 button(False);ended=wait(lambda:selection(1,'selectend',before))[-1];wait(lambda:synced_heartbeat(ended['order']))
 check(len(selection(1,'select',before))==1,'A layer change in one session does not cancel selection in another')
 command('restore-output');wait(lambda:latest()['latest'][0]['length']==0)
 if host:
  command('prepare-immersive');move();before=mark();button(True);wait(lambda:selection(0,'selectstart',before));key('space')
  consent=wait(lambda:next((w for w in wayland.windows() if w.get_wm_name()=='Enter immersive WebXR'),None));consent.set_input_focus();key('Return')
  ready=wait(lambda:seen('immersive-ready'))[-1];wait(lambda:selection(0,'selectend',before) and selection(1,'selectend',before));button(False)
  check(ready['data']['lengths']==[0,0,0],'Immersive activation retires every held inline source')
  wait(lambda:synced_heartbeat(ready['order']));check(not selection(0,'select',before) and not selection(1,'select',before),'Immersive activation cancels inline gestures without selecting')
  paused=[x['frames'] for x in latest()['latest']];after=mark();wait(lambda:len([e for e in seen('heartbeat') if e['order']>after])>=2)
  check(paused==[x['frames'] for x in latest()['latest']],'Inline animation remains paused while immersive mode is active')
  command('end-immersive');wait(lambda:all(x['length']==0 for x in latest()['latest']) and all(x['frames']>old for x,old in zip(latest()['latest'],paused)))
  check(True,'Inline sessions resume without stale sources after immersive shutdown');window.set_input_focus()
 move();thread_samples.append({'stage':'before-32-gestures','threads':len(list((Path('/proc')/str(child.pid)/'task').iterdir()))})
 cycle_sources=[]
 for cycle in range(32):
  after=mark();button(True);started=wait(lambda:selection(0,'selectstart',after))[-1];cycle_sources.append(started['data']['source']['id']);button(False)
  wait(lambda:selection(0,'selectend',after) and selection(1,'selectend',after));wait(lambda:all(x['length']==0 for x in latest()['latest'][:2]))
  wait(lambda:synced_heartbeat(max(selection(0,'selectend',after)[-1]['order'],selection(1,'selectend',after)[-1]['order'])))
  assert len(selection(0,'select',after))==1 and len(selection(1,'select',after))==1
 thread_samples.append({'stage':'after-32-gestures','threads':len(list((Path('/proc')/str(child.pid)/'task').iterdir()))})
 check(len(set(cycle_sources))==32,'Repeated native gestures retire their sources and allocate fresh identities')
 check(all(x['length']==0 for x in latest()['latest']),'No screen sources remain after repeated pointer lifecycles')
 before=mark();move();button(True);wait(lambda:selection(0,'selectstart',before));key('t',('Control_L',));wait(lambda:selection(0,'selectend',before));button(False)
 check(not selection(0,'select',before),'Hiding the page cancels a held inline pointer without selecting')
 check(selection(0,'selectend',before)[-1]['data']['poseError']=='SecurityError','Hidden cancellation events do not expose an input pose')
 wait(lambda:synced_heartbeat(selection(0,'selectend',before)[-1]['order']))
 key('Tab',('Control_L','Shift_L'));wait(lambda:latest()['visibility']=='visible')
 command('end-on-start');move();before=mark();button(True);wait(lambda:seen('reentrant-end'));button(False);wait(lambda:selection(1,'selectend',before))
 wait(lambda:synced_heartbeat(selection(1,'selectend',before)[-1]['order']))
 check(len(selection(0,'selectend',before))==1 and not selection(0,'select',before),'Reentrant session end cancels once without selecting')
 check(len([e for e in seen('ended') if e['data']['index']==0])==1,'Repeated end calls resolve through one shutdown')
 check(len(selection(1,'select',before))==1,'Other sessions sharing the canvas continue after one session ends')
 before=mark();move();button(True);wait(lambda:selection(1,'selectstart',before));command('remove');wait(lambda:selection(1,'selectend',before));button(False)
 wait(lambda:synced_heartbeat(selection(1,'selectend',before)[-1]['order']))
 check(not selection(1,'select',before),'Removing the output canvas cancels its active pointer')
 check(not seen('error'),'Inline pointer lifecycle finishes without script errors')
 wayland.api('/api/action',{'op':'close','id':window.id});check(child.wait(timeout=10)==0,'Browser exits cleanly');child=None;completed=True
finally:
 if child is not None and child.poll() is None:child.terminate();child.wait(timeout=10)
 if host is not None and host.poll() is None:host.stdin.write('{"quit":true}\n');host.stdin.flush();host.wait(timeout=5)
 if wayland:wayland.close()
 if connection:connection.close()
 if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
 if site:site.shutdown();site.server_close()
 for log in logs:log.close()
 (out/'report.json').write_text(json.dumps({'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'checks':checks,'events':sorted(events,key=lambda e:e['order']),'thread_samples':thread_samples,'passed':completed and all(c['passed'] for c in checks),'failure':None if completed else str(sys.exc_info()[1]),'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'fixture_sha256':hashlib.sha256(fixture.read_bytes()).hexdigest(),'host_fixture_sha256':hashlib.sha256(args.host_fixture.read_bytes()).hexdigest() if args.host_fixture else None,'scope':'Real Servo DOM; native mouse events in a private nested Wayland session; optional synthetic native XR transport. No physical headset or touchscreen validation.','goal_status':'active'},indent=2)+'\n');paths.close()
if not completed or not all(c['passed'] for c in checks):raise SystemExit(1)
