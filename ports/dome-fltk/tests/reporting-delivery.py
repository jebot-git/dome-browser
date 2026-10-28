#!/usr/bin/env python3
"""Check local Reporting delivery outcomes, metadata and document/worker ownership."""
from pathlib import Path
import argparse, contextlib, datetime, hashlib, http.server, json, os, resource, shutil
import signal, subprocess, sys, tempfile, threading, time
from urllib.parse import urlsplit, parse_qs

signal.pthread_sigmask(signal.SIG_SETMASK, [])
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
if os.environ.get('DOME_REPORT_DELIVERY_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
              {**os.environ, 'DOME_REPORT_DELIVERY_TEST_BUS': '1'})
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--cases', nargs='+')
a = p.parse_args()
HERE = Path(__file__).resolve().parent
W = next((path for path in HERE.parents if (path / 'components/script').is_dir()), Path.cwd())
T = W / 'ports/dome-fltk/tests'
assert (T / 'private_wayland.py').is_file(), 'Run from the Servo checkout root'
sys.path.insert(0, str(T))
from Xlib import display
from private_wayland import PrivateWayland

out = a.output.resolve()
out.mkdir(mode=0o700, parents=True, exist_ok=False)
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
fixture = (HERE / 'reporting-delivery.html').read_text()
worker = (HERE / 'reporting-delivery-worker.js').read_bytes()
sources = [Path(__file__), HERE / 'reporting-delivery.html', HERE / 'reporting-delivery-worker.js']
source_hashes = {path.name: sha(path) for path in sources}
(out / 'fixture-source').mkdir()
for path in sources:
    shutil.copy2(path, out / 'fixture-source' / path.name)
cases = {
    'success': dict(status=204, expected=2),
    'gone': dict(status=410, expected=1),
    'failure': dict(status=500, expected=2),
    'client-error': dict(status=400, expected=2),
    'ua': dict(status=204, expected=2, ua=True),
    'ua-snapshot': dict(status=204, expected=2, ua=True),
    'name-isolation': dict(status=410, expected=2),
    'worker-success': dict(status=204, expected=2, ua=True),
    'worker-gone': dict(status=410, expected=1),
    'worker-failure': dict(status=500, expected=2),
    'worker-terminate': dict(status=410, expected=1),
    'global-isolation': dict(status=410, expected=3),
    'document-removal': dict(status=410, expected=3),
    'cors-success': dict(status=204, expected=2, cors=True),
    'cors-gone': dict(status=410, expected=1, cors=True),
    'cors-denied': dict(status=410, expected=2, cors=True),
}
if a.cases:
    assert all(name in cases for name in a.cases)
    cases = {name: cases[name] for name in a.cases}
results, deliveries, requests, completed, errors = {}, {}, {}, {}, []
servers = []

def stop(proc):
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill(); proc.wait()

