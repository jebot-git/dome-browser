#!/usr/bin/env python3
"""Real Servo inline canvas and immersive coexistence in a private Wayland session."""
from pathlib import Path
import argparse,contextlib,hashlib,http.server,json,os,resource,select,signal,subprocess,sys,tempfile,threading,time
signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_INLINE_TEST_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_INLINE_TEST_BUS':'1'})
from private_wayland import PrivateWayland
from PIL import Image
from Xlib import X,XK,display
from Xlib.ext import xtest
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary',type=Path);parser.add_argument('fixture',type=Path);parser.add_argument('output',type=Path)
parser.add_argument('--no-host',action='store_true')
parser.add_argument('--page-lifecycle',action='store_true')
parser.add_argument('--native-visibility',action='store_true');args=parser.parse_args()
if args.native_visibility:args.page_lifecycle=True
if args.page_lifecycle:args.no_host=True
root=Path(__file__).resolve().parent;binary=args.binary.resolve();out=args.output.resolve();out.mkdir(exist_ok=False,parents=True,mode=0o700)
events=[];checks=[];logs=[];child=host=xvfb=wayland=connection=site=None;completed=False;acks=[];commands=[];thread_samples=[]
native_stop=threading.Event();native_heartbeat=None;native_errors=[]
fixture_page=root/('webxr-inline-lifecycle.html' if args.page_lifecycle else 'webxr-inline.html')
temporary_paths=contextlib.ExitStack()
def check(ok,name):
 checks.append({'name':name,'passed':bool(ok)});print(('PASS ' if ok else 'FAIL ')+name,flush=True)
 if not ok:raise AssertionError(name)
def seen(kind):return sorted((e for e in events if e['type']==kind),key=lambda e:e['order'])
def checkpoint(kind,page=None):
 values=[e for e in seen(kind) if page is None or e['data'].get('page')==page]
 if not values:return None
 last=values[-1]
 return last if set(range(1,last['order']+1)).issubset({e['order'] for e in events if page is None or e['data'].get('page')==page}) else None
def wait(fn,seconds=25):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  if seen('error'):raise AssertionError(seen('error')[-1]['data']['message'])
  value=fn()
  if value:return value
  if child and child.poll() is not None:raise AssertionError('Browser exited: '+str(child.returncode))
  time.sleep(.05)
 raise AssertionError('Private inline fixture timed out')
def stats():
 host.stdin.write(json.dumps({'ack':len(acks)+1})+'\n');host.stdin.flush()
 assert select.select([host.stdout],[],[],3)[0],'Native host did not acknowledge'
 response=json.loads(host.stdout.readline());acks.append(response);return response['stats']
