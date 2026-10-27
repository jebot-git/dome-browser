#!/usr/bin/env python3
"""Private Wayland repaint-hint test with a real FLTK client and simulated XR output."""
from pathlib import Path
import argparse,contextlib,hashlib,json,os,shlex,signal,subprocess,sys,tempfile,threading,time
if os.environ.get('DOME_NATIVE_VISIBILITY_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_NATIVE_VISIBILITY_BUS':'1'})
from Xlib import display
from private_wayland import PrivateWayland
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--toolkit',type=Path,required=True);parser.add_argument('--compositor',type=Path,required=True);parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args();out=args.output.resolve();out.mkdir(exist_ok=False,parents=True,mode=0o700);toolkit=args.toolkit.resolve()
os.environ['DOME_COMPOSITOR']=str(args.compositor.resolve())
code=r'''#include <FL/Fl.H>
#include <FL/Fl_Double_Window.H>
#include <FL/Fl_Box.H>
#include <FL/platform.H>
#include <cstdio>
int main(int argc,char** argv) {
 Fl_Double_Window window(320,180,"Window visibility probe");
 window.xclass("org.domed.VisibilityProbe");Fl_Box box(10,10,300,160,"Visibility probe");window.end();window.show();
 int previous=-2;
 while(window.shown()) {Fl::wait(.01);const int state=fl_wl_window_suspended(&window);
  if(state!=previous){std::printf("{\"suspended\":%d,\"shown\":%d}\n",state,window.shown());std::fflush(stdout);previous=state;}}
 return 0;
}'''
(out/'probe.cpp').write_text(code)
flags=shlex.split(subprocess.check_output([str(toolkit/'bin/fltk-config'),'--cxxflags','--ldflags'],text=True))
subprocess.run(['c++','-O3','-std=c++17',str(out/'probe.cpp'),'-o',str(out/'probe'),*flags],check=True)
checks=[];completed=False;child=xvfb=wayland=connection=None;logs=[];heartbeat_stop=threading.Event();heartbeat=None;heartbeat_errors=[]
temporary_paths=contextlib.ExitStack()
def check(ok,name):
 checks.append({'name':name,'passed':bool(ok)});print(('PASS ' if ok else 'FAIL ')+name,flush=True)
 assert ok,name
def records():
 rows=[]
 for line in (out/'client.log').read_text().splitlines():
  try:rows.append(json.loads(line))
  except ValueError:pass
 return rows
def wait(fn,seconds=20):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  if child and child.poll() is not None:raise AssertionError('FLTK probe exited unexpectedly')
  value=fn()
  if value:return value
  time.sleep(.03)
 raise AssertionError('Private visibility probe timed out')
