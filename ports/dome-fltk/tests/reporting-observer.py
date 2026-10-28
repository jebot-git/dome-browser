#!/usr/bin/env python3
"""Validate local CSP reporting delivery and observer lifecycle with xr-native-input.

Run through uv with python-xlib and a private DOME_COMPOSITOR fixture.
No live user profile or XR device is used.
"""
from pathlib import Path
import argparse, contextlib, datetime, hashlib, http.server, json, os, resource, signal, subprocess, sys, tempfile, threading, time
from urllib.parse import urlsplit, parse_qs
signal.pthread_sigmask(signal.SIG_SETMASK, [])
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
if os.environ.get('DOME_REPORTING_VALIDATION_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv], {**os.environ, 'DOME_REPORTING_VALIDATION_BUS': '1'})
parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True, type=Path)
parser.add_argument('--output', required=True, type=Path)
parser.add_argument('--plain-endpoint', action='store_true', help='Use a simple endpoint for comparison with older parsers')
parser.add_argument('--cases', nargs='+', help='Run selected lifecycle cases')
args = parser.parse_args()
W = Path(__file__).resolve().parents[3]
T = W / 'ports/dome-fltk/tests'
sys.path.insert(0, str(T))
from Xlib import display
from private_wayland import PrivateWayland
out = args.output.resolve()
out.mkdir(mode=0o700, parents=True, exist_ok=False)
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
sources = {str(p.relative_to(W)): sha(p) for p in [T/'reporting-observer.html', T/'reporting-observer-worker.js']}
results, deliveries, request_errors = {}, {}, []
def stop(proc):
    if proc.poll() is None:
        proc.terminate()
        try: proc.wait(timeout=8)
        except subprocess.TimeoutExpired: proc.kill(); proc.wait()
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def reply(self, data, content_type='application/json', policy=None, case=None):
        self.send_response(200)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(data)))
        if policy:
            self.send_header('Content-Security-Policy', policy)
            endpoint = f'collector="/__endpoint?case={case}"' if args.plain_endpoint else f'collector="/__endpoint?case={case}&batch=a,b";ignored=?1'
            self.send_header('Reporting-Endpoints', endpoint)
        self.end_headers(); self.wfile.write(data)
    def do_GET(self):
        url = urlsplit(self.path)
        case = parse_qs(url.query).get('case', [''])[0]
        if url.path == '/__counts':
            self.reply(json.dumps({'reports':len(deliveries.get(case, []))}).encode()); return
        if url.path == '/fixture':
            self.reply((T/'reporting-observer.html').read_bytes(), 'text/html', "img-src 'none'; report-to collector", case); return
        if url.path == '/worker.js':
            self.reply((T/'reporting-observer-worker.js').read_bytes(), 'text/javascript', "script-src 'self'; report-to collector", case); return
        self.send_error(404)
    def do_POST(self):
        url = urlsplit(self.path)
        case = parse_qs(url.query).get('case', [''])[0]
        try:
            data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if url.path == '/__result': results[case] = data
            elif url.path == '/__endpoint':
                assert self.headers.get_content_type() == 'application/reports+json'
                assert isinstance(data, list)
                deliveries.setdefault(case, []).extend(data)
            else: raise ValueError('Unexpected local POST path')
            self.send_response(204); self.end_headers()
        except Exception as error:
            request_errors.append(str(error)); self.send_error(400)
scenarios = {'buffer':3, 'bound':110, 'disconnect':2, 'records':1, 'repeat':3, 'filter':1, 'buffer-once':2, 'notify-snapshot':1, 'worker':1}
if args.cases:
    assert all(name in scenarios for name in args.cases), 'Unknown reporting scenario'
    scenarios = {name: scenarios[name] for name in args.cases}
runs = []
with contextlib.ExitStack() as stack:
    root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-reporting-regression-')))
    root.chmod(0o700)
    rd, wr = os.pipe()
    xvfb = subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1200x900x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    stack.callback(stop, xvfb); os.close(wr)
    screen = ':'+os.read(rd,64).decode().strip(); os.close(rd)
    connection = display.Display(screen); stack.callback(connection.close)
    wayland = PrivateWayland(connection, screen, root, out); stack.callback(wayland.close)
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    stack.callback(server.server_close); stack.callback(server.shutdown)
    env = {k:v for k,v in os.environ.items() if not k.startswith(('DOME_', 'SERVO_')) and k not in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']}
    env.update(HOME=str(root),DISPLAY=screen,WAYLAND_DISPLAY=wayland.socket,XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_CACHE_HOME=str(root/'cache'),XDG_DATA_HOME=str(root/'data'),LIBGL_ALWAYS_SOFTWARE='1',FONTCONFIG_FILE='/etc/fonts/fonts.conf',RUST_LOG='warn')
    for index, (name, expected) in enumerate(scenarios.items()):
        env['XR_INPUT_PROFILE'] = str(root/f'profile-{index}')
        with (out/(name+'.log')).open('w') as log:
            proc = subprocess.Popen([str(args.binary),f'http://127.0.0.1:{server.server_port}/fixture?case={name}'],stdin=subprocess.PIPE,stdout=log,stderr=log,text=True,env=env)
            stack.callback(stop,proc)
            deadline = time.monotonic()+25
            while name not in results and proc.poll() is None and time.monotonic()<deadline: time.sleep(.025)
            result = results.get(name, {'checks':[], 'error':'No fixture result within 25 seconds'})
            time.sleep(.15)
            live = proc.poll() is None
            if live:
                proc.stdin.write('{"op":"quit","token":1}\n'); proc.stdin.flush()
            try: proc.wait(timeout=10)
            except subprocess.TimeoutExpired: stop(proc)
        reports = deliveries.get(name, [])
        schema = all(report.get('type')=='csp-violation' and report.get('body',{}).get('disposition')=='enforce' and 'documentURL' in report.get('body',{}) for report in reports)
        checks = result['checks'] + [
            {'name':'Endpoint receives each report exactly once','passed':len(reports)==expected},
            {'name':'Endpoint preserves CSP body schema','passed':bool(reports) and schema},
            {'name':'Browser remains live until requested exit','passed':live},
            {'name':'Browser closes cleanly','passed':proc.returncode==0}]
        row = dict(name=name, checks=checks, expected_reports=expected, received_reports=len(reports), exit_code=proc.returncode, error=result.get('error'))
        row['passed'] = not row['error'] and all(check['passed'] for check in checks)
        runs.append(row)
        print(name, 'PASS' if row['passed'] else 'FAIL', len(checks), row['error'], 'reports', len(reports), flush=True)
record = dict(binary=str(args.binary), binary_sha256=sha(args.binary), source_sha256=sources, runs=runs, request_errors=request_errors, plain_endpoint=args.plain_endpoint, runtime_checks=sum(len(row['checks']) for row in runs), delivered_reports=sum(len(reports) for reports in deliveries.values()), passed=not request_errors and all(row['passed'] for row in runs), finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(), scope='Private local CSP reporting lifecycle fixture; software renderer; separate temporary profile, session bus and compositor. No public site/profile access and no complete Reporting API conformance claim.')
(out/'report.json').write_text(json.dumps(record,indent=2)+'\n')
assert record['passed'], 'Reporting lifecycle regression failed'