class FixtureServer(http.server.ThreadingHTTPServer):
    request_queue_size = 256
    daemon_threads = True

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_OPTIONS(self):
        self.send_response(204)
        self.send_header('Access-Control-Allow-Origin', self.headers.get('Origin', 'null'))
        self.send_header('Access-Control-Allow-Methods', 'POST')
        self.send_header('Access-Control-Allow-Headers', 'content-type')
        self.send_header('Access-Control-Max-Age', '0')
        self.end_headers()

    def do_GET(self):
        url = urlsplit(self.path)
        query = parse_qs(url.query)
        name = query.get('case', [''])[0]
        if url.path == '/__counts':
            body = json.dumps({'total': len(deliveries.get(name, [])),
                               'completed': completed.get(name, 0)}).encode()
            kind = 'application/json'
        elif url.path in ['/fixture', '/child', '/worker.js'] and name in cases:
            cfg = dict(cases[name], case=name)
            slot = query.get('slot', [''])[0]
            if slot:
                cfg['slot'] = slot
            body = worker if url.path == '/worker.js' else fixture.replace('CONFIGURATION', json.dumps(cfg)).encode()
            kind = 'text/javascript' if url.path == '/worker.js' else 'text/html'
        else:
            self.send_error(404); return
        self.send_response(200)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(body)))
        if url.path in ['/fixture', '/child', '/worker.js']:
            self.send_header('Permissions-Policy', ('gamepad=(self)' if url.path == '/fixture' and name in ['global-isolation', 'document-removal'] else 'gamepad=()') + ';report-to=collector')
            self.send_header('Content-Security-Policy',
                             "script-src 'self';report-to collector" if url.path == '/worker.js'
                             else "img-src 'none';report-to other")
            endpoint = f'/__endpoint?case={name}'
            if cases[name].get('cors'):
                endpoint = f'http://127.0.0.1:{servers[1].server_port}' + endpoint
            self.send_header('Reporting-Endpoints', f'collector="{endpoint}", other="{endpoint}"')
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_POST(self):
        url = urlsplit(self.path)
        name = parse_qs(url.query).get('case', [''])[0]
        try:
            data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if url.path == '/__result':
                results[name] = data
                self.send_response(204); self.end_headers(); return
            assert url.path == '/__endpoint' and name in cases
            assert self.headers.get_content_type() == 'application/reports+json' and isinstance(data, list)
            deliveries.setdefault(name, []).extend(data)
            requests.setdefault(name, []).append({
                'reports': len(data), 'cookie': self.headers.get('Cookie'),
                'origin': self.headers.get('Origin'), 'referer': self.headers.get('Referer')})
            code = cases[name]['status']
            if name == 'name-isolation' and all(report['type'] == 'csp-violation' for report in data):
                code = 204
            if name in ['global-isolation', 'document-removal']:
                slot = parse_qs(urlsplit(data[0]['url']).query).get('slot', [''])[0]
                code = 410 if slot == 'a' else 204
            if name == 'worker-terminate' or name == 'document-removal' and code == 410:
                time.sleep(.6)
            self.send_response(code)
            if cases[name].get('cors') and name != 'cors-denied':
                self.send_header('Access-Control-Allow-Origin', self.headers.get('Origin', 'null'))
            self.end_headers()
            completed[name] = completed.get(name, 0) + len(data)
        except (BrokenPipeError, ConnectionResetError):
            # Removal/termination can cancel the disposable document's request.
            completed[name] = completed.get(name, 0) + len(data)
        except Exception as error:
            errors.append(str(error)); self.send_error(400)

