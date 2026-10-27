#!/usr/bin/env python3
"""Observe real native tablet protocol frames on an isolated FLTK client surface."""
import argparse, json, os, resource, signal, subprocess, sys, tempfile, time
from pathlib import Path
from wayland_touch_fixture import TouchWayland
from Xlib import X
from Xlib.ext import xtest

signal.pthread_sigmask(signal.SIG_SETMASK, [])
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
if os.environ.get('DOME_TABLET_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session','--',sys.executable,*sys.argv],
               {**os.environ,'DOME_TABLET_TEST_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--compositor',type=Path,required=True)
p.add_argument('--probe',type=Path,required=True)
p.add_argument('--output',type=Path,required=True)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
checks=[];fixture=child=log=None;failure=None
def events(after=0):
    return [json.loads(line[len('TABLET_PROBE '):]) for line in (out/'probe.log').read_text().splitlines()
            if line.startswith('TABLET_PROBE ')][after:]
def wait(fn):
    deadline=time.monotonic()+15
    while time.monotonic()<deadline:
        assert child.poll() is None,'Tablet client exited unexpectedly'
        result=fn()
        if result:return result
        time.sleep(.025)
    raise AssertionError('Tablet protocol fixture timed out')
def check(value,name):
    checks.append({'name':name,'passed':bool(value)});print(('PASS ' if value else 'FAIL ')+name,flush=True)
    assert value,name
