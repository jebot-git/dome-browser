#!/usr/bin/env python3
"""Native tablet protocol into FLTK callbacks and widgets in a private session."""
import argparse, json, os, resource, signal, subprocess, sys, tempfile, time
from pathlib import Path
from wayland_touch_fixture import TouchWayland
signal.pthread_sigmask(signal.SIG_SETMASK, [])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_PEN_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_PEN_TEST_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--compositor',type=Path,required=True);p.add_argument('--probe',type=Path,required=True)
p.add_argument('--output',type=Path,required=True);p.add_argument('--scale',type=float,default=1)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
checks=[];child=fixture=log=None;failure=None
def events(after=0):
    return [json.loads(line[len('PEN_PROBE '):]) for line in (out/'probe.log').read_text().splitlines() if line.startswith('PEN_PROBE ')][after:]
def wait(fn):
    deadline=time.monotonic()+15
    while time.monotonic()<deadline:
        assert child.poll() is None,'Pen probe exited unexpectedly'
        value=fn()
        if value:return value
        time.sleep(.025)
    raise AssertionError('Native FLTK pen fixture timed out')
def check(value,name):
    checks.append({'name':name,'passed':bool(value)});print(('PASS ' if value else 'FAIL ')+name,flush=True);assert value,name
def raw(after=0,action=None):return [e for e in events(after) if 'action' in e and (action is None or e['action']==action)]
try:
    with tempfile.TemporaryDirectory(prefix='dome-native-pen-') as folder:
        runtime=Path(folder);runtime.chmod(0o700);fixture=TouchWayland(a.compositor.resolve(),runtime,out)
        env={**os.environ,'HOME':folder,'XDG_RUNTIME_DIR':folder,'XDG_CONFIG_HOME':folder+'/config',
             'WAYLAND_DISPLAY':fixture.wayland.socket,'FLTK_BACKEND':'wayland','LIBGL_ALWAYS_SOFTWARE':'1','FLTK_SCALING_FACTOR':str(a.scale)}
        for name in ('DISPLAY','WAYLAND_SOCKET'):env.pop(name,None)
        log=(out/'probe.log').open('w');child=subprocess.Popen([str(a.probe.resolve())],stdin=subprocess.PIPE,text=True,env=env,stdout=log,stderr=log)
        wait(lambda:any(e.get('ready') for e in events()))
        window=fixture.wayland.wait(lambda:next((w for w in fixture.wayland.windows() if w.value()['client_pid']==child.pid),None));window.set_input_focus();g=window.get_geometry()
        check(abs(g.width/480-a.scale)<.02,'Requested toolkit scale is active')
        def send(action,point=None,slot=0,**axes):
            if point is not None:point=[g.x+point[0]*a.scale,g.y+point[1]*a.scale]
            fixture.command(action,slot,point,**axes);time.sleep(.06)
        def control(op):
            start=len(events());child.stdin.write(op+'\n');child.stdin.flush();wait(lambda:any(e.get('ack')==op for e in events(start)))
        start=len(events());send('tablet-in',[120.25,130.5],pressure=.3,tilt=[-12.5,33.25],distance=.25,rotation=45.5,slider=-.25)
        wait(lambda:raw(start,4))
        check(len(raw(start,5))==1 and not raw(start,0),'Proximity creates hover without pressing the tip')
        hover=raw(start,4)[-1]
        check(abs(hover['x']-120.25)<.01 and abs(hover['y']-130.5)<.01,'Fractional tablet positions retain toolkit-scale precision')
        check(hover['primary']==1 and hover['pressure']==0 and hover['buttons']==0,'Hover has stable primary identity and no contact pressure or buttons')
        start=len(events());send('tablet-down');down=wait(lambda:raw(start,0))[-1]
        check(abs(down['pressure']-.3)<.0001 and down['buttons']==1,'Tip contact carries measured pressure and primary button state')
        check(down['tilt']==[-12.5,33.25] and down['rotation']==45.5 and abs(down['slider']+.25)<.0001,
              'Tilt, twist and tangential pressure reach the raw callback')
        check(abs(down['distance']-.25)<.0001 and down['capabilities']!=0,'Distance and supported-axis flags reach the callback')
        send('tablet-move',[150.75,160.25],pressure=.7,wheel=[12.5,2]);moved=raw(start,1)[-1]
        check(abs(moved['pressure']-.7)<.0001 and moved['x']==150.75 and moved['y']==160.25,'Held contact motion updates coordinates and pressure')
        check(moved['wheel']==12.5 and moved['wheel_clicks']==2,'Wheel angle and steps belong to their pen frame')
        send('tablet-move',[155,165]);check(raw(start,1)[-1]['wheel']==0,'Wheel deltas do not repeat in the next frame')
        send('tablet-button',button=331,pressed=True);send('tablet-button',button=331,pressed=False)
        check([e['buttons'] for e in raw(start,7)]==[3,1],'Barrel-button changes preserve the independent tip state')
        send('tablet-up');up=raw(start,2)[-1]
        check(up['pressure']==0 and up['buttons']==0,'Tip release clears contact pressure and primary button')
        check(not any('mouse' in e for e in events(start)),'Owned pen contacts do not synthesize native mouse input')
        start=len(events());send('tablet-in',[180,180],slot=1);send('tablet-down',slot=0);send('tablet-down',slot=1)
        down=raw(start,0);check(len(down)==2 and [e['primary'] for e in down]==[1,0],'Simultaneous pen tools retain separate primary and auxiliary identities')
        send('tablet-out',slot=0);send('tablet-move',[190,190],slot=1)
        check(raw(start,1)[-1]['primary']==0,'Auxiliary pen does not promote when the primary leaves')
        send('tablet-up',slot=1);send('tablet-out',slot=1)
        start=len(events());send('tablet-in',[110,130]);send('tablet-down');send('tablet-out')
        check([e['action'] for e in raw(start) if e['action'] in (0,2,3,6)]==[0,3,6],
              'Forced tip-up plus proximity-out cancels once without ordinary completion')
        start=len(events());send('tablet-in',[80,35]);send('tablet-down');send('tablet-up')
        check(sum(bool(e.get('clicked')) for e in events(start))==1,'Unclaimed primary pen activates a native button once')
        start=len(events());send('tablet-down');send('tablet-out')
        check(not any(e.get('clicked') for e in events(start)),'Cancelled native button contact never activates the widget')
        start=len(events());send('tablet-in',[110,130]);send('tablet-down');control('disable');control('enable');send('tablet-move',[120,140]);send('tablet-up')
        check(len(raw(start,0))==1 and len(raw(start,3))==1 and not raw(start,2),
              'Callback replacement cancels ownership and cannot inherit a held contact')
        start=len(events());send('tablet-down');send('tablet-up')
        check([e['action'] for e in raw(start) if e['action'] in (0,2,3)]==[0,2],'Fresh contact works after callback replacement')
        start=len(events());send('tablet-move',[410,130]);send('tablet-down');send('tablet-up')
        check([e['action'] for e in raw(start) if e['action'] in (0,2,3)]==[0,3],
              'Reentrant callback replacement cancels once and skips queued completion')
        send('tablet-move',[110,130]);start=len(events());send('tablet-down');send('tablet-remove')
        check(len(raw(start,3))==1 and not raw(start,2),'Physical device removal cancels active raw ownership')
        start=len(events());send('tablet-in',[120,140]);send('tablet-down');fixture.wayland.api('/api/action',{'op':'minimize','id':window.id})
        wait(lambda:raw(start,3))
        check(len(raw(start,3))==1 and not raw(start,2),'Native window minimization cancels its pen contact')
        cancelled=raw(start,3)[-1]
        check(cancelled['x']==120 and cancelled['y']==140,'Cancellation preserves the last valid contact coordinates')
        window.set_input_focus();send('tablet-up');start=len(events());send('tablet-move',[120,140]);send('tablet-down');send('tablet-up')
        check(len(raw(start,0))==1 and len(raw(start,2))==1,'Restored window starts a fresh usable pen gesture')
        start=len(events());send('tablet-down');control('hide');wait(lambda:raw(start,3))
        cancelled=raw(start,3)[-1]
        check(cancelled['x']==120 and cancelled['y']==140 and cancelled['pressure']==0 and cancelled['buttons']==0,
              'Hiding the native surface retains cancellation coordinates while clearing contact state')
        control('show')
        window=fixture.wayland.wait(lambda:next((w for w in fixture.wayland.windows() if w.value()['client_pid']==child.pid),None));window.set_input_focus();g=window.get_geometry()
        send('tablet-up');start=len(events());send('tablet-move',[120,140]);send('tablet-down');send('tablet-up')
        check(len(raw(start,0))==1 and len(raw(start,2))==1,'Recreated native surface accepts a fresh contact without reviving its old owner')
        send('tablet-out');start=len(events());send('tablet-in',[120,140],slot=2,capabilities=0);send('tablet-down',slot=2,capabilities=0)
        check(raw(start,0)[-1]['pressure']==.5 and raw(start,0)[-1]['capabilities']==0,
              'A pen without a pressure sensor uses the active-contact default')
        send('tablet-up',slot=2,capabilities=0);send('tablet-out',slot=2,capabilities=0)
        start=len(events());send('tablet-in',[80,35],slot=3,eraser=True);send('tablet-down',slot=3,eraser=True);send('tablet-up',slot=3,eraser=True)
        eraser=raw(start,0)[-1]
        check(eraser['primary']==1 and eraser['eraser']==1 and eraser['buttons']==32,
              'Primary eraser contact exposes the eraser button instead of an ordinary tip')
        check(not any(e.get('clicked') for e in events(start)),'An unclaimed eraser cannot activate a native button')
        start=len(events());send('tablet-move',[120,140],slot=3,eraser=True);send('tablet-down',slot=3,eraser=True);send('tablet-up',slot=3,eraser=True)
        check(len(raw(start,0))==1 and len(raw(start,2))==1 and raw(start,0)[-1]['buttons']==32,
              'Owned eraser contacts complete with their separate button identity')
        send('tablet-out',slot=3,eraser=True);control('disable');start=len(events())
        send('tablet-in',[80,35]);send('tablet-down');send('tablet-up')
        check(not raw(start,0) and sum(bool(e.get('clicked')) for e in events(start))==1,
              'Native primary-tip widgets remain usable without a registered raw callback')
        control('enable')
        check(child.poll() is None and fixture.wayland.child.poll() is None,'Toolkit and compositor survive all pen lifecycle cases')
except BaseException as ex:
    failure=str(ex);raise
finally:
    if child and child.poll() is None:child.terminate();child.wait(timeout=5)
    if log:log.close()
    if fixture:fixture.close()
    (out/'report.json').write_text(json.dumps({'checks':checks,'passed':failure is None and bool(checks),'failure':failure,
                                             'scale':a.scale,'scope':'Native Wayland tablet protocol into the FLTK pen callback and widgets; no browser or physical-device claim'},indent=2))