runs = []
with contextlib.ExitStack() as stack:
    root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-report-delivery-')))
    root.chmod(0o700)
    rd, wr = os.pipe()
    xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(wr), '-screen', '0', '1200x900x24', '-nolisten', 'tcp'],
                            pass_fds=(wr,), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    stack.callback(stop, xvfb); os.close(wr)
    screen = ':' + os.read(rd, 64).decode().strip(); os.close(rd)
    connection = display.Display(screen); stack.callback(connection.close)
    wayland = PrivateWayland(connection, screen, root, out); stack.callback(wayland.close)
    for _ in range(2):
        server = FixtureServer(('127.0.0.1', 0), Handler); servers.append(server)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close); stack.callback(server.shutdown)
    env = {k: v for k, v in os.environ.items() if not k.startswith(('DOME_', 'SERVO_', 'XR_INPUT_'))
           and k not in ['WAYLAND_DISPLAY', 'WAYLAND_SOCKET']}
    env.update(HOME=str(root), DISPLAY=screen, WAYLAND_DISPLAY=wayland.socket, XDG_RUNTIME_DIR=str(root),
               XDG_CONFIG_HOME=str(root / 'config'), XDG_DATA_HOME=str(root / 'data'),
               XDG_CACHE_HOME=str(root / 'cache'), LIBGL_ALWAYS_SOFTWARE='1',
               FONTCONFIG_FILE='/etc/fonts/fonts.conf', RUST_LOG='warn',
               XR_INPUT_POLICY_TEST='1', DOME_GRAPHICS_LOSS_TEST='1')
    for index, (name, cfg) in enumerate(cases.items()):
        env['XR_INPUT_PROFILE'] = str(root / f'profile-{index}')
        with (out / (name + '.log')).open('w') as log:
            proc = subprocess.Popen([str(a.binary), f'http://127.0.0.1:{servers[0].server_port}/fixture?case={name}'],
                                    stdin=subprocess.PIPE, stdout=log, stderr=log, text=True, env=env)
            stack.callback(stop, proc)
            deadline = time.monotonic() + 25
            while name not in results and proc.poll() is None and time.monotonic() < deadline:
                time.sleep(.025)
            row = results.get(name, {'checks': [], 'reports': [], 'error': 'No fixture result within deadline'})
            time.sleep(.2); live = proc.poll() is None
            if live:
                proc.stdin.write('{"op":"quit","token":1}\n'); proc.stdin.flush()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                stop(proc)
        wire = deliveries.get(name, [])
        types = {kind: sum(report.get('type') == kind for report in wire)
                 for kind in ['permissions-policy-violation', 'csp-violation']}
        schema = bool(wire) and all(isinstance(report.get('age'), int) and report['age'] >= 0
                                   and isinstance(report.get('body'), dict) for report in wire)
        expected_types = ({'permissions-policy-violation': 1, 'csp-violation': 1} if name == 'name-isolation'
                          else {'permissions-policy-violation': 0, 'csp-violation': cfg['expected']}
                          if name.startswith('worker-') else
                          {'permissions-policy-violation': cfg['expected'], 'csp-violation': 0})
        expected_agents = row.get('agents', [])[:cfg['expected']]
        agent_ok = not cfg.get('ua') or (len(expected_agents) == len(wire)
                    and sorted(report.get('user_agent', '') for report in wire) == sorted(expected_agents))
        reqs = requests.get(name, [])
        cookies_ok = all(not row['cookie'] if cfg.get('cors') else row['cookie'] == 'delivery_fixture=one'
                         for row in reqs)
        row['checks'] += [
            {'name': 'Collector outcomes preserve exact per-type delivery counts',
             'passed': len(wire) == cfg['expected'] and types == expected_types},
            {'name': 'Endpoint schema and report age are valid', 'passed': schema},
            {'name': 'Serialized User-Agent matches report creation snapshots', 'passed': agent_ok},
            {'name': 'Same-origin credentials and cross-origin omission preserve cookie controls',
             'passed': bool(reqs) and cookies_ok},
            {'name': 'Reporting requests omit a referrer', 'passed': bool(reqs) and all(not row['referer'] for row in reqs)},
            {'name': 'Browser remains live until requested exit', 'passed': live},
            {'name': 'Browser closes cleanly', 'passed': proc.returncode == 0}]
        row.update(name=name, exit_code=proc.returncode, received_reports=len(wire), report_types=types,
                   user_agents=[report.get('user_agent') for report in wire], requests=reqs)
        row['passed'] = not row.get('error') and all(check['passed'] for check in row['checks'])
        runs.append(row)
        print(name, 'PASS' if row['passed'] else 'FAIL', len(row['checks']), row.get('error'), types, flush=True)
assert source_hashes == {path.name: sha(path) for path in sources}, 'Fixture changed during runtime'
record = dict(binary=str(a.binary), binary_sha256=sha(a.binary), source_sha256=source_hashes,
              runs=runs, request_errors=errors, runtime_checks=sum(len(row['checks']) for row in runs),
              passed=not errors and all(row['passed'] for row in runs),
              finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
              scope='Private local reporting delivery/metadata checks using restricted gamepad calls and CSP; temporary profiles, software rendering, no controller gestures or geolocation. No whole-API conformance claim.')
(out / 'report.json').write_text(json.dumps(record, indent=2) + '\n')
assert record['passed'], 'Reporting delivery lifecycle regression failed'
