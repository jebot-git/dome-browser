#!/usr/bin/env python3
"""Real wl_touch delivery into the staged FLTK toolkit, in a private session."""
import argparse, json, math, os, resource, signal, subprocess, sys, tempfile, time
from pathlib import Path
from wayland_touch_fixture import TouchWayland

signal.pthread_sigmask(signal.SIG_SETMASK, [])
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
if os.environ.get('DOME_TOUCH_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_TOUCH_TEST_BUS': '1'})
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--compositor', type=Path, required=True)
p.add_argument('--probe', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--scale', type=float, default=1)
args = p.parse_args()
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=False)
checks = []
child = fixture = log = None

def events():
    return [json.loads(line[len('TOUCH_PROBE '):]) for line in
            (out/'probe.log').read_text().splitlines() if line.startswith('TOUCH_PROBE ')]

def wait(fn):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        assert child.poll() is None, 'Probe exited unexpectedly'
        result = fn()
        if result:
            return result
        time.sleep(.03)
    raise AssertionError('Probe timed out')

def check(value, name):
    checks.append({'name': name, 'passed': bool(value)})
    print(('PASS ' if value else 'FAIL ') + name, flush=True)
    assert value, name

try:
    with tempfile.TemporaryDirectory(prefix='dome-native-touch-') as folder:
        runtime = Path(folder)
        runtime.chmod(0o700)
        fixture = TouchWayland(args.compositor.resolve(), runtime, out)
        env = {**os.environ, 'HOME': folder, 'XDG_RUNTIME_DIR': folder,
               'XDG_CONFIG_HOME': folder+'/config', 'WAYLAND_DISPLAY': fixture.wayland.socket,
               'FLTK_BACKEND': 'wayland', 'LIBGL_ALWAYS_SOFTWARE': '1',
               'FLTK_SCALING_FACTOR': str(args.scale)}
        env.pop('DISPLAY', None)
        env.pop('WAYLAND_SOCKET', None)
        log = (out/'probe.log').open('w')
        child = subprocess.Popen([str(args.probe.resolve())], env=env, stdout=log, stderr=log)
        wait(lambda: any(e.get('ready') for e in events()))
        window = fixture.wayland.wait(lambda: next((w for w in fixture.wayland.windows()
                                        if w.value()['client_pid'] == child.pid), None))
        window.set_input_focus()
        geometry = window.get_geometry()
        (out/'window.json').write_text(json.dumps(window.value(), indent=2))
        scale = geometry.width / 480
        check(abs(scale-args.scale) < .02, 'Requested toolkit scale is active')
        def send(action, slot=None, point=None, frame=True):
            if point is not None:
                point = [geometry.x+point[0]*scale, geometry.y+point[1]*scale]
            fixture.command(action, slot, point)
            if frame:
                fixture.command('frame')
            time.sleep(.05)
        def raw(after=0):
            return [e for e in events()[after:] if 'action' in e]
        before = len(events())
        send('down', 1, [120.25, 130.5], frame=False)
        check(not raw(before), 'Down is deferred until wl_touch.frame')
        fixture.command('frame')
        first = wait(lambda: raw(before))[-1]
        check(first['action'] == 0 and first['id'] == 1 and first['primary'] == 1,
              'First native contact has stable primary identity')
        check(abs(first['x']-120.25) < .01 and abs(first['y']-130.5) < .01,
              'Fractional native coordinates retain precision at toolkit scale')
        send('move', 1, [150.75, 160.25])
        send('up', 1)
        check([e['action'] for e in raw(before)] == [0, 1, 2],
              'Native down, motion and up arrive in order')
        check(not any('mouse' in e for e in events()[before:]),
              'Owned raw contacts do not synthesize mouse input')
        before = len(events())
        send('down', 2, [110, 130]); send('down', 3, [170, 150])
        send('up', 2); send('move', 3, [180, 155]); send('up', 3)
        contacts = raw(before)
        check(all(e['primary'] == (e['id'] == 2) for e in contacts),
              'Secondary finger never promotes after primary release')
        before = len(events())
        send('down', 4, [110, 130], False); send('up', 4, frame=False)
        send('down', 4, [170, 150], False); send('up', 4)
        check([(e['action'], e['primary']) for e in raw(before)] == [(0,1),(2,1),(0,1),(2,1)],
              'Contact IDs can be reused within a single Wayland frame')
        before = len(events())
        send('down', 5, [110, 130], False); send('cancel')
        check(not raw(before), 'Cancellation discards undelivered contacts')
        before = len(events())
        send('down', 6, [110, 130]); send('cancel'); send('up', 6)
        check([e['action'] for e in raw(before)] == [0, 3],
              'Cancellation ends delivered contact once and ignores late up')
        before = len(events())
        send('down', 7, [110, 130]); send('capability-off')
        wait(lambda: any(e['action'] == 3 for e in raw(before)))
        check([e['action'] for e in raw(before)] == [0, 3],
              'Removing seat touch capability cancels active contacts')
        send('capability-on'); time.sleep(.15)
        before = len(events())
        send('down', 8, [110, 130]); send('up', 8)
        check([e['action'] for e in raw(before)] == [0, 2],
              'Re-added seat capability starts a fresh gesture')
        before = len(events())
        send('down', 9, [80, 35]); send('up', 9)
        check(sum(bool(e.get('clicked')) for e in events()[before:]) == 1,
              'Unclaimed primary contact activates native FLTK button once')
        before = len(events())
        send('down', 10, [80, 35]); send('cancel')
        check(not any(e.get('clicked') for e in events()[before:]),
              'Cancelled native button gesture never activates the button')
        before = len(events())
        send('down', 99, [110, 130], False)
        send('down', 100, [170, 150])
        check([(e['id'],e['action']) for e in raw(before)] == [(99,0),(99,3)],
              'Reentrant handler replacement invalidates remaining events in the frame')
        send('cancel')
        before = len(events())
        send('down', 101, [110, 130]); send('up', 101, frame=False); send('cancel')
        check([e['action'] for e in raw(before)] == [0,3],
              'Cancellation clears an unframed up without delivering stale completion')
        before = len(events())
        send('down', 11, [110, 130])
        fixture.wayland.api('/api/action', {'op':'minimize', 'id':window.id})
        wait(lambda: any(e['action'] == 3 for e in raw(before)))
        check([e['action'] for e in raw(before)] == [0, 3],
              'Hiding the owning surface cancels its active contact')
        check(child.poll() is None and fixture.wayland.child.poll() is None,
              'Toolkit and compositor remain alive after native lifecycle tests')
finally:
    if child and child.poll() is None:
        child.terminate(); child.wait(timeout=5)
    if log:
        log.close()
    if fixture:
        fixture.close()
    (out/'report.json').write_text(json.dumps({'checks':checks, 'scale':args.scale,
                                             'passed':bool(checks) and all(c['passed'] for c in checks)}, indent=2))
