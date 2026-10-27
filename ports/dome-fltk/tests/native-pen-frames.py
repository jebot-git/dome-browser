#!/usr/bin/env python3
"""Trusted native pen routing across actual same-origin and cross-origin frames."""
import argparse,http.server,json,os,resource,signal,subprocess,sys,tempfile,threading,time
from pathlib import Path
from urllib.parse import quote
from wayland_touch_fixture import TouchWayland
from Xlib import X,XK
from Xlib.ext import xtest
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_PEN_FRAME_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_PEN_FRAME_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--compositor',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--cross-origin',action='store_true');p.add_argument('--diagnostic',action='store_true');a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);checks=[];snapshots=[];commands=[];acks=[];failure=None
child=fixture=log=None;servers=[]
def check(value,name):
 checks.append({'name':name,'passed':bool(value)});print(('PASS ' if value else 'FAIL ')+name,flush=True)
 if not value and not a.diagnostic:raise AssertionError(name)
def latest():return max(snapshots,key=lambda s:s['frames']) if snapshots else None
def wait(fn):
 end=time.monotonic()+20
 while time.monotonic()<end:
  if child:assert child.poll() is None,'Browser exited unexpectedly'
  value=fn()
  if value:return value
  time.sleep(.025)
 raise AssertionError('Native frame fixture timed out')
def settle():
 s=latest();root=s['frames'];sub=s['child']['frames'];wait(lambda:latest()['frames']>root+4 and latest()['child']['frames']>sub+4)
def ev(doc,start=0,name=None):
 s=latest();s=s['child'] if doc else s
 return [e for e in s['events'][start:] if name is None or e['name']==name]
def mark():settle();return [len(ev(0)),len(ev(1))]
def command(op,sub=False,**kwargs):
 token=len(acks)+time.monotonic_ns();commands.append(dict(op=op,child=sub,token=token,**kwargs));wait(lambda:token in acks);settle()
