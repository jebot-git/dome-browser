#!/usr/bin/env python3
"""Exercise local WebXR policy report snapshots, delivery and document lifetime.

Use the xr-native-input embedder, python-xlib and a private DomeD compositor.
All page state, profiles, buses, displays and reporting endpoints are isolated.
"""
from pathlib import Path
import argparse, contextlib, datetime, hashlib, http.server, json, os, resource, signal, subprocess, sys, tempfile, threading, time
from urllib.parse import urlsplit, parse_qs
signal.pthread_sigmask(signal.SIG_SETMASK, [])
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
if os.environ.get('DOME_POLICY_REPORTING_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv], {**os.environ, 'DOME_POLICY_REPORTING_TEST_BUS':'1'})
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--cases', nargs='+')
a = p.parse_args()
HERE = Path(__file__).resolve().parent
W = next((path for path in HERE.parents if (path/'components/script').is_dir()), Path.cwd())
assert (W/'ports/dome-fltk/tests/private_wayland.py').is_file(), 'Run from the Servo checkout root'
T = W/'ports/dome-fltk/tests'
sys.path.insert(0, str(T))
from Xlib import display
from private_wayland import PrivateWayland
out = a.output.resolve(); out.mkdir(mode=0o700, parents=True, exist_ok=False)
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
fixture = (HERE/'permissions-policy-reporting.html').read_text()
caller = (HERE/'caller.js').read_bytes()
assert caller.count(b'\n') == 1
base_deny = 'xr-spatial-tracking=();report-to=enforced'
base_report = 'xr-spatial-tracking=();report-to=reportonly'
allow = 'xr-spatial-tracking=*'
cases = {
    'enforce':(base_deny, '', False, 'enforce', {'enforced':1}),
    'enforce-fragment':(base_deny, '', False, 'enforce', {'enforced':1}),
    'report-only':(allow, base_report, True, 'report', {'reportonly':1}),
    'precedence':(base_deny, base_report, False, 'enforce', {'enforced':1}),
    'observer-only':('xr-spatial-tracking=()', '', False, 'enforce', {}),
    'endpoint-unknown':('xr-spatial-tracking=();report-to=missing', '', False, 'enforce', {}),
    'header-allowed':(allow, allow, True, None, {}),
    'header-repeated':(['geolocation=(), xr-spatial-tracking=*',base_deny], '', False, 'enforce', {'enforced':1}),
    'malformed':('xr-spatial-tracking=(', '', True, None, {}),
    'introspection':(base_deny, base_report, False, 'enforce', {}),
    'inline-required':(base_deny, '', False, 'enforce', {'enforced':1}),
    'inline-optional':(base_deny, '', False, 'enforce', {'enforced':1}),
    'disconnect':(base_deny, '', False, 'enforce', {'enforced':2}),
    'records':(base_deny, '', False, 'enforce', {'enforced':1}),
    'snapshot':(base_deny, '', False, 'enforce', {'enforced':1}),
    'cross-realm':(base_deny, '', False, 'enforce', {'enforced':2}),
    'buffer-types':(base_deny, '', False, 'enforce', {'enforced':110,'csp':110}),
    'frame-report-only':(allow, 'xr-spatial-tracking=();report-to=parent', True, 'report', {'child':1,'parent':1}),
    'frame-enforced':('xr-spatial-tracking=();report-to=parent', allow, False, 'enforce', {'child':1,'parent':1}),
    'frame-cross-origin':(allow, 'xr-spatial-tracking=self;report-to=parent', True, 'report', {'child':1,'parent':1}),
    'navigation':(allow, allow, True, 'report', {'child':1}),
}
if a.cases:
    assert all(name in cases for name in a.cases)
    cases = {name:cases[name] for name in a.cases}
results, deliveries, errors = {}, {}, []
servers = []
class FixtureHTTPServer(http.server.ThreadingHTTPServer):
    request_queue_size = 256
def stop(proc):
    if proc.poll() is None:
        proc.terminate()
        try: proc.wait(timeout=8)
        except subprocess.TimeoutExpired: proc.kill(); proc.wait()
def endpoint_counts(case):
    return {name:len(reports) for name,reports in deliveries.get(case,{}).items()}
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self):
        url = urlsplit(self.path); query = parse_qs(url.query)
        case = query.get('case',[''])[0]; role = query.get('role',['main'])[0]
        headers = []
        if url.path == '/__counts':
            body = json.dumps(endpoint_counts(case)).encode(); content_type = 'application/json'
        elif url.path == '/caller.js':
            body = caller; content_type = 'text/javascript'
        elif url.path == '/fixture' and case in cases:
            enforced, report_only, allowed, disposition, _ = cases[case]
            origin = f'http://127.0.0.1:{self.server.server_port}'
            parent_origin = f'http://127.0.0.1:{servers[0].server_port}'
            child_origin = f'http://127.0.0.1:{servers[1].server_port}' if case == 'frame-cross-origin' else parent_origin
            if role == 'child':
                enforced = allow + (';report-to=child' if case == 'frame-enforced' else '')
                report_only = ('xr-spatial-tracking=()' if case == 'navigation' else allow) + ';report-to=child'
            elif role == 'empty-child':
                enforced = report_only = allow
            cfg = dict(case=case, role=role, allowed=allowed, disposition=disposition,
                parent_origin=parent_origin, child_url=child_origin+f'/fixture?case={case}&role=child',
                empty_url=parent_origin+f'/fixture?case={case}&role=empty-child')
            body = fixture.replace('CONFIGURATION',json.dumps(cfg)).encode(); content_type = 'text/html; charset=utf-8'
            headers += [('Permissions-Policy',value) for value in (enforced if isinstance(enforced,list) else [enforced]) if value]
            if report_only: headers.append(('Permissions-Policy-Report-Only',report_only))
            headers.append(('Content-Security-Policy',"img-src 'none'; report-to csp"))
            headers.append(('Reporting-Endpoints', ', '.join(f'{name}="/__endpoint?case={case}&name={name}&batch=a,b";ignored=?1' for name in ['enforced','reportonly','csp','child','parent','default'])))
        else:
            self.send_error(404); return
        self.send_response(200); self.send_header('Content-Type',content_type); self.send_header('Content-Length',str(len(body)))
        for name,value in headers: self.send_header(name,value)
        self.end_headers(); self.wfile.write(body)
    def do_POST(self):
        url = urlsplit(self.path); query = parse_qs(url.query); case = query.get('case',[''])[0]
        try:
            data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if url.path == '/__result': results[case] = data
            elif url.path == '/__endpoint':
                assert self.headers.get_content_type() == 'application/reports+json'
                assert isinstance(data,list)
                name = query['name'][0]
                deliveries.setdefault(case,{}).setdefault(name,[]).extend(data)
            else: raise ValueError('Unexpected local POST')
            self.send_response(204); self.end_headers()
        except Exception as error:
            errors.append(str(error)); self.send_error(400)
