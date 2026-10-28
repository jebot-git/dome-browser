#!/usr/bin/env python3
"""Verify clientless reporting preserves native cosmetic/privacy filtering and site exceptions."""
import argparse
import hashlib
import asyncio
from urllib.parse import urlsplit
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

signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_REPORT_FILTERING_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_REPORT_FILTERING_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest
from PIL import Image
from dbus_next.aio import MessageBus
from dbus_next import Variant

repo = next((p for p in Path(__file__).resolve().parents if (p/'components/script').is_dir()), Path.cwd())
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=repo / 'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument('--allowlisted', action='store_true')
a = p.parse_args()
out = a.output.resolve(); out.mkdir(parents=True, exist_ok=False)
checks = []; children = []; logs = []; reports = []
def check(ok, name):
    checks.append({'check':name, 'passed':bool(ok)})
    print(('PASS ' if ok else 'FAIL ') + name, flush=True)
    assert ok, name
def wait(callback, seconds=30):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        try:
            value = callback()
            if value: return value
        except (OSError, ValueError): pass
        time.sleep(.05)
    raise AssertionError('Timed out; inspect ' + str(out))
def start(command, name, env, **kwargs):
    log = (out / (name + '.log')).open('w'); logs.append(log)
    proc = subprocess.Popen(command, env=env, stdout=log, stderr=log, **kwargs)
    children.append(proc); return proc

try:
 with tempfile.TemporaryDirectory(prefix='dome-adblock-transports-') as directory:
    runtime=Path(directory);runtime.chmod(0o700)
    fixture=Path(__file__).with_suffix('.html').read_text().replace('ALLOWLISTED', 'true' if a.allowlisted else 'false').encode()
    hits=[]
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*_):pass
        def do_POST(self):
            body=self.rfile.read(int(self.headers.get('Content-Length','0')))
            if self.path=='/report': reports.append(json.loads(body))
            else: hits.append({'path':self.path,'payload':json.loads(body),'referrer':self.headers.get('Referer')})
            self.send_response(204);self.send_header('Content-Length','0');self.end_headers()
        def do_GET(self):
            path=urlsplit(self.path).path
            if path=='/__counts':
                data=json.dumps({'allowed':sum(len(hit['payload']) for hit in hits if hit['path']=='/allowed-endpoint'), 'blocked':sum(len(hit['payload']) for hit in hits if hit['path']=='/blocked-endpoint')}).encode()
                self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data);return
            self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8')
            self.send_header('Content-Security-Policy', "img-src 'none';report-to blocked")
            self.send_header('Content-Security-Policy-Report-Only', "img-src 'none';report-to allowed")
            self.send_header('Reporting-Endpoints', 'blocked="/blocked-endpoint", allowed="/allowed-endpoint"')
            self.send_header('Content-Length',str(len(fixture)));self.end_headers();self.wfile.write(fixture)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True
    origin='http://127.0.0.1:'+str(server.server_port)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    env = {**os.environ, 'FLTK_BACKEND':'x11', 'XDG_RUNTIME_DIR':directory,
           'XDG_CONFIG_HOME':str(runtime/'config'), 'XDG_DATA_HOME':str(runtime/'data'),
           'XDG_CACHE_HOME':str(runtime/'cache'), 'XDG_CURRENT_DESKTOP':'Dome',
           'LIBGL_ALWAYS_SOFTWARE':'1', 'XMODIFIERS':'@im=none', 'RUST_LOG':'warn', 'DOME_BROWSER_TRACE':'1'}
    for key in ['DISPLAY','WAYLAND_DISPLAY','WAYLAND_SOCKET','DOME_BROWSER_EXTERNAL_PLAYER']:
        env.pop(key,None)
    rd, wr = os.pipe()
    start(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'], 'xvfb', env, pass_fds=(wr,))
    os.close(wr)
    with os.fdopen(rd) as stream: env['DISPLAY'] = ':' + stream.readline().strip()
    xd = display.Display(env['DISPLAY'])
    profile=runtime/'profile';profile.mkdir(mode=0o700)
    (profile/'filterlists.json').write_text('[]')
    (profile/'adblock.txt').write_text('##.ad\n/blocked-endpoint$other\n')
    if a.allowlisted: (profile/'adblock-allowlist.txt').write_text(origin+'\n')
    app=start([str(a.binary.resolve()),'--no-restore','--profile',str(profile),origin+'/progress'],'browser',env)
    main=wait(lambda:next((w for w in xd.screen().root.query_tree().children if 'Reporting filtering fixture'in str(w.get_wm_name()or'')),None))
    def key(name, control=False, target=None):
        (target or main).set_input_focus(X.RevertToParent,X.CurrentTime)
        if control: xtest.fake_input(xd,X.KeyPress,xd.keysym_to_keycode(XK.XK_Control_L))
        code=xd.keysym_to_keycode(XK.string_to_keysym(name))
        xtest.fake_input(xd,X.KeyPress,code);xtest.fake_input(xd,X.KeyRelease,code)
        if control: xtest.fake_input(xd,X.KeyRelease,xd.keysym_to_keycode(XK.XK_Control_L))
        xd.sync()
    def type_text(text):
        for char in text:
            if char == "\n":
                code = xd.keysym_to_keycode(XK.XK_Return)
                xtest.fake_input(xd,X.KeyPress,code);xtest.fake_input(xd,X.KeyRelease,code)
                continue
            sym = XK.string_to_keysym(char) or ord(char)
            code = xd.keysym_to_keycode(sym)
            shifted = xd.keycode_to_keysym(code,1) == sym
            if shifted: xtest.fake_input(xd,X.KeyPress,xd.keysym_to_keycode(XK.XK_Shift_L))
            xtest.fake_input(xd,X.KeyPress,code);xtest.fake_input(xd,X.KeyRelease,code)
            if shifted: xtest.fake_input(xd,X.KeyRelease,xd.keysym_to_keycode(XK.XK_Shift_L))
        xd.sync()
    result=wait(lambda:reports,seconds=30)[-1]
    time.sleep(.2)
    (out/'observations.json').write_text(json.dumps({'fixture':result,'hits':hits},indent=2)+'\n')
    check('error' not in result, 'reporting fixture completes without script errors')
    for row in result['checks']: check(row['passed'],row['name'])
    expected={'/blocked-endpoint':1 if a.allowlisted else 0, '/allowed-endpoint':1}
    for path,count in expected.items():
        values=[hit for hit in hits if hit['path']==path]
        check(sum(len(hit['payload']) for hit in values)==count, 'Exact delivery count for '+path)
        check(all(not hit['referrer'] for hit in values), 'No report referrer for '+path)
    key('q',True);app.wait(timeout=15);check(app.returncode==0,'browser closes cleanly after report filtering')
    (out/'report.json').write_text(json.dumps({'passed':True,'allowlisted':a.allowlisted,'binary_sha256':hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest(),'source_sha256':{p.name:hashlib.sha256(p.read_bytes()).hexdigest()for p in [Path(__file__),Path(__file__).with_suffix('.html')]},'checks':checks},indent=2)+'\n')

finally:
 (out/'fixture-reports.json').write_text(json.dumps(reports,indent=2)+'\n')
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