def near(actual,want):return max(abs(x-y) for x,y in zip(actual,want))<2
try:
 with tempfile.TemporaryDirectory(prefix='dome-pen-frames-') as folder:
  runtime=Path(folder);runtime.chmod(0o700);fixture=TouchWayland(a.compositor.resolve(),runtime,out)
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*_):pass
   def do_GET(self):
    if self.path=='/commands':body=json.dumps(commands[:]).encode();commands.clear();kind='application/json'
    else:body=Path(__file__).with_suffix('.html').read_bytes();kind='text/html'
    self.send_response(200);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
   def do_POST(self):
    value=json.loads(self.rfile.read(int(self.headers['Content-Length'])));(acks if self.path=='/ack' else snapshots).append(value);self.send_response(204);self.end_headers()
  for _ in range(2):
   server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);servers.append(server);threading.Thread(target=server.serve_forever,daemon=True).start()
  port=servers[int(a.cross_origin)].server_port;frame=f'http://127.0.0.1:{port}/?child=1'
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')};env.update(HOME=folder,XDG_RUNTIME_DIR=folder,XDG_CONFIG_HOME=folder+'/config',XDG_DATA_HOME=folder+'/data',XDG_STATE_HOME=folder+'/state',XDG_CACHE_HOME=folder+'/cache',FLTK_BACKEND='wayland',WAYLAND_DISPLAY=fixture.wayland.socket,LIBGL_ALWAYS_SOFTWARE='1',DOME_WEBXR_SOCKET=folder+'/no-host.sock',RUST_LOG='warn')
  for name in ('DISPLAY','WAYLAND_SOCKET','FLTK_SCALING_FACTOR'):env.pop(name,None)
  log=(out/'browser.log').open('w');child=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',folder+'/profile',f'http://127.0.0.1:{servers[0].server_port}/?frame='+quote(frame,safe='')],env=env,stdout=log,stderr=log)
  wait(lambda:latest() and latest()['child']['frames']>4)
  window=fixture.wayland.wait(lambda:next((w for w in fixture.wayland.windows() if w.value()['client_pid']==child.pid),None));window.set_input_focus();settle()
  def point(sub,xy):
   if not sub:return xy
   f=latest()['frame'];x,y=[v+f['border'] for v in xy]
   m=f['matrix'];m=[1,0,0,1,0,0] if m=='none' else [float(v) for v in m.removeprefix('matrix(').removesuffix(')').split(',')]
   return [f['x']+m[0]*x+m[2]*y+m[4],f['y']+m[1]*x+m[3]*y+m[5]]
  def send(action,sub=0,xy=None,slot=0,**axes):
   pos=None
   if xy is not None:
    xy=point(sub,xy);g=window.get_geometry();s=latest();ratio=s['ratio'];top=g.height-s['viewport'][1]*ratio-28;pos=[g.x+xy[0]*ratio,g.y+top+xy[1]*ratio]
   fixture.command(action,slot,pos,**axes)
   if not action.startswith('tablet-'):fixture.command('frame')
  start=mark();send('tablet-in',1,[80,100],pressure=.4);wait(lambda:ev(1,start[1],'pointermove'));pid=ev(1,start[1],'pointermove')[-1]['id']
  check(near(ev(1,start[1],'pointermove')[-1]['client'],[80,100]),'Frame hover coordinates are relative to its viewport')
  send('tablet-down');wait(lambda:ev(1,start[1],'pointerdown'));send('tablet-move',1,[260,-20]);wait(lambda:ev(1,start[1],'gotpointercapture'));settle()
  moves=ev(1,start[1],'pointermove');check(near(moves[-1]['client'],[260,-20]) and moves[-1]['id']==pid and moves[-1]['target']=='area','Implicit capture retains frame target, identity and unclipped coordinates')
  send('tablet-move',1,[260,-60]);settle()
  check(near(ev(1,start[1],'pointermove')[-1]['client'],[260,-60]),'Capture continues beyond the top-level viewport')
  check(not ev(1,start[1],'pointercancel'),'Leaving the captured viewport does not cancel pen contact')
  send('tablet-up');wait(lambda:ev(1,start[1],'pointerup'));settle()
  check(ev(1,start[1],'lostpointercapture') and not ev(1,start[1],'click'),'Outside release ends capture without a false click')
  send('tablet-move',0,[80,100]);wait(lambda:ev(0,start[0],'pointermove'));settle()
  check(ev(0,start[0],'pointermove')[-1]['id']==pid and not ev(0,start[0],'pointerdown'),'Hover transfers to the parent with the same pen identity')
  send('tablet-out');settle();command('probe',id=pid);command('probe',True,id=pid)
  check(latest()['probe']['error']=='NotFoundError' and latest()['child']['probe']['error']=='NotFoundError','Leaving range retires the pointer in both documents')
  command('release',value=True);start=mark();send('tablet-in',0,[80,100]);send('tablet-down');wait(lambda:ev(0,start[0],'pointerdown'));pid=ev(0,start[0],'pointerdown')[-1]['id'];send('tablet-move',1,[80,100]);wait(lambda:ev(1,start[1],'pointermove'));settle()
  check(ev(0,start[0],'pointerout') and not ev(0,start[0],'pointercancel'),'An uncaptured held pen leaves its previous document without cancellation')
  moved=ev(1,start[1],'pointermove')[-1];check(moved['id']==pid and moved['buttons']==1 and not ev(1,start[1],'pointerdown'),'Entering another document with a held pen does not manufacture pointerdown')
  command('probe',id=pid);check(latest()['probe']['error'] is None and not latest()['probe']['captured'],'The inactive document cannot capture a pointer active in another document')
  send('tablet-up');settle();check(not ev(1,start[1],'click'),'An uncaptured cross-document release does not activate a click')
  send('tablet-out');command('release',value=True);start=mark();send('tablet-in',0,[80,100]);send('tablet-down');wait(lambda:ev(0,start[0],'pointerdown'));pid=ev(0,start[0],'pointerdown')[-1]['id'];command('capture');send('tablet-move',1,[80,100]);settle()
  check(ev(0,start[0],'gotpointercapture') and ev(0,start[0],'pointermove')[-1]['target']=='area' and not ev(1,start[1],'pointermove'),'Capture set by a later task is applied before cross-document routing')
  send('tablet-up');wait(lambda:ev(0,start[0],'pointerup'));wait(lambda:ev(1,start[1],'pointermove'));settle()
  hover=ev(1,start[1],'pointermove')[-1];check(hover['id']==pid and hover['buttons']==0 and not ev(1,start[1],'pointerup'),'Release restores actual frame hover without another hardware motion')
  send('tablet-out');start=mark();send('tablet-in',0,[80,100]);send('tablet-down');send('down',1,[80,100],slot=1);send('tablet-in',1,[120,100],slot=2);send('tablet-down',slot=2);wait(lambda:len(ev(0,start[0],'pointerdown'))+len(ev(1,start[1],'pointerdown'))>=3);settle()
  downs=ev(0,start[0],'pointerdown')+ev(1,start[1],'pointerdown');check(len({e['id'] for e in downs})==3 and {e['kind'] for e in downs}=={'pen','touch'},'Pen tools and touch contacts have distinct IDs across documents')
  send('up',slot=1);send('tablet-up',slot=2);send('tablet-out',slot=2);send('tablet-up');send('tablet-out');settle()
  before=latest()['ratio'];c=fixture.connection
  for name in ['Control_L','Shift_L','equal']:xtest.fake_input(c,X.KeyPress,c.keysym_to_keycode(XK.string_to_keysym(name)))
  for name in ['equal','Shift_L','Control_L']:xtest.fake_input(c,X.KeyRelease,c.keysym_to_keycode(XK.string_to_keysym(name)))
  c.sync();wait(lambda:latest()['ratio']>before);settle();check(latest()['ratio']>before,'Page zoom updates the native input coordinate scale')
  command('transform',value='matrix(1.1,.1,-.05,1.05,0,0)');command('scroll',value=40);command('scroll',True,value=30);start=mark();send('tablet-in',1,[80,70]);wait(lambda:ev(1,start[1],'pointermove'));send('tablet-down');send('tablet-move',1,[260,-20]);wait(lambda:ev(1,start[1],'gotpointercapture'));settle()
  check(near(ev(1,start[1],'pointermove')[-1]['client'],[260,-20]) and near(ev(1,start[1],'pointermove')[-1]['page'],[260,10]),'Capture coordinates respect frame transforms and independent document scrolling')
  send('tablet-up');send('tablet-out');settle();check(child.poll() is None and fixture.wayland.child.poll() is None,'Browser and compositor survive the cross-frame lifecycle cases')
 if not all(c['passed'] for c in checks):raise AssertionError('Diagnostic contains failed checks')
except BaseException as ex:failure=str(ex);raise
finally:
 if child and child.poll() is None:child.terminate();child.wait(timeout=8)
 for server in servers:server.shutdown();server.server_close()
 if log:log.close()
 if fixture:fixture.close()
 (out/'snapshots.json').write_text(json.dumps(snapshots));(out/'report.json').write_text(json.dumps({'checks':checks,'passed':failure is None and bool(checks),'failure':failure,'cross_origin':a.cross_origin,'scope':'Synthetic trusted native input through a private Wayland compositor, actual browser frames; no physical hardware claim'},indent=2))
