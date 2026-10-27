#!/usr/bin/env python3
"""Measure SVG gradient-stop cascade, opacity and repeated paint changes."""
import argparse
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
if os.environ.get('DOME_SVG_STOPS_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_SVG_STOPS_BUS': '1'})
source = Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=source/'target/release/dome-browser')
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=True)
page = Path(__file__).with_suffix('.html').read_bytes()
phase = 0
reports = []

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
app = xvfb = connection = None
results = []
try:
    with tempfile.TemporaryDirectory(prefix='dome-svg-stops-') as directory:
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
            app = subprocess.Popen([str(args.binary.resolve()), '--software', '--profile', directory+'/profile',
                                    f'http://127.0.0.1:{server.server_port}/'], env=env, stdout=log, stderr=log, start_new_session=True)
        for requested in range(3):
            phase = requested
            deadline = time.monotonic()+20
            while not any(r['phase'] == requested for r in reports):
                assert app.poll() is None and time.monotonic() < deadline, ('Missing phase', requested, reports)
                time.sleep(.05)
            green = [20,180,90]
            half = [138,218,173]
            expected = [green]*4 + [[0,0,0]] + [green]*2 + [half]*2 + [[196,236,214], [255,255,255], green, half,
                        green if requested == 1 else [255,0,0], half if requested == 1 else green]
            deadline = time.monotonic()+2
            while True:
                try:
                    frame = Image.open(out/'frame.png').convert('RGB')
                    samples = [list(frame.getpixel((i%8*100+50, i//8*100+50))) for i in range(15)]
                    if all(max(abs(a-b) for a,b in zip(actual,wanted)) <= 1 for actual,wanted in zip(samples,expected)) or time.monotonic() >= deadline:
                        break
                except OSError:
                    assert time.monotonic() < deadline, 'No frame captured'
                time.sleep(.05)
            frame.save(out/f'phase-{requested}.png')
            checks = {name: max(abs(a-b) for a,b in zip(actual,wanted)) <= 1 for name, actual, wanted in zip(
                ['presentation attribute','stylesheet overrides attribute','important stylesheet overrides inline',
                 'important inline wins','non-inherited defaults','explicit inherit','currentColor',
                 'CSS opacity','attribute opacity','color alpha times stop opacity','negative opacity clamps',
                 'high opacity clamps','percentage opacity','custom property repaint','CSSOM opacity repaint'], samples, expected)}
            results.append({'phase': requested, 'checks': checks, 'pixels': samples, 'expected': expected})
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
        computed_checks = []
        for report in reports:
            expected_colors = ['rgb(20,180,90)']*15
            expected_colors[4] = 'rgb(0,0,0)'
            expected_colors[9] = 'rgba(20,180,90,0.5)'
            expected_colors[13] = 'rgb(20,180,90)' if report['phase'] == 1 else 'rgb(255,0,0)'
            expected_opacity = [1]*7 + [.5,.5,.5,0,1,.5,1,.5 if report['phase'] == 1 else 1]
            values = []
            for value, color, opacity in zip(report['values'], expected_colors, expected_opacity):
                try:
                    opacity_matches = float(value.get('opacity', '')) == opacity
                except (ValueError, TypeError):
                    opacity_matches = False
                values.append({'id': value['id'],
                               'color': value.get('color', '').replace(' ', '') == color,
                               'opacity': opacity_matches})
            computed_checks.append({'phase': report['phase'], 'values': values})
        record = {'phases': results, 'computed_checks': computed_checks, 'dom_reports': reports, 'exit_code': app.returncode,
                  'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                  'fixture_sha256': hashlib.sha256(page).hexdigest()}
        (out/'report.json').write_text(json.dumps(record, indent=2)+'\n')
        for result in results:
            for name, passed in result['checks'].items():
                print(('PASS ' if passed else 'FAIL ')+f"phase {result['phase']}: {name}", flush=True)
        assert app.returncode == 0
        assert all(all(v['color'] and v['opacity'] for v in r['values']) for r in computed_checks), computed_checks
        assert all(r['supports'] for r in reports), 'CSS.supports did not expose gradient-stop properties'
        assert all(all(r['checks'].values()) for r in results), results
finally:
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