try:
 with contextlib.nullcontext(temporary_paths.enter_context(tempfile.TemporaryDirectory(prefix='dome-inline-dom-'))) as folder:
  runtime=Path(folder);runtime.chmod(0o700)
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*a):pass
   def do_GET(self):
    body=fixture_page.read_bytes();self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
   def do_POST(self):
    event=json.loads(self.rfile.read(int(self.headers['Content-Length'])));events.append(event)
    if event['type']=='check':print(('PASS ' if event['data']['passed'] else 'FAIL ')+event['data']['name'],flush=True)
    (out/'events.jsonl').write_text('\n'.join(json.dumps(e) for e in events)+'\n')
    if args.page_lifecycle:
     body=json.dumps(commands.pop(0) if commands and event['type']=='heartbeat' else None).encode();self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers()
     try:self.wfile.write(body)
     except (BrokenPipeError,ConnectionResetError):pass # Normal navigation/close cancellation.
    else:self.send_response(204);self.end_headers()
  site=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=site.serve_forever,daemon=True).start()
  if not args.no_host:
   log=(out/'native-host.log').open('w');logs.append(log)
   host=subprocess.Popen([str(args.fixture.resolve()),str(runtime/'xr.sock')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1)
   assert select.select([host.stdout],[],[],5)[0];assert json.loads(host.stdout.readline())['ready']
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
  with os.fdopen(rd) as pipe:screen=':'+pipe.readline().strip()
  connection=display.Display(screen);wayland=PrivateWayland(connection,screen,runtime,out)
  env={**os.environ,'HOME':folder,'XDG_RUNTIME_DIR':folder,'XDG_CONFIG_HOME':folder+'/config','XDG_DATA_HOME':folder+'/data','XDG_STATE_HOME':folder+'/state','XDG_CACHE_HOME':folder+'/cache','FLTK_BACKEND':'wayland','WAYLAND_DISPLAY':wayland.socket,'DOME_WEBXR_SOCKET':str(runtime/'xr.sock'),'LIBGL_ALWAYS_SOFTWARE':'1','RUST_LOG':'warn'}
  for k in ['WAYLAND_SOCKET','DISPLAY','DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE']:env.pop(k,None)
  log=(out/'browser.log').open('w');logs.append(log)
  child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(runtime/'profile'),f'http://127.0.0.1:{site.server_port}/'+('' if args.no_host else '?coexist')],env=env,stdout=log,stderr=log)
  wait(lambda:checkpoint('ready','/' if args.page_lifecycle else None) if args.page_lifecycle or not args.no_host else checkpoint('done'))
  check(not seen('error'),'Inline canvas checks finish without script errors')
  if args.no_host:check(not seen('support')[-1]['data']['immersive'],'No-headset run does not advertise immersive support')
  w=wait(lambda:next((w for w in wayland.windows() if w.value()['client_pid']==child.pid and w.get_wm_name()=='WebXR inline integration — Dome Browser'),None))
  if args.page_lifecycle:
   def key(name,mods=()):
    for mod in mods:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(XK.string_to_keysym(mod)))
    code=connection.keysym_to_keycode(XK.string_to_keysym(name));xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
    for mod in reversed(mods):xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(XK.string_to_keysym(mod)))
    connection.sync()
   initial_child=wait(lambda:next((e for e in seen('child') if e['data']['inlineChild']=='initial-child' and e['data']['kind']=='ready'),None))
   check(initial_child['data']['visibilityEntries']==[{'name':'visible','startTime':0,'duration':0}],'Child frames record their own initial visible performance entry')
   initial_observer=wait(lambda:seen('initial-observer'))[-1]['data'];check(initial_observer['entries']==[{'name':'visible','startTime':0}],'Buffered performance observers receive the initial visibility entry')
   wait(lambda:seen('heartbeat') and seen('heartbeat')[-1]['data']['frames']>2)
   check(all(s=='visible' for s in seen('ready')[0]['data']['sessions']),'Visible documents start with visible inline sessions')
   w.set_input_focus();key('t',('Control_L',))
   hidden=wait(lambda:next((e for e in seen('document') if e['data']['document']=='hidden'),None))
   sample=hidden['data'];check(sample['document']=='hidden','Switching tabs updates document visibility')
   check(sample['sessions']==['hidden','hidden'],'Inline session visibility mirrors its hidden document')
   sample=wait(lambda:next((e['data'] for e in seen('heartbeat') if e['order']>hidden['order']),None))
   check(sample['frames']==hidden['data']['frames'],'Hidden inline sessions suspend animation callbacks')
   check(sample['windowFrames']<=hidden['data']['windowFrames']+1,'Hidden page animation callbacks stop after the in-flight frame')
   hidden_child=wait(lambda:next((e for e in seen('child') if e['data']['inlineChild']=='initial-child' and e['data']['document']=='hidden' and e['data']['session']=='hidden'),None))
   check(True,'Existing child frames inherit the tab visibility transition')
   commands.append({'op':'create-hidden-child'});created=wait(lambda:next((e for e in seen('child') if e['data']['inlineChild']=='hidden-child' and e['data']['kind']=='ready'),None))
   check(created['data']['document']=='hidden' and created['data']['session']=='hidden' and created['data']['frames']==0,'A child created in a hidden tab starts hidden without inline callbacks')
   check(created['data']['visibilityEntries']==[{'name':'hidden','startTime':0,'duration':0}],'Background-created child frames record an initial hidden performance entry')
   thread_samples.append({'stage':'before-hidden-session-cycles','threads':len(list((Path('/proc')/str(child.pid)/'task').iterdir()))})
   commands.append({'op':'cycle-sessions'});cycles=wait(lambda:seen('cycles-ended'))[-1]['data'];check(cycles['cycles']==32 and cycles['document']=='hidden','Repeated inline creation and shutdown remain responsive in a hidden tab')
   time.sleep(.1);thread_samples.append({'stage':'after-hidden-session-cycles','threads':len(list((Path('/proc')/str(child.pid)/'task').iterdir()))})
   commands.append({'op':'end-secondary'});wait(lambda:seen('secondary-ended'));check(len([e for e in seen('ended') if e['data']['index']==1])==1,'An inline session ends once while its tab is hidden')
   key('Tab',('Control_L','Shift_L'));wait(lambda:any(e['data']['document']=='visible' for e in seen('document')))
   wait(lambda:seen('heartbeat')[-1]['data']['document']=='visible' and seen('heartbeat')[-1]['data']['frames']>sample['frames'])
   check(seen('heartbeat')[-1]['data']['sessions'][0]=='visible','Returning to the tab restores inline visibility and callbacks')
   shown_child=wait(lambda:next((e for e in seen('child') if e['data']['inlineChild']=='hidden-child' and e['data']['document']=='visible' and e['data']['session']=='visible'),None))
   check(True,'The background-created child becomes visible with its parent')
   restored_child=wait(lambda:next((e for e in seen('child') if e['data']['inlineChild']=='initial-child' and e['data']['kind']=='session' and e['data']['document']=='visible'),None))
   check(restored_child['data']['frames']==hidden_child['data']['frames'] and shown_child['data']['frames']==0,'Child inline callbacks stay paused for the entire hidden interval')
   transitions=[e['data']['sessions'][0] for e in seen('session') if e['data']['index']==0]
   check(transitions==['hidden','visible'] and all(e['data']['eventSession'] for e in seen('session')),'Inline visibility transitions dispatch one XRSessionEvent per change')
   entries=seen('heartbeat')[-1]['data']['visibilityEntries'];check([e['name'] for e in entries]==['visible','hidden','visible'] and entries[0]['startTime']==0 and 0<entries[1]['startTime']<=entries[2]['startTime'],'Visibility timing records the ordered hide and restore transitions')
   notified=wait(lambda:seen('heartbeat')[-1]['data']['visibilityNotifications'] if len(seen('heartbeat')[-1]['data']['visibilityNotifications'])>=2 else None)
   check(notified==entries[1:],'Unbuffered visibility observers receive each new transition once')
   if args.native_visibility:
    def transition(action,state,name):
     after=max(e['order'] for e in events)
     wayland.api('/api/action',action)
     event=wait(lambda:next((e for e in seen('document') if e['order']>after and e['data']['document']==state),None))
     check(event['data']['sessions'][0]==state,name)
     sample=wait(lambda:next((e['data'] for e in seen('heartbeat') if e['order']>event['order']),None))
     if state=='hidden':check(sample['frames']==event['data']['frames'],name+' stops inline animation callbacks')
     else:check(wait(lambda:seen('heartbeat')[-1]['data']['frames']>event['data']['frames']),name+' restarts inline animation callbacks')
     return event
    def remains_visible(name):
     after=max(e['order'] for e in events)
     samples=wait(lambda:[e for e in seen('heartbeat') if e['order']>after] if len([e for e in seen('heartbeat') if e['order']>after])>=2 else None)
     check(all(e['data']['document']=='visible' and e['data']['sessions'][0]=='visible' for e in samples),name)
    transition({'op':'minimize','id':w.id},'hidden','Native minimization hides the document and its inline sessions')
    transition({'op':'focus','id':w.id},'visible','Native restore resumes the same document and inline sessions')
    transition({'op':'switch_workspace','workspace':1},'hidden','Switching native workspaces hides the browser document')
    transition({'op':'switch_workspace','workspace':0},'visible','Returning to the native workspace restores document visibility')
    def heartbeat():
     while not native_stop.is_set():
      try:wayland.api('/api/xr-session',{'id':4812,'visible':True,'focused':True,'frame_period_ns':11111111})
      except Exception as e:native_errors.append(str(e));return
      native_stop.wait(.15)
    native_heartbeat=threading.Thread(target=heartbeat,daemon=True);native_heartbeat.start();wait(lambda:wayland.api('/api/xr-control')['visible'])
    wayland.api('/api/action',{'op':'window_visibility','id':w.id,'desktop':False,'xr':True});remains_visible('An XR-only browser document remains visible while XR output is active')
    transition({'op':'xr_presentation','desktop':False},'hidden','Hiding XR desktop presentation pauses an XR-only browser document')
    transition({'op':'window_visibility','id':w.id,'desktop':True,'xr':False},'visible','A monitor-visible browser resumes even when XR desktop presentation is hidden')
    wayland.api('/api/action',{'op':'xr_presentation','desktop':True})
    wayland.api('/api/action',{'op':'window_visibility','id':w.id,'desktop':False,'xr':True});remains_visible('XR transfer preserves a visible browser document')
    native_stop.set();native_heartbeat.join(timeout=3);assert not native_errors,native_errors
    wayland.api('/api/xr-session',{'id':4812,'visible':False})
    wait(lambda:next(v for v in wayland.scene()['windows'] if v['id']==w.id)['desktop_visible']);remains_visible('Desktop fallback keeps the browser visible when XR output disconnects')
    transition({'op':'window_visibility','id':w.id,'desktop':False,'xr':True},'hidden','An XR-only browser pauses when no XR output is available')
    transition({'op':'window_visibility','id':w.id,'desktop':True,'xr':False},'visible','Returning the browser to the monitor resumes its document')
   commands.append({'op':'navigate'});wait(lambda:checkpoint('ready','/replacement'))
   check(True,'Navigation starts the replacement inline page cleanly')
   check(not seen('error'),'Page lifecycle completes without script errors')
   events.append({'order':max(e['order'] for e in events)+1,'type':'done','data':{'driver':True}})
  if host:
   check(stats()['sessions']==0,'Inline rendering never acquires a native XR lease')
   def capture():
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    return Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
   def painted():
    image=capture();return sum(1 for r,g,b in image.crop((0,90,image.width,image.height-28)).getdata() if b-r>50 and b-g>40)>500
   check(wait(painted),'Document activation button is painted before the trusted click');capture().save(out/'before-activation.png')
   data=seen('ready')[-1]['data'];g=w.get_geometry();ratio=data['ratio'];rect=data['start'];top=g.height-data['viewport'][1]*ratio-28
   check(abs(data['viewport'][0]*ratio-g.width)<2,'DOM and private native window geometry agree')
   w.set_input_focus();xtest.fake_input(connection,X.MotionNotify,x=g.x+int((rect['x']+rect['width']/2)*ratio),y=g.y+int(top+(rect['y']+rect['height']/2)*ratio));connection.sync();time.sleep(.05)
   xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync()
   consent=wait(lambda:next((v for v in wayland.windows() if v.get_wm_name()=='Enter immersive WebXR'),None));consent.set_input_focus()
   key=connection.keysym_to_keycode(XK.string_to_keysym('Return'));xtest.fake_input(connection,X.KeyPress,key);xtest.fake_input(connection,X.KeyRelease,key);connection.sync()
   wait(lambda:checkpoint('done'));check(stats()['sessions']==1,'Only the requested immersive session acquires a native XR lease')
  check(bool(seen('done')) and all(e['data']['passed'] for e in seen('check')),'All browser-level inline assertions pass')
  wayland.api('/api/action',{'op':'close','id':w.id});check(child.wait(timeout=10)==0,'Browser exits cleanly');child=None;completed=True
