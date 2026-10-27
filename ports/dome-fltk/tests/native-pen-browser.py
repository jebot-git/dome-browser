#!/usr/bin/env python3
"""Real native Wayland pen input into the shipping browser, DOM and inline XR."""
import argparse,http.server,json,math,os,resource,signal,subprocess,sys,tempfile,threading,time
from pathlib import Path
from wayland_touch_fixture import TouchWayland
from Xlib import X,XK
from Xlib.ext import xtest
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_PEN_BROWSER_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_PEN_BROWSER_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--compositor',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--diagnostic',action='store_true');a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);checks=[];snapshots=[];commands=[];acks=[];failure=None
child=fixture=server=log=None
def check(value,name):
 checks.append({'name':name,'passed':bool(value)});print(('PASS ' if value else 'FAIL ')+name,flush=True)
 if not value and not a.diagnostic:raise AssertionError(name)
def latest():
 s=snapshots[max(range(len(snapshots)),key=lambda i:(snapshots[i]['frames'],i))] if snapshots else None
 if s:assert not s.get('error'),s['error']
 return s
def wait(fn):
 end=time.monotonic()+20
 while time.monotonic()<end:
  if child:assert child.poll() is None,'Browser exited unexpectedly'
  value=fn()
  if value:return value
  time.sleep(.025)
 raise AssertionError('Native browser pen fixture timed out')
def events(start=0,index=0):return [e for e in latest()['events'][start:] if e.get('index',index)==index]
def named(name,start=0,index=0):return [e for e in events(start,index) if e['name']==name]
def settle():
 before=latest()['frames'];wait(lambda:latest()['frames']>before+4)
def zero():wait(lambda:latest()['lengths']==[0,0]);settle()
def mark():settle();return len(latest()['events'])
def command(op):commands.append(op);wait(lambda:op in acks);acks.remove(op);settle()
def close_ray(ray,want):
 n=math.sqrt(sum(x*x for x in want));return ray and max(abs(x-y/n) for x,y in zip(ray,want))<.03
