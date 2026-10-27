#!/usr/bin/env python3
"""Verify adblock-rust query removal before transport, preserving request/security semantics."""
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
if os.environ.get('DOME_QUERY_REMOVAL_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_QUERY_REMOVAL_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest
from PIL import Image
from dbus_next.aio import MessageBus
from dbus_next import Variant

repo = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=repo / 'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument("--baseline",action="store_true",help="Record current query-modifier coverage")
p.add_argument("--disabled",action="store_true",help="Explicit user-disabled filtering control")
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
 with tempfile.TemporaryDirectory(prefix='dome-query-removal-') as directory:
    runtime=Path(directory);runtime.chmod(0o700)
    fixture=Path(__file__).with_suffix('.html').read_bytes()
    hits=[]
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*_):pass
        def record(self,body=''):
            value={'path':self.path,'method':self.command,'body':body,'cookie':self.headers.get('Cookie',''),'origin':self.headers.get('Origin',''),'probe':self.headers.get('X-Probe','')};hits.append(value);return value
        def send(self,data,kind='application/json',cors=True,status=200):
            self.send_response(status);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(data)));self.send_header('Cache-Control','no-store')
            if cors:self.send_header('Access-Control-Allow-Origin',self.headers.get('Origin','*'))
            self.end_headers();self.wfile.write(data)
        def do_OPTIONS(self):
            self.record();self.send_response(204);self.send_header('Content-Length','0');self.send_header('Access-Control-Allow-Origin',self.headers.get('Origin','*'));self.send_header('Access-Control-Allow-Methods','POST');self.send_header('Access-Control-Allow-Headers','content-type,x-probe');self.end_headers()
        def do_POST(self):
            body=self.rfile.read(int(self.headers.get('Content-Length','0'))).decode()
            if self.path=='/report':reports.append(json.loads(body));self.send(b'');return
            self.send(json.dumps(self.record(body)).encode())
        def do_GET(self):
            path=urlsplit(self.path).path
            if path in ['/scope-default-script','/scope-default-fetch']:
                value=self.record()
                if path=='/scope-default-script':self.send(('window.defaultScopeScript='+json.dumps(self.path)+';').encode(),kind='application/javascript')
                else:self.send(json.dumps(value).encode())
                return
            if path=='/redirect':
                self.record();self.send_response(302);self.send_header('Location','/privacy-redirect?keep=a%2Bb&utm_source=redirect&keep=two');self.send_header('Content-Length','0');self.end_headers();return
            if path.startswith('/privacy'):
                value=self.record()
                if self.headers.get('Upgrade','').lower()=='websocket':
                    import base64
                    accept=base64.b64encode(hashlib.sha1((self.headers['Sec-WebSocket-Key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
                    self.send_response(101);self.send_header('Upgrade','websocket');self.send_header('Connection','Upgrade');self.send_header('Sec-WebSocket-Accept',accept);self.end_headers();self.wfile.write(bytes([0x88,0x02,0x03,0xe8]));self.wfile.flush();self.close_connection=True;return
                if path in ['/privacy-nav','/privacy-hash']:
                    self.send(b"<!doctype html><title>Privacy query fixture</title><script>fetch('/report',{method:'POST',body:JSON.stringify({phase:'nav',url:location.href})});</script>",kind='text/html');return
                if path in ['/privacy-script','/privacy-preload','/privacy-preload-cross','/privacy-block']:
                    key={'/privacy-script':'scriptPath','/privacy-preload':'preloadPath','/privacy-preload-cross':'preloadCrossPath','/privacy-block':'blockedPath'}[path];self.send(('window.'+key+'='+json.dumps(self.path)+';').encode(),kind='application/javascript');return
                if path=='/privacy-image':
                    import base64
                    self.send(base64.b64decode('R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw=='),kind='image/gif');return
                self.send(json.dumps(value).encode(),cors=path!='/privacy-deny');return
            data=fixture.replace(b'__CROSS_ORIGIN__',cross_origin.encode())
            self.send(data,kind='text/html')
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True
    origin='http://127.0.0.1:'+str(server.server_port);cross_origin='http://localhost:'+str(server.server_port)
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
    (profile/'adblock.txt').write_text('/privacy$all,removeparam=utm_source\n/privacy-block$script\n/scope-default-$removeparam=utm_source\n')
    (profile/'adblock-allowlist.txt').write_text(cross_origin+'\n')
    if a.disabled:(profile/'adblock-disabled').write_text('Explicit test control\n')
    app=start([str(a.binary.resolve()),'--no-restore','--profile',str(profile),origin+'/progress'],'browser',env)
    main=wait(lambda:next((w for w in xd.screen().root.query_tree().children if 'Privacy query fixture'in str(w.get_wm_name()or'')),None))
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
    result=wait(lambda:next((r for r in reports if r.get('phase')=='main'),None),seconds=60)
    check('error'not in result,'all request transports finish without fixture errors')
    wait(lambda:any(urlsplit(h['path']).path=='/privacy-beacon'for h in hits))
    check(result['cookie'],'first-party cookie is installed')
    check(result['ws'],'legitimate WebSocket opens and closes')
    check(result['image']==1,'rewritten image request decodes')
    check(isinstance(result.get('script'),str)and isinstance(result.get('preload'),str)and isinstance(result.get('preloadCross'),str),'ordinary and preloaded scripts execute')
    check(result['beacon'],'beacon is queued successfully')
    check('utm_source=default-script'in result['defaultScopeScript'],'default removeparam scope leaves script requests unchanged')
    default_scope_ok=('utm_source='in result['defaultScopeFetch']['path'])==a.disabled
    if a.baseline:checks.append({'check':'default removeparam scope cleans fetch unless filtering is disabled','passed':default_scope_ok})
    else:check(default_scope_ok,'default removeparam scope cleans fetch unless filtering is disabled')
    check(result['deny'],'CORS still denies a response without permission')
    check(result['blocked']==a.disabled,'blocking retains priority over query cleanup')
    for name,method,body in [('get','GET',''),('post','POST','post body'),('cross','POST','cross body'),('worker','GET','')]:
        r=result[name];check(r['method']==method and r['body']==body,name+' preserves method and body')
    check('queryProbe=yes'in result['get']['cookie']and'queryProbe=yes'in result['post']['cookie'],'first-party credentials survive query cleanup')
    check(result['cross']['origin']==origin and result['cross']['probe']=='yes','CORS request origin and custom header remain intact')
    check(result['redirected'],'server redirect remains observable')
    for base,tag in [(origin,'nav'),(origin,'hash'),(cross_origin,'allowed')]:
        target=base+('/privacy-hash#route?utm_source=fragment-only'if tag=='hash'else'/privacy-nav?keep=a%2Bb&utm_source='+tag+'&keep=two#retained')
        before=len(reports);key('l',True);type_text(target);key('Return')
        r=wait(lambda:next((r for r in reports[before:]if r.get('phase')=='nav'),None),seconds=30)
        if tag=='hash':check(r['url']==target,'fragment-only query lookalikes retain their document URL')
        elif tag=='allowed':check(r['url']==target,'trusted top-level site exception retains query parameters')
        else:
            expected=target if a.disabled else origin+'/privacy-nav?keep=a%2Bb&keep=two#retained'
            if a.baseline:checks.append({'check':'navigation URL removes tracking and preserves fragment','passed':r['url']==expected})
            else:check(r['url']==expected,'navigation URL removes tracking and preserves fragment')
    check(any(h['path']=='/privacy-fragment'for h in hits),'fragment-only query lookalikes never enter the transport query')
    beacon=next(h for h in hits if urlsplit(h['path']).path=='/privacy-beacon')
    check(beacon['body']=='beacon body'and'queryProbe=yes'in beacon['cookie'],'beacon body and first-party credentials remain intact')
    failures=[]
    for record in hits:
        path=urlsplit(record['path']).path
        if not path.startswith('/privacy')or path in ['/privacy-block','/privacy-hash','/privacy-fragment']:continue
        # The final allowlisted navigation intentionally retains its tag.
        if 'utm_source=allowed'in record['path']:continue
        query=urlsplit(record['path']).query
        ok=('keep=a%2Bb'in query and query.endswith('keep=two')and(('utm_source='in query)==a.disabled))
        name=record['method']+' '+path+' keeps raw unrelated fields and applies privacy policy'
        if a.baseline:
            checks.append({'check':name,'passed':ok});print(('PASS 'if ok else'FAIL ')+name,flush=True)
        else:check(ok,name)
        if not ok:failures.append(name)
    check(sum(urlsplit(h['path']).path=='/privacy-preload'for h in hits)==1,'preloaded script is consumed once without a duplicate transport request')
    check(sum(urlsplit(h['path']).path=='/privacy-preload-cross'for h in hits)==1,'cross-origin opaque preloaded script is consumed once')
    check(any(h['method']=='OPTIONS'and urlsplit(h['path']).path=='/privacy-cross'for h in hits),'cross-origin POST still performs a preflight')
    key('q',True);app.wait(timeout=15);check(app.returncode==0,'browser closes cleanly after query filtering')
    (out/'observations.json').write_text(json.dumps({'result':result,'reports':reports,'hits':hits},indent=2)+'\n')
    passed=not failures and all(c['passed']for c in checks)
    (out/'report.json').write_text(json.dumps({'passed':passed,'baseline':a.baseline,'disabled':a.disabled,'binary_sha256':hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest(),'checks':checks},indent=2)+'\n')

finally:
 (out/'hits.json').write_text(json.dumps(hits,indent=2)+'\n')
 (out/'fixture-reports.json').write_text(json.dumps(reports,indent=2)+'\n')
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
