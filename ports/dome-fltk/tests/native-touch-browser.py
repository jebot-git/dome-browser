#!/usr/bin/env python3
"""Native Wayland touch through FLTK and the shipping browser into inline WebXR."""
import argparse, http.server, json, math, os, resource, signal, subprocess, sys, tempfile, threading, time
from pathlib import Path
from wayland_touch_fixture import TouchWayland
from Xlib import X, XK
from Xlib.ext import xtest

signal.pthread_sigmask(signal.SIG_SETMASK, [])
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
if os.environ.get('DOME_TOUCH_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_TOUCH_TEST_BUS':'1'})
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--compositor', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
args = p.parse_args()
out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
snapshots = []; commands = []; checks = []; replies = []; error = None
child = fixture = server = log = None

def check(value, name):
    checks.append({'name':name, 'passed':bool(value)})
    print(('PASS ' if value else 'FAIL ') + name, flush=True)
    assert value, name

def latest():
    s = snapshots[-1] if snapshots else None
    if s:
        assert not s.get('error'), s['error']
    return s

def wait(fn, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if child:
            assert child.poll() is None, 'Browser exited unexpectedly'
        result = fn()
        if result:
            return result
        time.sleep(.03)
    raise AssertionError('Native browser fixture timed out')

def events(start=0, index=0):
    return [e for e in latest()['events'][start:] if e.get('index', index) == index]

def named(name, start=0, index=0):
    return [e for e in events(start, index) if e['name'] == name]

def settled():
    old = latest()['frames']
    wait(lambda: latest()['frames'] > old + 4)

def command(op):
    commands.append(op)
    wait(lambda: op in replies)
    replies.remove(op)
    settled()

def zero():
    wait(lambda: latest()['lengths'] == [0,0]); settled()

def mark():
    settled()
    return len(latest()['events'])

def ray_close(ray, expected):
    n = math.sqrt(sum(v*v for v in expected))
    return ray and max(abs(a-b/n) for a,b in zip(ray, expected)) < .03

try:
    with tempfile.TemporaryDirectory(prefix='dome-browser-native-touch-') as folder:
        runtime = Path(folder); runtime.chmod(0o700)
        fixture = TouchWayland(args.compositor.resolve(), runtime, out)
        class Site(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def do_GET(self):
                if self.path == '/commands':
                    body = json.dumps(commands[:]).encode(); commands.clear()
                    kind = 'application/json'
                else:
                    body = Path(__file__).with_name('webxr-touch.html').read_bytes()
                    body += b'''<script>
                    setInterval(async()=>{try{
                      if(!window.test)return;
                      const s=window.test.snapshot();
                      s.viewport=[innerWidth,innerHeight];s.ratio=devicePixelRatio;
                      s.rect=canvas.getBoundingClientRect().toJSON();
                      await fetch('/snapshot',{method:'POST',body:JSON.stringify(s)});
                      for(const op of await (await fetch('/commands')).json()){
                        window.test.command(op);
                        await fetch('/ack',{method:'POST',body:JSON.stringify(op)});
                      }
                    }catch(e){console.error(e)}},80);
                    </script>'''
                    kind = 'text/html'
                self.send_response(200); self.send_header('Content-Type',kind)
                self.send_header('Content-Length',str(len(body))); self.end_headers(); self.wfile.write(body)
            def do_POST(self):
                value = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                (replies if self.path == '/ack' else snapshots).append(value)
                self.send_response(204); self.end_headers()
        server = http.server.ThreadingHTTPServer(('127.0.0.1',0), Site)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        env = {k:v for k,v in os.environ.items() if not k.startswith('DOME_')}
        env.update(HOME=folder, XDG_RUNTIME_DIR=folder, XDG_CONFIG_HOME=folder+'/config',
                   XDG_DATA_HOME=folder+'/data', XDG_STATE_HOME=folder+'/state',
                   XDG_CACHE_HOME=folder+'/cache', FLTK_BACKEND='wayland',
                   WAYLAND_DISPLAY=fixture.wayland.socket, LIBGL_ALWAYS_SOFTWARE='1',
                   DOME_WEBXR_SOCKET=folder+'/no-host.sock', RUST_LOG='warn')
        for name in ('DISPLAY','WAYLAND_SOCKET','FLTK_SCALING_FACTOR'):
            env.pop(name,None)
        log = (out/'browser.log').open('w')
        child = subprocess.Popen([str(args.binary.resolve()),'--software','--no-restore',
                                   '--profile',folder+'/profile',f'http://127.0.0.1:{server.server_port}/'],
                                  env=env, stdout=log, stderr=log)
        wait(lambda: latest() and latest()['ready'] and latest()['frames'] > 4)
        window = fixture.wayland.wait(lambda: next((w for w in fixture.wayland.windows()
                                         if w.value()['client_pid'] == child.pid),None))
        window.set_input_focus()
        check(latest()['lengths'] == [0,0], 'Real browser starts two inline sessions without a host')
        def send(action, slot=None, point=None, chrome=False):
            if point is not None:
                g = window.get_geometry(); s = latest(); ratio = s['ratio']
                top = g.height - s['viewport'][1]*ratio - 28
                point = [g.x+point[0]*ratio, g.y+(point[1] if chrome else top+point[1]*ratio)]
            fixture.command(action,slot,point); fixture.command('frame')
        def key(name, modifiers=()):
            connection = fixture.connection
            for item in (*modifiers,name):
                xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(XK.string_to_keysym(item)))
            for item in (name,*reversed(modifiers)):
                xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(XK.string_to_keysym(item)))
            connection.sync()
        start = mark(); send('down',1,[205,195])
        wait(lambda: named('selectstart',start))
        s = wait(lambda: latest()['lengths'] == [1,1])
        source = named('selectstart',start)[0]['source']
        check(source['mode'] == 'screen' and source['handedness'] == 'none' and
              all(source[k] for k in ('grip','gamepad','hand')),
              'Physical route creates a transient screen source without controller capabilities')
        check(ray_close(named('selectstart',start)[0]['ray'],[0,0,-1]),
              'Native canvas-center touch has the correct viewer ray')
        check(all(e.get('trusted',True) for e in events(start)), 'Native DOM and XR events are trusted')
        send('up',1); zero()
        names = [e['name'] for e in events(start) if e['name'] in
                 ('pointerdown','sources','selectstart','select','click','selectend','pointerup')]
        check(names == ['pointerdown','sources','selectstart','select','click','selectend','sources','pointerup'],
              'Native touch selects around compatibility click and retires before pointerup')
        selected = named('select',start)[0]; ended = named('selectend',start)[0]
        check(selected['frame'] == ended['frame'] and selected['time'] == ended['time'],
              'Native completion shares one XR action frame and timestamp')
        check(all(ray is None for ray in latest()['latest'][0]['retired']),
              'Native retired sources no longer produce poses')
        start = mark(); send('down',12,[205,195]); wait(lambda: named('selectstart',start))
        send('move',12,[280,195])
        wait(lambda: latest()['latest'][0]['rays'] and
             ray_close(latest()['latest'][0]['rays'][0]['ray'],[.75,0,-1]))
        check(True,'Native motion updates the held XR ray in animation frames')
        send('up',12); zero()
        check(len(named('select',start)) == 1 and not named('click',start),
              'Moved contact completes its XR action without a tap compatibility click')
        start = mark(); send('down',2,[205,195]); send('down',3,[280,195])
        wait(lambda: latest()['lengths'] == [2,2]); settled()
        check(len(named('selectstart',start)) == 1, 'Secondary native finger exposes a source without selecting')
        send('up',2); send('move',3,[260,195]); send('up',3); zero()
        check(len(named('select',start)) == 1 and not named('click',start),
              'Auxiliary finger never promotes and multitouch suppresses tap compatibility clicks')
        start = mark(); send('down',4,[205,195]); wait(lambda: named('selectstart',start))
        send('cancel'); zero()
        check(len(named('selectend',start)) == 1 and not named('select',start),
              'Wayland cancellation retires XR input without selecting')
        check(len(named('pointercancel',start)) == 1, 'Wayland cancellation delivers one DOM pointercancel')
        start = mark(); send('down',5,[205,195]); wait(lambda: named('selectstart',start))
        send('move',5,[850,380]); zero(); send('up',5); settled()
        check(not named('select',start), 'Leaving the native canvas cancels selection')
        start = mark(); command('prevent')
        send('down',6,[205,195]); wait(lambda: named('selectstart',start)); send('up',6); zero()
        check(len(named('select',start)) == 1 and not named('click',start),
              'Prevented touch compatibility mouse leaves native XR selection intact')
        command('allow')
        start = mark(); g = window.get_geometry()
        send('down',7,[20,g.height-12],chrome=True)
        send('down',8,[205,195]); wait(lambda: latest()['lengths'] == [1,1]); settled()
        check(not named('selectstart',start) and named('pointerdown',start)[0]['primary'] is False,
              'A primary contact in browser chrome keeps a page contact auxiliary')
        send('up',8); send('cancel'); zero()
        check(not named('select',start) and not named('click',start),
              'Auxiliary page contact cannot generate selection or a compatibility click')
        start = mark(); send('down',9,[205,195]); wait(lambda: named('selectstart',start))
        key('l',('Control_L',)); zero(); send('up',9); settled()
        check(not named('select',start) and len(named('pointercancel',start)) == 1,
              'Moving keyboard focus into browser chrome cancels the page contact')
        start = mark(); send('down',10,[205,195]); wait(lambda: named('selectstart',start))
        key('t',('Control_L',))
        wait(lambda: latest()['lengths'] == [0,0])
        key('w',('Control_L',)); settled(); send('up',10); zero()
        check(not named('select',start) and len(named('pointercancel',start)) == 1,
              'Tab replacement cancels the old owner and late release never selects after return')
        start = mark(); send('down',11,[205,195]); wait(lambda: named('selectstart',start))
        fixture.wayland.api('/api/action',{'op':'minimize','id':window.id})
        wait(lambda: latest()['lengths'] == [0,0])
        window.set_input_focus(); settled(); send('up',11); zero()
        check(not named('select',start) and len(named('pointercancel',start)) == 1,
              'Native minimization cancels the contact and restoring never revives it')
        start = mark(); send('down',12,[205,195]); wait(lambda: named('selectstart',start))
        send('up',12); zero()
        check(len(named('select',start)) == 1 and len(named('click',start)) == 1,
              'Fresh native tap still works after focus, tab and window cancellation')
        check(child.poll() is None and fixture.wayland.child.poll() is None,
              'Browser and compositor survive all native touch lifecycle cases')
except BaseException as ex:
    error = str(ex)
    raise
finally:
    if child and child.poll() is None:
        child.terminate(); child.wait(timeout=8)
    if server: server.shutdown(); server.server_close()
    if log: log.close()
    if fixture: fixture.close()
    (out/'snapshots.json').write_text(json.dumps(snapshots,indent=2))
    (out/'report.json').write_text(json.dumps({'checks':checks,'error':error,
                                             'passed':error is None and bool(checks)},indent=2))
