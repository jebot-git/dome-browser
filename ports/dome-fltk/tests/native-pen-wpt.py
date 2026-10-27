#!/usr/bin/env python3
"""Run upstream pen WPT cases with an isolated native Wayland action adapter."""
import argparse
import hashlib
import http.server
import json
import mimetypes
import os
from pathlib import Path
import resource
import signal
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit

from wayland_touch_fixture import TouchWayland

signal.pthread_sigmask(signal.SIG_SETMASK, [])
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
if os.environ.get('DOME_PEN_WPT_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_PEN_WPT_BUS': '1'})

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--compositor', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--wpt-root', type=Path, default=Path(__file__).resolve().parents[3] / 'tests/wpt/tests')
args = parser.parse_args()
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=False)
wpt = args.wpt_root.resolve()
cases = [
    'pointerevent_auxclick_is_a_pointerevent.html',
    'pointerevent_setpointercapture_override_pending_capture_element.html',
    'pointerevent_element_haspointercapture_release_pending_capture.html',
    'pointerevent_setpointercapture_to_same_element_twice.html',
    'pointerevent_releasepointercapture_release_right_after_capture.html',
]
adapter = r"""
'use strict';
test_driver_internal.in_automation = true;
test_driver_internal.action_sequence = async actions => {
  for (const source of actions) for (const action of source.actions) {
    if (action.origin instanceof Element) {
      action.origin.scrollIntoView({block:'center',inline:'center'});
      await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
      const rect=action.origin.getBoundingClientRect();
      action.x += (Math.max(0,rect.left)+Math.min(innerWidth,rect.right))/2;
      action.y += (Math.max(0,rect.top)+Math.min(innerHeight,rect.bottom))/2;
      action.origin='viewport';
    }
  }
  const response=await fetch('/native-actions',{method:'POST',body:JSON.stringify({actions,viewport:[innerWidth,innerHeight],ratio:devicePixelRatio})});
  const reply=await response.json();
  if(reply.error)throw new Error(reply.error);
};
add_completion_callback((tests,status) => fetch('/wpt-result',{method:'POST',body:JSON.stringify({status:status.status,message:status.message,tests:tests.map(t=>({name:t.name,status:t.status,message:t.message,stack:t.stack}))})}));
"""
reports = []
sources = {}
actions_log = []
errors = []
result = None
child = fixture = server = log = None


def native_actions(value):
    windows = fixture.wayland.windows()
    window = next(w for w in windows if w.value()['client_pid'] == child.pid)
    geometry = window.get_geometry()
    ratio = value['ratio']
    top = geometry.height - value['viewport'][1] * ratio - 28
    assert sum(source['type'] == 'pointer' for source in value['actions']) <= 1
    pauses = [source for source in value['actions'] if source['type'] == 'none']
    assert all(action['type'] == 'pause' for source in pauses for action in source['actions'])
    for source in value['actions']:
        if source['type'] == 'none':
            continue
        assert source['type'] == 'pointer', source
        assert source['parameters']['pointerType'] == 'pen', source
        in_range = False
        point = [0, 0]
        for tick, action in enumerate(source['actions']):
            duration = max([action.get('duration', 0)] + [p['actions'][tick].get('duration', 0) for p in pauses if tick < len(p['actions'])]) / 1000
            kind = action['type']
            if kind == 'pause':
                time.sleep(max(.06, duration))
                continue
            if kind == 'pointerMove':
                assert action['origin'] == 'viewport', action
                point = [geometry.x + action['x'] * ratio,
                         geometry.y + top + action['y'] * ratio]
                fixture.command('tablet-move' if in_range else 'tablet-in', 0, point)
                in_range = True
            elif kind in ('pointerDown', 'pointerUp'):
                assert in_range
                button = action['button']
                pressed = kind == 'pointerDown'
                if button == 0:
                    fixture.command('tablet-down' if pressed else 'tablet-up', 0)
                else:
                    assert button in (1, 2), action
                    fixture.command('tablet-button', 0, button=332 if button == 1 else 331, pressed=pressed)
            else:
                raise AssertionError(action)
            time.sleep(max(.06, duration))
        if in_range:
            fixture.command('tablet-out', 0)


class Site(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        path = urlsplit(self.path).path
        if path == '/resources/testdriver-vendor.js':
            body = adapter.encode()
            kind = 'text/javascript'
        else:
            file = (wpt / path.lstrip('/')).resolve()
            if not file.is_relative_to(wpt) or not file.is_file():
                self.send_error(404)
                return
            body = file.read_bytes()
            sources[str(file.relative_to(wpt))] = hashlib.sha256(body).hexdigest()
            kind = mimetypes.guess_type(file)[0] or 'application/octet-stream'
        self.send_response(200)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        global result
        value = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        reply = {}
        if self.path == '/wpt-result':
            result = value
        elif self.path == '/native-actions':
            actions_log.append(value)
            try:
                native_actions(value)
            except Exception as error:
                reply = {'error': str(error)}
                errors.append(str(error))
        else:
            self.send_error(404)
            return
        body = json.dumps(reply).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


failure = None
try:
    with tempfile.TemporaryDirectory(prefix='dome-pen-wpt-') as folder:
        runtime = Path(folder)
        runtime.chmod(0o700)
        fixture = TouchWayland(args.compositor.resolve(), runtime, out)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Site)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        for index, case in enumerate(cases):
            result = None
            env = {k: v for k, v in os.environ.items() if not k.startswith('DOME_')}
            env.update(HOME=folder, XDG_RUNTIME_DIR=folder, XDG_CONFIG_HOME=folder+'/config',
                       XDG_DATA_HOME=folder+'/data', XDG_STATE_HOME=folder+'/state',
                       XDG_CACHE_HOME=folder+'/cache', FLTK_BACKEND='wayland',
                       WAYLAND_DISPLAY=fixture.wayland.socket, LIBGL_ALWAYS_SOFTWARE='1',
                       DOME_WEBXR_SOCKET=folder+'/no-host.sock', RUST_LOG='warn')
            for name in ['DISPLAY', 'WAYLAND_SOCKET', 'FLTK_SCALING_FACTOR']:
                env.pop(name, None)
            log = (out / (case + '.log')).open('w')
            url = f'http://127.0.0.1:{server.server_port}/pointerevents/{case}?pen'
            child = subprocess.Popen([str(args.binary.resolve()), '--software', '--no-restore',
                                      '--profile', folder+f'/profile-{index}', url],
                                     env=env, stdout=log, stderr=log)
            deadline = time.monotonic() + 45
            while result is None and time.monotonic() < deadline:
                assert child.poll() is None, ('Unexpected browser exit', child.returncode)
                time.sleep(.05)
            assert result is not None, ('WPT timed out', case)
            reports.append({'case': case, **result})
            print(json.dumps(reports[-1]), flush=True)
            child.terminate()
            child.wait(timeout=8)
            log.close()
        assert not errors, errors
        assert all(r['status'] == 0 and r['tests'] and all(t['status'] == 0 for t in r['tests']) for r in reports), reports
except BaseException as error:
    failure = str(error)
    raise
finally:
    if child and child.poll() is None:
        child.terminate()
        child.wait(timeout=8)
    if server:
        server.shutdown()
        server.server_close()
    if fixture:
        fixture.close()
    if log and not log.closed:
        log.close()
    (out / 'report.json').write_text(json.dumps({'passed': failure is None, 'failure': failure,
        'reports': reports, 'sources': sources, 'native_actions': actions_log,
        'scope': 'Unmodified upstream pen test pages and assertions; native Wayland action adapter; no physical device claim'}, indent=2))