try:
 with tempfile.TemporaryDirectory(prefix='dome-browser-pen-') as folder:
  runtime=Path(folder);runtime.chmod(0o700);fixture=TouchWayland(a.compositor.resolve(),runtime,out)
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*_):pass
   def do_GET(self):
    if self.path=='/commands':body=json.dumps(commands[:]).encode();commands.clear();kind='application/json'
    else:body=Path(__file__).with_suffix('.html').read_bytes();kind='text/html'
    self.send_response(200);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
   def do_POST(self):
    value=json.loads(self.rfile.read(int(self.headers['Content-Length'])));(acks if self.path=='/ack' else snapshots).append(value);self.send_response(204);self.end_headers()
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=server.serve_forever,daemon=True).start()
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')};env.update(HOME=folder,XDG_RUNTIME_DIR=folder,XDG_CONFIG_HOME=folder+'/config',XDG_DATA_HOME=folder+'/data',XDG_STATE_HOME=folder+'/state',XDG_CACHE_HOME=folder+'/cache',FLTK_BACKEND='wayland',WAYLAND_DISPLAY=fixture.wayland.socket,LIBGL_ALWAYS_SOFTWARE='1',DOME_WEBXR_SOCKET=folder+'/no-host.sock',RUST_LOG='warn')
  for name in ('DISPLAY','WAYLAND_SOCKET','FLTK_SCALING_FACTOR'):env.pop(name,None)
  log=(out/'browser.log').open('w');child=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',folder+'/profile',f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log)
  wait(lambda:latest() and latest()['ready'] and latest()['frames']>4)
  window=fixture.wayland.wait(lambda:next((w for w in fixture.wayland.windows() if w.value()['client_pid']==child.pid),None));window.set_input_focus()
  def send(action,point=None,slot=0,**axes):
   if point is not None:
    g=window.get_geometry();s=latest();ratio=s['ratio'];top=g.height-s['viewport'][1]*ratio-28;point=[g.x+point[0]*ratio,g.y+top+point[1]*ratio]
   fixture.command(action,slot,point,**axes)
   if not action.startswith('tablet-'):fixture.command('frame')
  def key(name,mods=()):
   c=fixture.connection
   for item in (*mods,name):xtest.fake_input(c,X.KeyPress,c.keysym_to_keycode(XK.string_to_keysym(item)))
   for item in (name,*reversed(mods)):xtest.fake_input(c,X.KeyRelease,c.keysym_to_keycode(XK.string_to_keysym(item)))
   c.sync()
  check(latest()['lengths']==[0,0],'Two real browser inline sessions start without a native XR host')
  start=mark();send('tablet-in',[205,195],pressure=.3,tilt=[30,0],rotation=45,slider=-.25)
  wait(lambda:named('pointermove',start));settle();hover=named('pointermove',start)[-1];pid=hover['id']
  check(hover['kind']=='pen' and hover['pointer'] and hover['trusted'],'Native hover is a trusted pen PointerEvent')
  check(hover['buttons']==0 and hover['pressure']==0 and hover['button']==-1,'Pen hover has no active buttons or pressure')
  check(named('pointerover',start) and named('pointerenter',start) and latest()['hover'],'Pen hover enters its target and updates CSS hover')
  check(not [e for e in events(start) if e['name'].startswith('touch')] and latest()['lengths']==[0,0],'Pen hover creates neither TouchEvents nor XR sources')
  command('hover-capture');send('tablet-move',[206,195]);settle()
  check(latest()['hoverCaptured'] is False and latest()['hoverCaptureError'] is None,'Hover pointer capture is ignored without throwing for an active pen')
  command('no-hover-capture');send('tablet-move',[205,195]);settle()
  check(not latest()['activation'],'Hover and callback setup do not grant user activation')
  command('prevent');start=mark();send('tablet-down')
  wait(lambda:named('selectstart',start));wait(lambda:latest()['lengths']==[1,1]);down=named('pointerdown',start)[0]
  check(down['id']==pid and down['primary'] and down['button']==0 and down['buttons']==1,'Contact retains its hover pointer identity and primary tip state')
  check(abs(down['pressure']-.3)<.0001 and down['tilt']==[30,0] and down['twist']==45 and abs(down['tangential']+.25)<.0001,'Measured native pressure, tilt, twist and tangential pressure reach script')
  check(abs(down['altitude']-math.pi/3)<.0001 and abs(down['azimuth'])<.0001,'Tilt and orientation angles describe the same physical direction')
  check(down['captured'] and not named('gotpointercapture',start),'Implicit pen capture is pending during pointerdown and awaits the next event')
  send('tablet-move',[205,195]);wait(lambda:named('gotpointercapture',start));settle()
  check(named('gotpointercapture',start)[0]['target']=='canvas','The next native pen sample applies pending implicit capture')
  captured=named('gotpointercapture',start)[0]
  check(captured['buttons']==1 and abs(captured['pressure']-down['pressure'])<.0001,
        'Capture events retain the current pen buttons and pressure')
  check(len(named('synthetic-select',start))==1 and not named('synthetic-select',start)[0]['activation'],'An author-dispatched XR select event does not grant activation')
  check(close_ray(named('selectstart',start)[0]['ray'],[0,0,-1]),'Canvas-center pen contact produces a forward XR ray')
  check(not [e for e in events(start) if e['name'].startswith('touch')],'Native pen contact does not manufacture touchscreen events')
  send('tablet-up');zero();click=named('click',start)[0];selected=named('select',start)[0];ended=named('selectend',start)[0]
  check(click['pointer'] and click['kind']=='pen' and click['id']==pid and click['pressure']==0,'Pen click is a PointerEvent with the original pen identity')
  check(selected['frame']==ended['frame'] and selected['time']==ended['time'],'Pen XR completion shares one action frame and timestamp')
  check(selected['activation'] and click['activation'],'Trusted XR select activates the document before its deferred pen click')
  names=[e['name'] for e in events(start) if e['name'] in ('pointerdown','selectstart','select','click','selectend','pointerup')]
  check(names==['pointerdown','selectstart','select','click','selectend','pointerup'],'XR selection brackets pen click and retires before pointerup')
  check(named('lostpointercapture',start) and not named('pointerout',start) and latest()['hover'],'Tip release releases capture while the pen remains hovering')
  command('allow');start=mark();send('tablet-move',[205,195],wheel=[12.5,2]);wait(lambda:named('wheel',start));settle()
  check(abs(named('wheel',start)[-1]['y']-12.5/15)<.0001 and named('wheel',start)[-1]['mode']==1 and latest()['lengths']==[0,0],
        'Native pen wheel steps reach a line-mode WheelEvent without creating an XR action')
  start=mark();send('tablet-move',[205,195],wheel=[-7.5,0]);wait(lambda:named('wheel',start));settle()
  check(abs(named('wheel',start)[-1]['y']+.5)<.0001,'Continuous reverse pen wheel motion retains fractional line deltas')
  start=mark();send('tablet-move',[205,195],wheel=[0,1]);settle()
  check(not named('wheel',start),'A discrete report for an already delivered rotation is not counted twice')
  start=mark();send('tablet-down');wait(lambda:named('selectstart',start));send('tablet-move',[280,195],pressure=.7)
  wait(lambda:latest()['latest'][0]['rays'] and close_ray(latest()['latest'][0]['rays'][0]['ray'],[.75,0,-1]))
  check(abs(named('pointermove',start)[-1]['pressure']-.7)<.0001,'Held pen motion updates both XR pose and DOM pressure')
  send('tablet-up');zero();check(len(named('select',start))==1 and not named('click',start),'A moved pen action completes without a tap click')
  send('tablet-move',[205,195]);start=mark();send('tablet-in',[280,195],slot=1);send('tablet-down');send('tablet-down',slot=1)
  wait(lambda:latest()['lengths']==[2,2]);settle();downs=named('pointerdown',start)
  check(len(downs)==2 and {e['primary'] for e in downs}=={True,False} and len({e['id'] for e in downs})==2,'Two pen tools have distinct pointers and stable primary status')
  send('tablet-up');send('tablet-move',[260,195],slot=1);send('tablet-up',slot=1);zero()
  check(len(named('selectstart',start))==1 and len(named('select',start))==1,'An auxiliary pen never promotes into the XR primary action')
  send('tablet-out',slot=1);start=mark();send('tablet-down');send('down',[250,195],slot=1)
  wait(lambda:latest()['lengths']==[2,2]);settle();downs=named('pointerdown',start)
  check({e['kind'] for e in downs}=={'pen','touch'} and len({e['id'] for e in downs})==2,'Native touch and pen identifiers cannot collide in the document')
  send('up',slot=1);send('tablet-up');zero()
  start=mark();send('tablet-button',button=331,pressed=True);settle()
  check(named('pointerdown',start)[0]['button']==2 and named('pointerdown',start)[0]['buttons']==2 and latest()['lengths']==[0,0],'A hovering barrel button is secondary input without an XR source')
  check(named('contextmenu',start)[0]['kind']=='pen','Pen contextmenu retains pen pointer metadata')
  send('tablet-down',pressure=.4);wait(lambda:named('selectstart',start));send('tablet-up');wait(lambda:latest()['lengths']==[0,0]);send('tablet-button',button=331,pressed=False);settle()
  check(len(named('pointerdown',start))==1 and len(named('pointerup',start))==1 and len(named('select',start))==1,'Chorded tip and barrel changes use pointermove with one active-buttons lifetime')
  send('tablet-out');start=mark();send('tablet-in',[205,195],slot=2,eraser=True,pressure=.5);send('tablet-down',slot=2,eraser=True)
  wait(lambda:latest()['lengths']==[1,1]);settle();down=named('pointerdown',start)[0]
  check(down['button']==5 and down['buttons']==32 and down['primary'],'An eraser is a distinct button on the primary pen pointer')
  check(not named('selectstart',start),'Eraser contact is auxiliary to the XR primary action')
  send('tablet-up',slot=2,eraser=True);zero();check(not named('select',start) and not named('click',start),'An eraser release cannot select or activate a primary click')
  send('tablet-out',slot=2,eraser=True);send('tablet-in',[205,195],pressure=.4);start=mark();send('tablet-down');wait(lambda:named('selectstart',start));send('tablet-out');zero()
  check(len(named('pointercancel',start))==1 and not named('pointerup',start) and not named('select',start) and not named('click',start),'Proximity cancellation ends contact without a false release selection')
  check(named('mouseout',start) and named('pointerout',start) and not latest()['hover'],'Leaving pen range clears DOM and compatibility hover state')
  g=window.get_geometry();s=latest();top=g.height-s['viewport'][1]*s['ratio']-28;c=fixture.connection
  xtest.fake_input(c,X.MotionNotify,x=g.x+int(205*s['ratio']),y=g.y+int(top+195*s['ratio']));c.sync();time.sleep(.1)
  start=mark();xtest.fake_input(c,X.ButtonPress,1);c.sync();wait(lambda:latest()['lengths']==[1,1])
  send('tablet-in',[205,195],pressure=.4);send('tablet-down');wait(lambda:latest()['lengths']==[2,2]);send('tablet-out');wait(lambda:latest()['lengths']==[1,1]);settle()
  check(not named('select',start),'Cancelling a pen leaves a held physical mouse XR source active')
  xtest.fake_input(c,X.ButtonRelease,1);c.sync();zero()
  check(len(named('select',start))==1 and len(named('click',start))==1,
        'Physical mouse release and click survive an intervening pen gesture')
  send('tablet-in',[205,195]);command('prevent');start=mark();send('tablet-down');wait(lambda:named('selectstart',start));send('tablet-up');zero()
  check(not named('mousedown',start) and not named('mouseup',start) and len(named('click',start))==1,'Prevented pointerdown suppresses compatibility mouse buttons while click remains independent')
  command('allow');start=mark();send('tablet-move',[452,322]);send('tablet-down');send('tablet-up');settle()
  wait(lambda:any(e.get('target')=='check' for e in named('click',start)));settle()
  check(latest()['checked'] and not named('selectstart',start),'A native pen click preserves checkbox activation outside XR canvases')
  send('tablet-move',[205,195]);start=mark();send('tablet-down');wait(lambda:named('selectstart',start));key('l',('Control_L',));zero()
  send('tablet-move',[220,195]);send('tablet-up');settle()
  check(len(named('pointercancel',start))==1 and not named('select',start),'Address focus cancels and cannot inherit a held native pen contact')
  send('tablet-move',[205,195]);start=mark();send('tablet-down');wait(lambda:named('selectstart',start));key('t',('Control_L',));wait(lambda:latest()['lengths']==[0,0]);key('w',('Control_L',));settle();send('tablet-up');zero()
  check(len(named('pointercancel',start))==1 and not named('select',start),'Tab replacement cancels the original pen owner and ignores its late release')
  send('tablet-move',[205,195]);start=mark();send('tablet-down');wait(lambda:named('selectstart',start));fixture.wayland.api('/api/action',{'op':'minimize','id':window.id});wait(lambda:latest()['lengths']==[0,0]);window.set_input_focus();settle();send('tablet-up');zero()
  check(len(named('pointercancel',start))==1 and not named('select',start),'Native minimization retires the pen before restoring the same document')
  send('tablet-move',[205,195]);start=mark();send('tablet-down');send('tablet-up');send('tablet-down');send('tablet-up');zero()
  wait(lambda:len(named('click',start))==2 and len(named('pointerup',start))==2);settle()
  check(len(named('click',start))==2 and len(named('dblclick',start))==1 and named('dblclick',start)[0]['detail']==2,
        'Two native pen taps preserve double-click behavior and click count')
  check([e['detail'] for e in named('click',start)]==[1,2] and [e['detail'] for e in named('mousedown',start)]==[1,2],
        'Pen click and compatibility mousedown detail share the same per-tool sequence')
  send('tablet-out');send('tablet-in',[205,195],slot=4);command('overlay-after-click');start=mark()
  send('tablet-down',slot=4);send('tablet-up',slot=4);send('tablet-down',slot=4);send('tablet-up',slot=4);settle()
  check([e['detail'] for e in named('click',start)]==[1,1] and not named('dblclick',start),
        'Replacing the target at the same position starts a new pen click sequence')
  command('remove-overlay');send('tablet-out',slot=4);send('tablet-in',[500,322]);start=mark();send('tablet-down');send('tablet-up');send('tablet-down');send('tablet-up');settle()
  wait(lambda:len(named('click',start))==2)
  check(latest()['selection'][1]>latest()['selection'][0] and len(named('dblclick',start))==1,
        'Native pen double-click preserves word selection in an editable control')
  send('tablet-out');send('tablet-in',[205,195],slot=3);start=mark();send('tablet-down',slot=3);send('tablet-up',slot=3);settle()
  check(len(named('click',start))==1 and named('click',start)[0]['detail']==1,'A different pen tool starts its own click count')
  send('tablet-out',slot=3);send('tablet-in',[205,195]);start=mark()
  for index in range(16):send('tablet-down');wait(lambda:named('selectstart',start) and latest()['lengths']==[1,1]);send('tablet-up');zero()
  check(len(named('selectstart',start))==16 and len(named('selectend',start))==16,'Repeated native pen actions create and retire each XR source once')
  check(all(ray is None for ray in latest()['latest'][0]['retired']),'Retired pen target-ray spaces no longer expose poses')
  for mode in ['redirect','release-got','in-move','release-down']:
   command('capture:'+mode);send('tablet-move',[205,195]);start=mark();send('tablet-down');wait(lambda:named('pointerdown',start));settle()
   check(not named('gotpointercapture',start),mode+': capture requests stay pending after pointerdown')
   send('tablet-move',[206,195]);settle()
   if mode in ['redirect','release-got']:
    check(named('pointermove',start)[-1]['target']=='other' and named('gotpointercapture',start)[0]['target']=='other',mode+': capture changes target before the next pointermove')
    boundary=[e for e in events(start) if e['name'] in ['pointerout','pointerover']]
    check(any(e['target']=='canvas' and e['related']=='other' for e in boundary) and any(e['target']=='other' and e['related']=='canvas' for e in boundary),mode+': capture emits matching pen boundary targets')
   elif mode=='in-move':
    check(named('pointermove',start)[-1]['target']=='canvas' and not [e for e in named('gotpointercapture',start) if e['target']=='other'],'Capture requested in pointermove waits for a later pointer event')
   else:
    check(not named('gotpointercapture',start),'A pointerdown listener can release pending implicit capture')
   send('tablet-move',[207,195]);settle()
   target='other' if mode in ['redirect','in-move'] else 'canvas'
   check(named('pointermove',start)[-1]['target']==target,mode+': the following sample uses the current capture target')
   check(named('mousemove',start)[-1]['target']==target,mode+': compatibility mousemove follows pen capture')
   send('tablet-up');zero()
   if mode in ['redirect','in-move']:
    check(named('pointerup',start)[-1]['target']=='other' and named('click',start)[-1]['target']=='other',mode+': pointerup and click retain the capturing target')
    check(any(e['target']=='other' for e in named('lostpointercapture',start)),mode+': release clears capture')
   if mode=='release-got':
    names=[e['name'] for e in events(start) if e.get('target')=='other' and e['name'] in ['gotpointercapture','pointermove','lostpointercapture','pointerout']]
    check(names==['gotpointercapture','pointermove','lostpointercapture','pointerout'],'Capture released in gotpointercapture stays effective for the current event only')
  command('capture:none');send('tablet-move',[205,195]);command('prevent')
  for button,want in [(331,2),(332,1)]:
   start=mark();send('tablet-button',button=button,pressed=True);settle();send('tablet-button',button=button,pressed=False);settle()
   aux=named('auxclick',start)
   check(len(aux)==1 and aux[0]['pointer'] and aux[0]['kind']=='pen' and aux[0]['button']==want and aux[0]['id']==named('pointerdown',start)[0]['id'],str(button)+': auxiliary click retains pen button and pointer identity')
   check(len(aux)==1 and aux[0]['trusted'] and aux[0]['bubbles'] and aux[0]['cancelable'] and aux[0]['composed'] and aux[0]['detail']==1,str(button)+': auxiliary click exposes standard trusted event flags and click count')
   check(not named('click',start) and not named('select',start) and latest()['lengths']==[0,0],str(button)+': auxiliary click remains independent of primary click and XR selection')
  command('allow');send('tablet-down');start=mark();send('tablet-button',button=331,pressed=True);send('tablet-button',button=331,pressed=False);settle()
  check(len(named('auxclick',start))==1 and not named('pointerup',start),'A chorded barrel release activates its own button without ending tip contact')
  send('tablet-up');zero();send('tablet-out');send('tablet-in',[205,195]);start=mark();send('tablet-button',button=332,pressed=True);send('tablet-out');settle()
  check(not named('auxclick',start) and named('pointercancel',start),'Canceled auxiliary input never activates a click')
  command('capture:redirect');send('tablet-in',[205,195]);start=mark();send('tablet-down');wait(lambda:named('pointerdown',start));send('tablet-out');zero()
  capture_events=[(e['name'],e['target']) for e in events(start) if e['name'] in ['gotpointercapture','pointercancel','lostpointercapture']]
  check(capture_events==[('gotpointercapture','other'),('pointercancel','other'),('lostpointercapture','other')],
        'Cancellation processes pending capture before cancel and releases the same target')
  canceled=next(i for i,e in enumerate(events(start)) if e['name']=='pointercancel');after_cancel=events(start)[canceled+1:]
  outs=[e for e in after_cancel if e['name']=='pointerout' and e.get('target')=='other']
  check(not [e for e in after_cancel if e['name'] in ['pointerover','pointerenter']] and len(outs)==1 and outs[0]['related'] is None,
        'Leaving pen range releases capture without re-entering the underlying element')
  command('capture:redirect-detach');send('tablet-in',[205,195]);start=mark();send('tablet-down');send('tablet-move',[206,195]);wait(lambda:named('gotpointercapture',start));command('detach-capture');send('tablet-move',[207,195],pressure=.6);settle()
  lost=[e for e in named('lostpointercapture',start) if e['target']=='document']
  check(len(lost)==1 and lost[0]['buttons']==1 and abs(lost[0]['pressure']-.6)<.0001,
        'Removing a capture target reports lost capture at the document with current pen properties')
  check(any(e['target']=='canvas' for e in named('gotpointercapture',start)) and named('pointermove',start)[-1]['target']=='canvas' and named('pointermove',start)[-1]['captured'],
        'A lost-capture listener can establish replacement capture before the next event')
  send('tablet-up');zero();command('restore-capture');command('capture:none')
  check(child.poll() is None and fixture.wayland.child.poll() is None,'Real browser and compositor survive all native pen lifecycle cases')
  if not all(c['passed'] for c in checks):raise AssertionError('Native pen diagnostic contains failed checks')
except BaseException as ex:
 failure=str(ex);raise
finally:
 if child and child.poll() is None:child.terminate();child.wait(timeout=8)
 if server:server.shutdown();server.server_close()
 if log:log.close()
 if fixture:fixture.close()
 (out/'snapshots.json').write_text(json.dumps(snapshots,indent=2));(out/'report.json').write_text(json.dumps({'checks':checks,'passed':failure is None and bool(checks) and all(c['passed'] for c in checks),'failure':failure,'scope':'Native Wayland pen callback through the real browser, Servo DOM and inline WebXR; synthetic host input, no physical hardware claim'},indent=2))