runs = []
with contextlib.ExitStack() as stack:
    root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-policy-reporting-'))); root.chmod(0o700)
    rd,wr = os.pipe()
    xvfb = subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1200x900x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    stack.callback(stop,xvfb); os.close(wr); screen = ':'+os.read(rd,64).decode().strip(); os.close(rd)
    connection = display.Display(screen); stack.callback(connection.close)
    wayland = PrivateWayland(connection,screen,root,out); stack.callback(wayland.close)
    for _ in range(2):
        server = FixtureHTTPServer(('127.0.0.1',0),Handler); servers.append(server)
        threading.Thread(target=server.serve_forever,daemon=True).start(); stack.callback(server.server_close); stack.callback(server.shutdown)
    env = {k:v for k,v in os.environ.items() if not k.startswith(('DOME_','SERVO_','XR_INPUT_')) and k not in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']}
    env.update(HOME=str(root),DISPLAY=screen,WAYLAND_DISPLAY=wayland.socket,XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_CACHE_HOME=str(root/'cache'),XDG_DATA_HOME=str(root/'data'),LIBGL_ALWAYS_SOFTWARE='1',FONTCONFIG_FILE='/etc/fonts/fonts.conf',RUST_LOG='warn')
    for index,(name,config) in enumerate(cases.items()):
        env['XR_INPUT_PROFILE'] = str(root/f'profile-{index}')
        expected = config[4]
        target = f'http://127.0.0.1:{servers[0].server_port}/fixture?case={name}' + ('#display-fragment' if name=='enforce-fragment' else '')
        with (out/(name+'.log')).open('w') as log:
            proc = subprocess.Popen([str(a.binary),target],stdin=subprocess.PIPE,stdout=log,stderr=log,text=True,env=env)
            stack.callback(stop,proc); deadline = time.monotonic()+45
            while name not in results and proc.poll() is None and time.monotonic()<deadline: time.sleep(.025)
            row = results.get(name, {'checks':[],'error':'No result within 45 seconds'})
            time.sleep(.15); live = proc.poll() is None
            if live: proc.stdin.write('{"op":"quit","token":1}\n'); proc.stdin.flush()
            try: proc.wait(timeout=10)
            except subprocess.TimeoutExpired: stop(proc)
        actual = endpoint_counts(name)
        payloads = deliveries.get(name,{})
        schema = all(
            isinstance(report.get('age'),int) and report['age']>=0 and isinstance(report.get('body'),dict) and
            ((report.get('type')=='permissions-policy-violation' and report['body'].get('featureId')=='xr-spatial-tracking' and report['body'].get('disposition') in ['enforce','report'] and 'allowAttribute' not in report['body'] and 'srcAttribute' not in report['body']) or
             (report.get('type')=='potential-permissions-policy-violation' and report['body'].get('featureId')=='xr-spatial-tracking' and report['body'].get('disposition')==config[3] and 'allowAttribute' in report['body'] and 'srcAttribute' in report['body']) or
             (report.get('type')=='csp-violation' and report['body'].get('disposition')=='enforce'))
            for reports in payloads.values() for report in reports)
        row['checks'] += [
            {'name':'Reports reach only the response-selected endpoints exactly once','passed':actual==expected},
            {'name':'Typed endpoint bodies and nonnegative report age are correct','passed':schema},
            {'name':'Browser stays live until requested exit','passed':live},
            {'name':'Browser closes cleanly','passed':proc.returncode==0}]
        diagnostics = (out/(name+'.log')).read_text(errors='replace')
        row.update(name=name,exit_code=proc.returncode,expected_endpoints=expected,actual_endpoints=actual,panic_seen='panicked at' in diagnostics)
        row['passed'] = not row.get('error') and not row['panic_seen'] and all(check['passed'] for check in row['checks'])
        runs.append(row); print(name,'PASS' if row['passed'] else 'FAIL',len(row['checks']),row.get('error'),actual,flush=True)
record = dict(binary=str(a.binary),binary_sha256=sha(a.binary),source_sha256={p.name:sha(p) for p in [Path(__file__),HERE/'permissions-policy-reporting.html',HERE/'caller.js']},runs=runs,request_errors=errors,runtime_checks=sum(len(row['checks'])for row in runs),delivered_reports=sum(len(reports)for groups in deliveries.values()for reports in groups.values()),passed=not errors and all(row['passed']for row in runs),finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),scope='Private local native-engine policy reporting fixture, software renderer, inline-only registry. No immersive host/device acquired; no whole-API or full WPT conformance claim.')
(out/'report.json').write_text(json.dumps(record,indent=2)+'\n');assert record['passed'],'Policy reporting regression failed'
