#!/usr/bin/env python3
"""Run local-resource WPT window/dedicated-worker tests in the native embedding.

This is a focused testharness adapter, not the full WPT runner. It uses loopback
HTTP (a secure context), the checked-in WPT testharness, and a private X server.
Remote/TLS and service-worker variants need the full WPT infrastructure.
"""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit
from Xlib import X, display, protocol

signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_CACHE_WPT_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_CACHE_WPT_BUS': '1'})
source = Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=source/'target/release/dome-browser')
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--reference-browser', type=Path)
parser.add_argument('--global', dest='global_', choices=['window','worker'], default='window')
parser.add_argument('--directory', default='/service-workers/cache-storage/')
parser.add_argument('--suffix', default='.https.any.js')
parser.add_argument('--suites', nargs='+', default=['cache-storage', 'cache-storage-keys',
    'cache-storage-match', 'cache-keys', 'cache-delete', 'cache-matchAll'])
args = parser.parse_args()
output = args.output.resolve(); output.mkdir(parents=True, exist_ok=True)
wpt = source/'tests/wpt/tests'
base = '/'+args.directory.strip('/')+'/'
suffix = args.suffix
html_input = suffix.endswith('.html')
assert html_input or suffix.endswith('.js')
assert not html_input or args.global_ == 'window', 'HTML suites require the window global'
html_suffix = suffix if html_input else suffix[:-3]+'.html'
worker_suffix = suffix[:-3]+'.worker.js'
reports = {}; hashes = {}; suites = {}; workers = {}
for suite in args.suites:
    path = wpt/(base.lstrip('/')+suite+suffix)
    script = path.read_text()
    dependencies = re.findall(r'^// META: script=(.+)$', script, re.M)
    if any('.sub.' in name for name in dependencies):
        raise ValueError('Suite needs WPT server substitutions: '+suite)
    suites[base+suite+html_suffix] = (suite, dependencies)
    if not html_input:
        # Match tools/serve/serve.py's ClassicWorkerHandler: .any.js wrappers
        # call global done() after importScripts, including suites whose async
        # tests call t.done(). The latter does not complete worker setup.
        needs_done = '.any.' in suffix or not re.search(r'\bdone\s*\(', script)
        workers[base+suite+worker_suffix] = (suite, dependencies, needs_done)

def completion_reporter(suite):
    return ('<script>add_completion_callback((tests,status)=>fetch("/__report/'+suite+'",{method:"POST",'
        'body:JSON.stringify({harness:{status:status.status,message:status.message},'
        'tests:tests.map(t=>({name:t.name,status:t.status,message:t.message,stack:t.stack}))})}));</script>')

html_documents = {}
if html_input:
    # Register before the test scripts run, including tests that finish synchronously.
    # Preserve the original test logic and hash both the source and served document.
    harness_tag = re.compile(r'<script\b[^>]*\bsrc\s*=\s*[\"\']?/resources/testharness\.js[\"\']?\s*[^>]*>\s*</script\s*>', re.I)
    for path, (suite, _) in suites.items():
        original = (wpt/path.lstrip('/')).read_bytes()
        document, count = harness_tag.subn(lambda match: match[0]+completion_reporter(suite), original.decode('utf-8'))
        assert count == 1, 'Expected exactly one testharness.js script in '+path
        hashes['original:'+path] = hashlib.sha256(original).hexdigest()
        html_documents[path] = document.encode('utf-8')