finally:
 native_stop.set()
 if native_heartbeat:native_heartbeat.join(timeout=3)
 if child is not None and child.poll() is None:child.terminate();child.wait(timeout=10)
 if host is not None and host.poll() is None:host.stdin.write('{"quit":true}\n');host.stdin.flush();host.wait(timeout=5)
 if wayland:wayland.close()
 if connection:connection.close()
 if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
 if site:site.shutdown();site.server_close()
 for log in logs:log.close()
 (out/'report.json').write_text(json.dumps({'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'checks':checks+[e['data'] for e in seen('check')],'events':sorted(events,key=lambda e:e['order']),'host_acks':acks,'thread_samples':thread_samples,'passed':completed and bool(checks) and all(c['passed'] for c in checks) and all(e['data']['passed'] for e in seen('check')),'failure':None if completed else str(sys.exc_info()[1]),'no_host':args.no_host,'page_lifecycle':args.page_lifecycle,'native_visibility':args.native_visibility,'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'fixture_sha256':hashlib.sha256(fixture_page.read_bytes()).hexdigest(),'scope':'Real Servo DOM and canvas; private nested Wayland and optional synthetic native XR transport. Native visibility tests simulate XR output availability. No physical headset or live desktop changes.','goal_status':'active'},indent=2)+'\n')
 temporary_paths.close()
