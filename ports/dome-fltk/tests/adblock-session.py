#!/usr/bin/env python3
"""Stress cosmetic/privacy filtering, DOM churn and document teardown in one private session."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit

signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_ADBLOCK_SESSION_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
              {**os.environ, 'DOME_ADBLOCK_SESSION_BUS': '1'})
from Xlib import X, display, protocol

root = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--seconds', type=int, default=300)
p.add_argument('--disable-adblock', action='store_true')
p.add_argument('--default-lists', action='store_true', help='Use bundled EasyList/EasyPrivacy in addition to explicit fixture controls')
a = p.parse_args()
assert 30 <= a.seconds <= 1800
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False, mode=0o700)
binary = a.binary.resolve()
page = Path(__file__).with_suffix('.html').read_text().replace('__DURATION_MS__', str(a.seconds * 1000)).replace('__FILTER_ENABLED__', 'false' if a.disable_adblock else 'true').encode()
(out / 'fixture.html').write_bytes(page)
reports, requests, checkpoints, samples = [], [], [], []
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self):
        path = urlsplit(self.path).path
        requests.append({'path': path, 'time': time.monotonic()})
        data = b'positive fixture response' if path.endswith('probe') else page
        self.send_response(200)
        self.send_header('Content-Type', 'text/plain' if path.endswith('probe') else 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Content-Security-Policy', "default-src 'self'; script-src 'nonce-fixture'; style-src 'nonce-fixture'; frame-src 'self'")
        self.end_headers()
        try: self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError): pass
    def do_POST(self):
        path = urlsplit(self.path).path
        data = self.rfile.read(int(self.headers.get('Content-Length', '0')))
        if path == '/report': reports.append(json.loads(data))
        elif path == '/checkpoint':
            checkpoints.append(json.loads(data))
            (out / 'checkpoints.json').write_text(json.dumps(checkpoints, indent=2) + '\n')
        else: requests.append({'path': path, 'time': time.monotonic()})
        self.send_response(204)
        self.end_headers()
# EasyList exempts127.0.0.1 fromgeneric hiding; another loopback address
# exercises the real generic rules without modifying list policy.
server = http.server.ThreadingHTTPServer(('127.0.0.2' if a.default_lists else '127.0.0.1', 0), Handler)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
app = xvfb = xd = None
try:
    with tempfile.TemporaryDirectory(prefix='dome-adblock-session-') as directory:
        runtime = Path(directory)
        runtime.chmod(0o700)
        profile = runtime / 'profile'
        profile.mkdir(mode=0o700)
        if not a.default_lists: (profile / 'filterlists.json').write_text('[]')
        (profile / 'adblock.txt').write_text('##.ad\n##.shadow-ad\n###late-ad\n/privacy-probe$xmlhttprequest\n/privacy-beacon$ping\n')
        if a.disable_adblock: (profile / 'adblock-disabled').write_text('private stress control\n')
        rd, wr = os.pipe()
        xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(wr), '-screen', '0', '1280x900x24', '-nolisten', 'tcp'], pass_fds=(wr,), stderr=subprocess.DEVNULL)
        os.close(wr)
        with os.fdopen(rd) as stream: screen = ':' + stream.readline().strip()
        xd = display.Display(screen)
        env = {**os.environ, 'DISPLAY': screen, 'FLTK_BACKEND': 'x11', 'LIBGL_ALWAYS_SOFTWARE': '1', 'XDG_RUNTIME_DIR': directory,
               'XDG_CONFIG_HOME': directory + '/config', 'XDG_DATA_HOME': directory + '/data', 'XDG_CACHE_HOME': directory + '/cache', 'RUST_LOG': 'warn'}
        for key in ['WAYLAND_DISPLAY', 'WAYLAND_SOCKET']: env.pop(key, None)
        with (out / 'browser.log').open('w') as log:
            app = subprocess.Popen([str(binary), '--software', '--no-restore', '--profile', str(profile), f'http://{server.server_address[0]}:{server.server_port}/'], env=env, stdout=log, stderr=log, start_new_session=True)
            started = time.monotonic()
            while not reports and app.poll() is None and time.monotonic() - started < a.seconds + 90:
                memory = {}
                try:
                    for line in Path(f'/proc/{app.pid}/smaps_rollup').read_text().splitlines():
                        fields = line.split()
                        if len(fields) > 1 and fields[0] in ['Rss:', 'Pss:', 'Private_Dirty:']:
                            memory[fields[0][:-1] + '_kib'] = int(fields[1])
                    samples.append({'elapsed': round(time.monotonic() - started, 3), **memory})
                except OSError: pass
                time.sleep(.5)
            (out / 'samples.json').write_text(json.dumps(samples, indent=2) + '\n')
            assert reports, ('no fixture result', app.poll())
            for w in xd.screen().root.query_tree().children:
                try:
                    if w.get_wm_name() and w.get_attributes().map_state == X.IsViewable:
                        w.send_event(protocol.event.ClientMessage(window=w, client_type=xd.intern_atom('WM_PROTOCOLS'), data=(32, [xd.intern_atom('WM_DELETE_WINDOW'), X.CurrentTime, 0, 0, 0])), event_mask=0)
                except Exception: pass
            xd.sync()
            app.wait(timeout=15)
            result = reports[-1]
            hits = {path: sum(r['path'] == path for r in requests) for path in ['/privacy-probe', '/privacy-beacon', '/legitimate-probe']}
            expected = result['networkCycles']
            result['checks'].extend([
                {'name': 'network privacy probes have expected server counts', 'passed': hits['/privacy-probe'] == (expected if a.disable_adblock else 0) and hits['/privacy-beacon'] == (expected if a.disable_adblock else 0), 'error': str(hits)},
                {'name': 'legitimate network probes all arrive', 'passed': hits['/legitimate-probe'] == expected, 'error': str(hits)},
                {'name': 'browser closes cleanly after sustained churn', 'passed': app.returncode == 0, 'error': str(app.returncode)},
            ])
            first = [s['Rss_kib'] for s in samples if 10 <= s['elapsed'] <= 20 and 'Rss_kib' in s]
            last = [s['Rss_kib'] for s in samples if s['elapsed'] >= a.seconds - 10 and 'Rss_kib' in s]
            result.update(binary_sha256=hashlib.file_digest(binary.open('rb'), 'sha256').hexdigest(), fixture_sha256=hashlib.sha256(page).hexdigest(), seconds=a.seconds,
                          disable_adblock=a.disable_adblock, default_lists=a.default_lists, exit_code=app.returncode, server_hits=hits,
                          rss_growth_kib=statistics.median(last) - statistics.median(first) if first and last else None,
                          scope='Private profile/bus/X server, software rendering. Memory and frame timings are diagnostics, not leak-free or conformance certification.')
            (out / 'report.json').write_text(json.dumps(result, indent=2) + '\n')
            for c in result['checks']: print(('PASS ' if c['passed'] else 'FAIL ') + c['name'] + ' ' + str(c.get('error', '')), flush=True)
            print(json.dumps({k: result.get(k) for k in ['frames', 'medianGap', 'p95Gap', 'maxGap', 'rss_growth_kib', 'server_hits']}), flush=True)
            assert all(c['passed'] for c in result['checks'])
finally:
    (out / 'requests.json').write_text(json.dumps(requests, indent=2) + '\n')
    (out / 'checkpoints.json').write_text(json.dumps(checkpoints, indent=2) + '\n')
    if app and app.poll() is None: os.killpg(app.pid, signal.SIGKILL); app.wait(timeout=5)
    if xd: xd.close()
    if xvfb:
        xvfb.terminate()
        xvfb.wait(timeout=5)
    server.shutdown()