def named(name,after=0):return [e for e in events(after) if e.get('event')==name]
try:
    with tempfile.TemporaryDirectory(prefix='dome-native-tablet-') as folder:
        runtime=Path(folder);runtime.chmod(0o700)
        fixture=TouchWayland(a.compositor.resolve(),runtime,out)
        env={**os.environ,'HOME':folder,'XDG_RUNTIME_DIR':folder,'XDG_CONFIG_HOME':folder+'/config',
             'WAYLAND_DISPLAY':fixture.wayland.socket,'FLTK_BACKEND':'wayland','LIBGL_ALWAYS_SOFTWARE':'1'}
        for key in ('DISPLAY','WAYLAND_SOCKET','FLTK_SCALING_FACTOR'):env.pop(key,None)
        log=(out/'probe.log').open('w')
        child=subprocess.Popen([str(a.probe.resolve())],env=env,stdout=log,stderr=log)
        wait(lambda:any(e.get('ready') for e in events()))
        check(True,'Native tablet manager binds on the client Wayland connection')
        window=fixture.wayland.wait(lambda:next((w for w in fixture.wayland.windows() if w.value()['client_pid']==child.pid),None))
        window.set_input_focus();geometry=window.get_geometry()
        def send(action,point=None,slot=0,**axes):
            if point is not None:point=[geometry.x+point[0],geometry.y+point[1]]
            fixture.command(action,slot,point,**axes);time.sleep(.07)
        start=len(events());send('tablet-in',[120.25,130.5],pressure=.3,tilt=[-12.5,33.25],distance=.25,
                                 rotation=45.5,slider=-.25,wheel=[12.5,2])
        wait(lambda:named('pressure',start))
        check(len(named('in',start))==1 and named('in',start)[0]['own_surface'],
              'Tablet proximity targets the actual native client surface')
        check(named('in',start)[0]['own_tablet'],
              'Independent tablet-seat bindings receive their own tablet resource')
        check(not named('down',start),'Hover alone never presses the pen tip')
        motion=named('motion',start)[-1]
        check(abs(motion['x']-120.25)<.01 and abs(motion['y']-130.5)<.01,
              'Tablet coordinates preserve Wayland fractional precision')
        check(len(named('capability',start))==6,'All declared tool axes are advertised')
        check(abs(named('pressure',start)[-1]['value']-19661)<=1,'Normalized pressure reaches the protocol accurately')
        check(abs(named('distance',start)[-1]['value']-16384)<=1,'Distance reaches the protocol accurately')
        tilt=named('tilt',start)[-1]
        check(tilt['x']==-12.5 and tilt['y']==33.25,'Signed tilt angles retain fractional precision')
        check(named('rotation',start)[-1]['value']==45.5,'Tool twist reaches the protocol')
        check(abs(named('slider',start)[-1]['value']+16384)<=1,'Signed tool slider reaches the protocol')
        check(named('wheel',start)[-1]['degrees']==12.5 and named('wheel',start)[-1]['clicks']==2,
              'Wheel angle and discrete clicks remain paired')
        axes_index=max(i for i,e in enumerate(events(start)) if e.get('event') in ('pressure','distance','tilt','rotation','slider','wheel'))
        check(events(start)[axes_index+1]['event']=='frame','Axis updates complete with the motion protocol frame')
        start=len(events());send('tablet-move',[125,135],pressure=2,distance=-1,tilt=[120,-120],rotation=-15,slider=-2)
        check(named('pressure',start)[-1]['value']==65535 and named('distance',start)[-1]['value']==0,
              'Pressure and distance are bounded to protocol ranges')
        tilt=named('tilt',start)[-1]
        check(tilt['x']==90 and tilt['y']==-90,'Tilt is bounded to supported angles')
        check(named('rotation',start)[-1]['value']==345 and named('slider',start)[-1]['value']==-65535,
              'Rotation and slider use valid normalized protocol ranges')
        start=len(events());send('tablet-down');send('tablet-down')
        check(len(named('down',start))==1,'Duplicate physical tip-down does not duplicate the press')
        send('tablet-move',[900,500],pressure=.8)
        check(not named('out',start) and named('motion',start)[-1]['x']==900,
              'Pressed tablet motion retains its original surface outside its bounds')
        send('tablet-button',button=331,pressed=True);send('tablet-button',button=331,pressed=False)
        check([(e['button'],e['state']) for e in named('button',start)]==[(331,1),(331,0)],
              'Barrel-button press and release are delivered independently of the tip')
        send('tablet-up');send('tablet-up')
        check(len(named('up',start))==1,'Duplicate physical tip-up does not duplicate completion')
        send('tablet-move',[900,500])
        check(len(named('out',start))==1,'Released hover can leave the original surface')
        start=len(events());send('tablet-button',button=331,pressed=True)
        send('tablet-move',[150,150]);send('tablet-down');send('tablet-out')
        check([e['event'] for e in events(start) if e.get('event') in ('in','down','up','out')]==['in','down','up','out'],
              'Proximity loss releases a held tip before leaving the surface')
        names=[e['event'] for e in events(start) if 'event' in e]
        check(names[-4:]==['up','button','out','frame'],
              'Tip and barrel-button releases share the final proximity-out frame')
        check([e['state'] for e in named('button',start)]==[1,0],
              'A barrel button held outside the surface is announced on entry and released on exit')
        first_frame=next(i for i,name in enumerate(names) if name=='frame')
        check('button' in names[:first_frame],
              'Held barrel-button state is included in the first proximity-in frame')
        start=len(events());send('tablet-out');send('tablet-up')
        check(not named('out',start) and not named('up',start),'Repeated proximity loss and stale tip release are ignored')
        start=len(events());send('tablet-move',[150,150]);send('tablet-down')
        fixture.wayland.api('/api/action',{'op':'minimize','id':window.id})
        wait(lambda:named('out',start))
        check(len(named('up',start))==1 and len(named('out',start))==1,
              'Minimizing a native owner cancels its pressed tablet tool')
        window.set_input_focus();send('tablet-up');send('tablet-move',[150,150]);send('tablet-down');send('tablet-up')
        check(len(named('down',start))==2 and len(named('up',start))==2,
              'Restoring the window requires a fresh press and allows a new gesture')
        start=len(events());send('tablet-down');send('tablet-remove')
        wait(lambda:any(e.get('tablet_removed') for e in events(start)))
        check(len(named('up',start))==1 and len(named('out',start))==1,
              'Device removal terminates a held tool before removing resources')
        check(len(named('removed',start))==1,'Removing a tablet releases its associated tool resource')
        start=len(events());send('tablet-in',[160,160],pressure=.4);send('tablet-down');send('tablet-up')
        check(len(named('done',start))==1 and len(named('down',start))==1 and len(named('up',start))==1,
              'Reconnecting a tablet creates usable fresh client resources')
        connection=fixture.connection
        xtest.fake_input(connection,X.MotionNotify,x=geometry.x+200,y=geometry.y+200)
        connection.sync();time.sleep(.1)
        start=len(events());xtest.fake_input(connection,X.ButtonPress,1);connection.sync()
        wait(lambda:any(e.get('mouse')==1 for e in events(start)))
        start=len(events());send('tablet-move',[220,220])
        check(not any(e.get('mouse')==5 for e in events(start)),
              'Tablet motion does not move a held physical mouse drag')
        check(named('motion',start)[-1]['x']==220,'Tablet coordinates still update independently during a mouse drag')
        xtest.fake_input(connection,X.MotionNotify,x=geometry.x+230,y=geometry.y+230);connection.sync()
        wait(lambda:any(e.get('mouse')==5 for e in events(start)))
        xtest.fake_input(connection,X.ButtonRelease,1);connection.sync()
        wait(lambda:any(e.get('mouse')==2 for e in events(start)))
        check(True,'Physical mouse motion and release remain usable during tablet proximity')
        first_tool=named('in')[-1]['tool']
        send('tablet-in',[170,170],slot=1,device=1,pressure=.4)
        second_tool=named('in')[-1]['tool']
        check(first_tool!=second_tool,'Independent physical tools have distinct client protocol resources')
        send('tablet-down',slot=0);send('tablet-down',slot=1)
        start=len(events());send('tablet-remove',device=0)
        check([e['tool'] for e in named('up',start)]==[first_tool],
              'Removing one tablet cancels only its associated pressed tools')
        send('tablet-up',slot=1)
        check([e['tool'] for e in named('up',start)]==[first_tool,second_tool],
              'A second tablet remains usable after the first device disconnects')
        send('tablet-out',slot=1);send('tablet-remove',device=1)
        for cycle in range(2):
            for slot in range(35):send('tablet-in',[160,160],slot=slot,pressure=.4)
            check(len(named('done'))-len(named('removed'))==32,
                  f'Tool resource limit remains bounded in connection cycle {cycle+1}')
            send('tablet-remove')
            wait(lambda:len(named('done'))==len(named('removed')))
            check(True,f'All client tool resources are retired in connection cycle {cycle+1}')
        frames=[e['time'] for e in named('frame')]
        check(all(a<=b for a,b in zip(frames,frames[1:])),'Protocol frame times follow the host monotonic timeline')
        check(child.poll() is None and fixture.wayland.child.poll() is None,
              'Native tablet client and compositor survive the full protocol lifecycle')
except BaseException as ex:
    failure=str(ex);raise
finally:
    if child and child.poll() is None:child.terminate();child.wait(timeout=5)
    if log:log.close()
    if fixture:fixture.close()
    (out/'report.json').write_text(json.dumps({'checks':checks,'passed':failure is None and bool(checks),
                                             'failure':failure,'scope':'Native host-to-Wayland-client tablet transport; browser and FLTK raw stylus delivery remain separate integration work'},indent=2))