def suspended(value):return bool(records()) and records()[-1]['suspended']==value
try:
 with contextlib.nullcontext(temporary_paths.enter_context(tempfile.TemporaryDirectory(prefix='dome-native-visibility-'))) as folder:
  runtime=Path(folder);runtime.chmod(0o700)
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
  with os.fdopen(rd) as pipe:screen=':'+pipe.readline().strip()
  connection=display.Display(screen);wayland=PrivateWayland(connection,screen,runtime,out)
  env={**os.environ,'HOME':folder,'XDG_RUNTIME_DIR':folder,'XDG_CONFIG_HOME':folder+'/config','XDG_DATA_HOME':folder+'/data','XDG_CACHE_HOME':folder+'/cache','FLTK_BACKEND':'wayland','WAYLAND_DISPLAY':wayland.socket,'LD_LIBRARY_PATH':str(toolkit/'lib64')+':'+str(toolkit/'lib')+':'+os.environ.get('LD_LIBRARY_PATH','')}
  for key in ['DISPLAY','WAYLAND_SOCKET']:env.pop(key,None)
  log=(out/'client.log').open('w');logs.append(log);child=subprocess.Popen([str(out/'probe')],env=env,stdout=log,stderr=log)
  window=wait(lambda:next((w for w in wayland.windows() if w.value()['client_pid']==child.pid),None));identity=window.id
  check(wait(lambda:suspended(0)),'FLTK observes an unsuspended decorated Wayland window')
  wayland.api('/api/action',{'op':'minimize','id':identity});check(wait(lambda:suspended(1)),'Minimization sends the suspended repaint hint')
  check(records()[-1]['shown']==1 and any(w['id']==identity for w in wayland.scene()['windows']),'A suspended FLTK toplevel stays alive for restoration')
  wayland.api('/api/action',{'op':'focus','id':identity});check(wait(lambda:suspended(0)),'Restoring focus resumes repainting without recreating the window')
  wayland.api('/api/action',{'op':'switch_workspace','workspace':1});check(wait(lambda:suspended(1)),'An inactive workspace suspends the window')
  wayland.api('/api/action',{'op':'switch_workspace','workspace':0});check(wait(lambda:suspended(0)),'Returning to the workspace resumes repainting')
  def tick():
   while not heartbeat_stop.is_set():
    try:wayland.api('/api/xr-session',{'id':4811,'visible':True,'focused':True,'frame_period_ns':11111111})
    except Exception as e:heartbeat_errors.append(str(e));return
    heartbeat_stop.wait(.15)
  heartbeat=threading.Thread(target=tick,daemon=True);heartbeat.start();wait(lambda:wayland.api('/api/xr-control')['visible'])
  wayland.api('/api/action',{'op':'window_visibility','id':identity,'desktop':False,'xr':True});check(wait(lambda:suspended(0)),'An XR-only window keeps repainting while XR output is active')
  wayland.api('/api/action',{'op':'xr_presentation','desktop':False});check(wait(lambda:suspended(1)),'Hiding XR desktop presentation suspends an XR-only window')
  wayland.api('/api/action',{'op':'window_visibility','id':identity,'desktop':True,'xr':False});check(wait(lambda:suspended(0)),'A visible monitor window keeps repainting when XR desktop content is hidden')
  wayland.api('/api/action',{'op':'xr_presentation','desktop':True})
  wayland.api('/api/action',{'op':'window_visibility','id':identity,'desktop':False,'xr':True});wait(lambda:suspended(0))
  heartbeat_stop.set();heartbeat.join(timeout=3);assert not heartbeat_errors,heartbeat_errors
  wayland.api('/api/xr-session',{'id':4811,'visible':False});wait(lambda:not wayland.api('/api/xr-control')['visible'])
  check(wait(lambda:next(w for w in wayland.scene()['windows'] if w['id']==identity)['desktop_visible']),'XR disconnection first restores an XR-only window to the desktop')
  wayland.api('/api/action',{'op':'window_visibility','id':identity,'desktop':False,'xr':True});check(wait(lambda:suspended(1)),'An XR-only window pauses when no XR output is active')
  wayland.api('/api/action',{'op':'window_visibility','id':identity,'desktop':True,'xr':False});check(wait(lambda:suspended(0)),'Moving the window back to the monitor resumes it')
  wayland.api('/api/action',{'op':'close','id':identity});check(child.wait(timeout=10)==0,'FLTK client closes cleanly after native visibility transitions');child=None;completed=True
finally:
 heartbeat_stop.set()
 if heartbeat:heartbeat.join(timeout=3)
 if child is not None and child.poll() is None:child.terminate();child.wait(timeout=5)
 if wayland:wayland.close()
 if connection:connection.close()
 if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
 for log in logs:log.close()
 (out/'report.json').write_text(json.dumps({'passed':completed and all(c['passed'] for c in checks),'checks':checks,'states':records() if (out/'client.log').exists() else [],'failure':None if completed else str(sys.exc_info()[1]),'compositor_sha256':hashlib.sha256(args.compositor.read_bytes()).hexdigest(),'probe_sha256':hashlib.sha256((out/'probe').read_bytes()).hexdigest(),'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'scope':'Real FLTK query and Wayland configure states in a private compositor; XR output availability is simulated. No physical headset or live desktop changes.'},indent=2)+'\n')
 temporary_paths.close()
