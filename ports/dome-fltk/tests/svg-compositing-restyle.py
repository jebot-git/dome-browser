#!/usr/bin/env python3
"""Measure SVG compositing after stylesheet changes and restoration."""
import argparse
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
from PIL import Image
from Xlib import X, display, protocol

signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_SVG_COMPOSITING_RESTYLE_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_SVG_COMPOSITING_RESTYLE_BUS': '1'})
source = Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=source/'target/release/dome-browser')
parser.add_argument('--reference-browser', type=Path)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=True)
page = Path(__file__).with_suffix('.html').read_bytes()
phase = 0
reports = []
(out/'fixture.html').write_bytes(page)

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        data = json.dumps(phase).encode() if self.path == '/phase' else page
        self.send_response(200)
        self.send_header('Content-Type', 'application/json' if self.path == '/phase' else 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
        self.send_response(204)
        self.end_headers()

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
app = xvfb = connection = reference = None
results = []
try:
    with tempfile.TemporaryDirectory(prefix='dome-svg-compositing-restyle-') as directory:
        rd, wr = os.pipe()
        xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(wr), '-screen', '0', '1280x900x24', '-nolisten', 'tcp'], pass_fds=(wr,), stderr=subprocess.DEVNULL)
        os.close(wr)
        with os.fdopen(rd) as stream:
            screen = ':'+stream.readline().strip()
        env = {**os.environ, 'DISPLAY': screen, 'FLTK_BACKEND': 'x11', 'LIBGL_ALWAYS_SOFTWARE': '1',
               'XDG_RUNTIME_DIR': directory, 'XDG_CONFIG_HOME': directory+'/config',
               'XDG_CACHE_HOME': directory+'/cache', 'XDG_DATA_HOME': directory+'/data',
               'DOME_BROWSER_FRAME_CAPTURE': str(out/'frame.png')}
        for key in ['WAYLAND_DISPLAY', 'WAYLAND_SOCKET']:
            env.pop(key, None)
        with (out/'browser.log').open('w') as log:
            if args.reference_browser:
                from reference_browser import ReferenceBrowser
                reference = ReferenceBrowser(args.reference_browser.resolve(), directory, 'about:blank', env, log)
                reference.command('Emulation.setDeviceMetricsOverride', {'width':1280,'height':800,'deviceScaleFactor':1,'mobile':False})
                reference.command('Page.navigate', {'url':f'http://127.0.0.1:{server.server_port}/'})
            else:
                app = subprocess.Popen([str(args.binary.resolve()), '--software', '--profile', directory+'/profile',
                                        f'http://127.0.0.1:{server.server_port}/'], env=env, stdout=log, stderr=log, start_new_session=True)
        for requested in range(3):
            phase = requested
            deadline = time.monotonic()+20
            while not any(r['phase'] == requested for r in reports):
                assert (app is None or app.poll() is None) and time.monotonic() < deadline, ('Missing phase', requested, reports)
                time.sleep(.05)
            expected = [[137,217,172],[255,255,255],[255,255,255],[255,255,255],[20,100,180],[137,217,172],[255,255,255],[255,255,255]] if requested == 1 else [[20,180,90]]*8
            deadline = time.monotonic()+2
            while True:
                try:
                    if reference is not None:
                        shot = reference.command('Page.captureScreenshot', {'format':'png'})
                        (out/'frame.png').write_bytes(base64.b64decode(shot['data']))
                    frame = Image.open(out/'frame.png').convert('RGB')
                    samples = [list(frame.getpixel(point)) for point in [(50,50),(130,50),(210,50),(305,50),(355,50),(450,50),(50,150),(130,150)]]
                    if all(abs(a-b)<=2 for pixel,wanted in zip(samples,expected) for a,b in zip(pixel,wanted)) or time.monotonic() >= deadline:
                        break
                except OSError:
                    assert time.monotonic() < deadline, 'No frame captured'
                time.sleep(.05)
            frame.save(out/f'phase-{requested}.png')
            checks = {name: all(abs(a-b)<=2 for a,b in zip(actual,wanted)) for name, actual, wanted in zip(
                ['opacity','display','visibility','clip path','paint order','root opacity','root attribute visibility override','root CSS visibility override'], samples, expected)}
            results.append({'phase': requested, 'checks': checks, 'pixels': samples, 'expected': expected})
        if reference is not None:
            closed = reference.close()
            reference = None
            exit_code = 0 if closed else 1
        else:
            connection = display.Display(screen)
            for window in connection.screen().root.query_tree().children:
                try:
                    if window.get_wm_name() and window.get_attributes().map_state == X.IsViewable:
                        window.send_event(protocol.event.ClientMessage(window=window, client_type=connection.intern_atom('WM_PROTOCOLS'),
                            data=(32, [connection.intern_atom('WM_DELETE_WINDOW'), X.CurrentTime, 0, 0, 0])), event_mask=0)
                except Exception:
                    pass
            connection.sync()
            app.wait(timeout=10)
            exit_code = app.returncode
        record = {'phases': results, 'dom_reports': reports, 'exit_code': exit_code,
                  'binary_sha256': hashlib.sha256((args.reference_browser or args.binary).read_bytes()).hexdigest(),
                  'fixture_sha256': hashlib.sha256(page).hexdigest()}
        (out/'report.json').write_text(json.dumps(record, indent=2)+'\n')
        for result in results:
            for name, passed in result['checks'].items():
                print(('PASS ' if passed else 'FAIL ')+f"phase {result['phase']}: {name}", flush=True)
        assert exit_code == 0
        assert all(all(r['checks'].values()) for r in results), results
finally:
    if reference is not None:
        reference.close()
    if app is not None and app.poll() is None:
        os.killpg(app.pid, signal.SIGTERM)
        app.wait(timeout=5)
    if connection is not None:
        connection.close()
    if xvfb is not None:
        xvfb.terminate()
        xvfb.wait(timeout=5)
    server.shutdown()
    server.server_close()
