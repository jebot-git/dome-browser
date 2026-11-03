#!/usr/bin/env python3
"""Run a local rendering fixture on the current native Wayland test desktop.

Uses a private browser profile and session bus. Closes only its own window;
it neither requires X11 nor changes desktop settings or application defaults.
"""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import threading
import time
import urllib.request

signal.pthread_sigmask(signal.SIG_SETMASK, [])
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--fixture', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--timeout', type=float, default=60)
args = parser.parse_args()
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=True)
reports = []
page = args.fixture.read_bytes()
(out / 'fixture.html').write_bytes(page)

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(page)))
        self.end_headers()
        self.wfile.write(page)

    def do_POST(self):
        reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
        self.send_response(204)
        self.end_headers()

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
runtime = Path(os.environ['XDG_RUNTIME_DIR'])
display = os.environ['WAYLAND_DISPLAY']
session = json.loads((runtime / (display + '.dome-session.json')).read_text())
base = f'http://127.0.0.1:{session["control_port"]}'
env = dict(os.environ, RUST_BACKTRACE='1', DOME_BROWSER_ERROR_TRACE='1',
           DOME_BROWSER_FRAME_CAPTURE=str(out / 'frame.png'))
proc = None
try:
    with (out / 'browser.log').open('w') as log:
        proc = subprocess.Popen(['dbus-run-session', '--', str(args.binary.resolve()),
            '--profile', str(out / 'profile'), '--no-restore',
            f'http://127.0.0.1:{server.server_port}/'], env=env, stdout=log,
            stderr=subprocess.STDOUT, start_new_session=True)
        deadline = time.monotonic() + args.timeout
        while not reports and time.monotonic() < deadline and proc.poll() is None:
            time.sleep(.1)
        assert reports, 'No fixture report; inspect browser.log'
        assert all(c['passed'] for r in reports for c in r['checks']), reports
        print('PASS', len(reports[0]['checks']), 'native rendering checks', flush=True)
finally:
    if proc is not None and proc.poll() is None:
        with urllib.request.urlopen(base + '/api/state', timeout=3) as response:
            windows = json.load(response)['scene']['windows']
        for window in windows:
            try:
                owned = os.getpgid(window.get('client_pid', 0)) == proc.pid
            except ProcessLookupError:
                owned = False
            if owned:
                request = urllib.request.Request(base + '/api/action',
                    json.dumps({'op':'close', 'id':window['id']}).encode(),
                    headers={'Content-Type':'application/json'})
                with urllib.request.urlopen(request, timeout=3) as response:
                    response.read()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGTERM)
            proc.wait(timeout=10)
    server.shutdown()
    result = dict(reports=reports, exit_code=proc.returncode if proc else None,
                  binary_sha256=hashlib.sha256(args.binary.read_bytes()).hexdigest())
    (out / 'report.json').write_text(json.dumps(result, indent=2) + '\n')