class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw): super().__init__(*a, directory=str(wpt), **kw)
    def log_message(self, *_): pass
    def do_GET(self):
        path = urlsplit(self.path).path
        # Match the canonical WPT server rewrite (tools/serve/serve.py).
        if path == '/resources/WebIDLParser.js':
            data = (wpt/'resources/webidl2/lib/webidl2.js').read_bytes()
            hashes[path] = hashlib.sha256(data).hexdigest()
            self.send_response(200); self.send_header('Content-Type','text/javascript')
            self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
            return
        if path in workers:
            suite, dependencies, needs_done = workers[path]
            data = ('importScripts("/resources/testharness.js");'
                'add_completion_callback((tests,status)=>postMessage({type:"native-wpt-completion",'
                'report:{harness:{status:status.status,message:status.message},'
                'tests:tests.map(t=>({name:t.name,status:t.status,message:t.message,stack:t.stack}))}}));'
                'importScripts('+','.join(json.dumps(name) for name in [*dependencies,suite+suffix])+');'+
                ('done();' if needs_done else '')).encode()
            hashes[path] = hashlib.sha256(data).hexdigest()
            self.send_response(200); self.send_header('Content-Type','text/javascript')
            self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
            return
        if path in suites:
            suite, dependencies = suites[path]
            if html_input:
                data = html_documents[path]
                hashes[path] = hashlib.sha256(data).hexdigest()
                self.send_response(200); self.send_header('Content-Type','text/html; charset=utf-8')
                self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
                return
            if args.global_ == 'worker':
                data = ('<!doctype html><meta charset="utf-8"><title>Native worker WPT</title><script>'
                    'const worker=new Worker("'+suite+worker_suffix+'");'
                    'worker.onmessage=event=>{if(event.data.type==="native-wpt-completion")'
                    'fetch("/__report/'+suite+'",{method:"POST",body:JSON.stringify(event.data.report)}).then(()=>worker.terminate())};'
                    'worker.onerror=event=>fetch("/__report/'+suite+'",{method:"POST",body:JSON.stringify({harness:{status:1,message:event.message},tests:[]})});'
                    '</script>').encode()
                self.send_response(200); self.send_header('Content-Type','text/html; charset=utf-8')
                self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
                return
            data = ('<!doctype html><meta charset="utf-8"><title>Native WPT</title>'
                '<script src="/resources/testharness.js"></script><script>'
                'add_completion_callback((tests,status)=>fetch("/__report/'+suite+'",{method:"POST",'
                'body:JSON.stringify({harness:{status:status.status,message:status.message},'
                'tests:tests.map(t=>({name:t.name,status:t.status,message:t.message,stack:t.stack}))})}));'
                '</script>'+''.join('<script src="'+name+'"></script>' for name in dependencies)+
                '<script src="'+suite+suffix+'"></script>').encode()
            self.send_response(200); self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
            return
        local = (wpt/path.lstrip('/')).resolve()
        if local.is_relative_to(wpt.resolve()) and local.is_file():
            hashes[path] = hashlib.sha256(local.read_bytes()).hexdigest()
        super().do_GET()
    def do_POST(self):
        data = self.rfile.read(int(self.headers['Content-Length']))
        if self.path.startswith('/__report/'):
            reports[self.path.rsplit('/', 1)[1]] = json.loads(data)
        self.send_response(204); self.end_headers()

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
app = xvfb = connection = None; runs = []
try:
    with tempfile.TemporaryDirectory(prefix='dome-cache-wpt-') as directory:
        read, write = os.pipe()
        xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(write), '-screen', '0', '1280x900x24', '-nolisten', 'tcp'], pass_fds=(write,), stderr=subprocess.DEVNULL)
        os.close(write)
        with os.fdopen(read) as stream: screen = ':'+stream.readline().strip()
        connection = display.Display(screen)
        env = {**os.environ, 'DISPLAY':screen, 'FLTK_BACKEND':'x11', 'LIBGL_ALWAYS_SOFTWARE':'1',
            'XDG_RUNTIME_DIR':directory, 'XDG_CONFIG_HOME':directory+'/config',
            'XDG_CACHE_HOME':directory+'/cache', 'XDG_DATA_HOME':directory+'/data'}
        for key in ['WAYLAND_DISPLAY', 'WAYLAND_SOCKET']: env.pop(key, None)
        for suite in args.suites:
            reference = None
            runtime = Path(directory)/suite; runtime.mkdir()
            url = f'http://127.0.0.1:{server.server_port}'+base+suite+html_suffix
            with (output/(suite+'.log')).open('w') as log:
                if args.reference_browser:
                    from reference_browser import ReferenceBrowser
                    reference = ReferenceBrowser(args.reference_browser.resolve(), runtime, url, env, log)
                    app = reference.process
                else:
                    app = subprocess.Popen([str(args.binary.resolve()), '--software', '--profile', str(runtime/'profile'), url], env=env, stdout=log, stderr=log, start_new_session=True)
            deadline = time.monotonic()+100
            while suite not in reports and app.poll() is None and time.monotonic()<deadline: time.sleep(.05)
            record = {'suite':suite, **reports.get(suite, {'harness':{'status':2, 'message':'No completion report'}, 'tests':[]})}
            if reference: closed = reference.close()
            else:
                for window in connection.screen().root.query_tree().children:
                    try:
                        if window.get_wm_name() and window.get_attributes().map_state == X.IsViewable:
                            window.send_event(protocol.event.ClientMessage(window=window, client_type=connection.intern_atom('WM_PROTOCOLS'), data=(32, [connection.intern_atom('WM_DELETE_WINDOW'), X.CurrentTime, 0, 0, 0])), event_mask=0)
                    except Exception: pass
                connection.sync(); closed = False
                try: app.wait(timeout=10); closed = app.returncode == 0
                except subprocess.TimeoutExpired: pass
            record.update(clean_close=closed, exit_code=app.poll()); runs.append(record)
            if app.poll() is None: os.killpg(app.pid, signal.SIGKILL); app.wait(timeout=5)
            passed = sum(t['status'] == 0 for t in record['tests'])
            print(f'{suite}: {passed}/{len(record["tests"])}; harness={record["harness"]["status"]}; clean_close={closed}', flush=True)
            for test in record['tests']:
                if test['status'] != 0: print('FAIL '+test['name']+': '+str(test['message']), flush=True)
            scope = ('WPT HTML with completion reporter injected; test logic unchanged' if html_input else 'unchanged WPT '+args.global_+' tests')+' via loopback embedding adapter'
            (output/'report.json').write_text(json.dumps({'scope':scope, 'runs':runs, 'served_sha256':hashes,
                'binary_sha256':hashlib.sha256((args.reference_browser or args.binary).read_bytes()).hexdigest()}, indent=2)+'\n')
        assert all(run['clean_close'] and run['harness']['status'] == 0 and run['tests'] and all(test['status'] == 0 for test in run['tests']) for run in runs)
finally:
    if app is not None and app.poll() is None: os.killpg(app.pid, signal.SIGKILL); app.wait(timeout=5)
    if connection is not None: connection.close()
    if xvfb is not None: xvfb.terminate(); xvfb.wait(timeout=5)
    server.shutdown(); server.server_close()
