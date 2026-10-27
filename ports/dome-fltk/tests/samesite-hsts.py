#!/usr/bin/env python3
"""Verify HSTS-upgraded navigation preserves SameSite status across native reload."""
import argparse
import hashlib
import ssl
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
if os.environ.get('DOME_HSTS_COOKIE_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_HSTS_COOKIE_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest

repo = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=repo / 'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument("--baseline",action="store_true",help="Characterize the earlier reload classification bug")
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
 with tempfile.TemporaryDirectory(prefix='dome-cookie-policy-') as directory:
    runtime=Path(directory);runtime.chmod(0o700)
    def openssl(*args):
        subprocess.run(['openssl',*args],cwd=runtime,check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    openssl('req','-x509','-newkey','rsa:2048','-nodes','-days','2','-subj','/CN=Dome cookie fixture CA',
            '-addext','basicConstraints=critical,CA:TRUE','-keyout','ca.key','-out','ca.pem')
    openssl('req','-newkey','rsa:2048','-nodes','-subj','/CN=localhost','-keyout','server.key','-out','server.csr')
    (runtime/'server.ext').write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1,IP:127.0.0.2,DNS:localhost\n')
    openssl('x509','-req','-in','server.csr','-CA','ca.pem','-CAkey','ca.key','-set_serial','2','-days','2',
            '-sha256','-extfile','server.ext','-out','server.pem')
    hits=[]
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*_):pass
        def do_POST(self):
            reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204);self.send_header('Access-Control-Allow-Origin',plain_origin);self.end_headers()
        def do_GET(self):
            path=urlsplit(self.path).path
            secure=isinstance(self.connection,ssl.SSLSocket)
            names=sorted(part.strip().split('=',1)[0]for part in self.headers.get('Cookie','').split(';')if part.strip())
            hits.append({'path':path,'secure':secure,'cookies':names})
            if path=='/enable':
                self.send_response(200);self.send_header('Access-Control-Allow-Origin',plain_origin)
                self.send_header('Access-Control-Allow-Credentials','true')
                self.send_header('Strict-Transport-Security','max-age=300')
                data=b'ok'
            else:
                data='<title>HSTS cookie fixture</title><h1>HSTS cookie fixture</h1>'
                if path=='/source':
                    data+=f'<a href="http://localhost:{tls_server.server_port}/target" style="position:fixed;left:20px;top:80px">Navigate with HSTS</a>'
                    script="fetch("+json.dumps(tls_origin+'/enable')+",{credentials:'include'}).then(r=>{if(!r.ok)throw Error('HSTS failed');return r.text()}).then(()=>fetch("+json.dumps(tls_origin+'/report')+",{method:'POST',body:JSON.stringify({phase:'source-ready',scheme:location.protocol})}));"
                else:
                    phase='seed'if path=='/seed'else'target'
                    data+='<button style="position:fixed;left:20px;top:80px" onclick="location.reload()">Script reload</button>'
                    data+=f'<a href="http://localhost:{tls_server.server_port}/target" style="position:fixed;left:20px;top:150px">Same-site upgrade</a>'
                    script="fetch('/report',{method:'POST',body:JSON.stringify("+json.dumps({'phase':phase,'secure':secure,'cookies':names})+")});"
                data=('<!doctype html>'+data+'<script>'+script+'</script>').encode()
                self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8')
                if path=='/seed':
                    for name,kind in [('strict','Strict'),('lax','Lax'),('none','None')]:
                        self.send_header('Set-Cookie',f'{name}=fixture; Path=/; Secure; HttpOnly; SameSite={kind}; Max-Age=3600')
            self.send_header('Content-Length',str(len(data)));self.send_header('Cache-Control','no-store');self.end_headers();self.wfile.write(data)
    tls_server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);tls_server.daemon_threads=True
    plain_server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);plain_server.daemon_threads=True
    context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(runtime/'server.pem',runtime/'server.key')
    tls_server.socket=context.wrap_socket(tls_server.socket,server_side=True)
    tls_origin='https://localhost:'+str(tls_server.server_port);plain_origin='http://localhost:'+str(plain_server.server_port)
    threading.Thread(target=tls_server.serve_forever,daemon=True).start();threading.Thread(target=plain_server.serve_forever,daemon=True).start()
    env = {**os.environ, 'FLTK_BACKEND':'x11', 'XDG_RUNTIME_DIR':directory,
           'XDG_CONFIG_HOME':str(runtime/'config'), 'XDG_DATA_HOME':str(runtime/'data'),
           'XDG_CACHE_HOME':str(runtime/'cache'), 'XDG_CURRENT_DESKTOP':'Dome',
           'LIBGL_ALWAYS_SOFTWARE':'1', 'XMODIFIERS':'@im=none', 'RUST_LOG':'warn', 'SSL_CERT_FILE':str(runtime/'ca.pem'), 'SSL_CERT_DIR':str(runtime/'empty-certs'), 'DOME_BROWSER_TRACE':'1'}
    for key in ['DISPLAY','WAYLAND_DISPLAY','WAYLAND_SOCKET','DOME_BROWSER_EXTERNAL_PLAYER']:
        env.pop(key,None)
    rd, wr = os.pipe()
    start(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'], 'xvfb', env, pass_fds=(wr,))
    os.close(wr)
    with os.fdopen(rd) as stream: env['DISPLAY'] = ':' + stream.readline().strip()
    xd = display.Display(env['DISPLAY'])
    (runtime/'empty-certs').mkdir()
    profile=runtime/'profile';profile.mkdir(mode=0o700)
    (profile/'cookie-policy.json').write_text('{"block_third_party":false}');(profile/'cookie-policy.json').chmod(0o600)
    app=start([str(a.binary.resolve()),'--no-restore','--profile',str(profile),tls_origin+'/seed'],'browser',env)
    def window(title):return wait(lambda:next((w for w in xd.screen().root.query_tree().children if title in str(w.get_wm_name()or'')),None))
    main=window('HSTS cookie fixture');wait(lambda:any(r.get('phase')=='seed'for r in reports))
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
    def navigate(url,phase):
        key('l',True);type_text(url);key('Return')
        r=wait(lambda:next((r for r in reports if r.get('phase')==phase),None),seconds=60)
        assert 'error'not in r,r
        return r
    r=navigate(plain_origin+'/source','source-ready')
    check(r['scheme']=='http:','source document exists over HTTP before HSTS is installed')
    time.sleep(.4);g=main.get_geometry()
    xtest.fake_input(xd,X.MotionNotify,x=g.x+40,y=g.y+120+88)
    xtest.fake_input(xd,X.ButtonPress,1);xtest.fake_input(xd,X.ButtonRelease,1);xd.sync()
    def target(count):return wait(lambda:[r for r in reports if r.get('phase')=='target'][count-1]if len([r for r in reports if r.get('phase')=='target'])>=count else None)
    initial=target(1)
    check(initial['secure']and set(initial['cookies'])=={'lax','none'},'HSTS upgrade reaches HTTPS and withholds Strict on the initial cross-scheme navigation')
    key('r',True);toolbar=target(2)
    (out/'observations.json').write_text(json.dumps({'initial':initial,'toolbar':toolbar,'hits':hits},indent=2)+'\n')
    check(set(toolbar['cookies'])==({'strict','lax','none'}if a.baseline else{'lax','none'}),'toolbar reload preserves the original cross-scheme cookie classification')
    time.sleep(.4);g=main.get_geometry();xtest.fake_input(xd,X.MotionNotify,x=g.x+40,y=g.y+120+88)
    xtest.fake_input(xd,X.ButtonPress,1);xtest.fake_input(xd,X.ButtonRelease,1);xd.sync();script=target(3)
    check(set(script['cookies'])=={'strict','lax','none'},'script reload uses the current HTTPS document context')
    key('l',True);type_text('http://localhost:'+str(tls_server.server_port)+'/target');key('Return');typed=target(4)
    check(set(typed['cookies'])=={'strict','lax','none'},'address-bar HSTS navigation remains a null-client first-party navigation')
    time.sleep(.4);g=main.get_geometry();xtest.fake_input(xd,X.MotionNotify,x=g.x+40,y=g.y+120+158)
    xtest.fake_input(xd,X.ButtonPress,1);xtest.fake_input(xd,X.ButtonRelease,1);xd.sync();same_site=target(5)
    check(set(same_site['cookies'])=={'strict','lax','none'},'HTTPS document link upgraded by HSTS sends same-site cookies')
    key('r',True);same_reload=target(6)
    check(set(same_reload['cookies'])==({'lax','none'}if a.baseline else{'strict','lax','none'}),'toolbar reload retains same-site classification after a same-site HSTS upgrade')
    key('q',True);app.wait(timeout=15);check(app.returncode==0,'browser closes cleanly after HSTS reloads')
    (out/'report.json').write_text(json.dumps({'passed':True,'baseline':a.baseline,'binary_sha256':hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest(),'checks':checks},indent=2)+'\n')
finally:
 (out/'fixture-reports.json').write_text(json.dumps({'reports':reports,'hits':locals().get('hits',[])},indent=2)+'\n')
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
