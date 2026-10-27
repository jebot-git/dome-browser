#!/usr/bin/env python3
"""Exercise third-party cookie controls using trusted local HTTPS sites."""
import argparse
import base64
import hashlib
import ssl
from urllib.parse import urlsplit, parse_qs
import asyncio
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
if os.environ.get('DOME_COOKIE_POLICY_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_COOKIE_POLICY_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest
from PIL import Image
from dbus_next.aio import MessageBus
from dbus_next import Variant

repo = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=repo / 'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument("--baseline",action="store_true",help="Characterize the earlier build without a cookie control")
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
    fixture=Path(__file__).with_suffix('.js').read_text()
    hits=[]
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*_):pass
        def cors(self):
            origin=self.headers.get('Origin')
            if origin in [first,third,'null']:
                self.send_header('Access-Control-Allow-Origin',origin)
                self.send_header('Access-Control-Allow-Credentials','true')
                self.send_header('Vary','Origin')
        def do_OPTIONS(self):
            self.send_response(204);self.cors();self.send_header('Access-Control-Allow-Headers','Authorization')
            self.send_header('Access-Control-Allow-Methods','GET, POST, OPTIONS');self.end_headers()
        def do_POST(self):
            reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204);self.cors();self.end_headers()
        def do_GET(self):
            parsed=urlsplit(self.path);query=parse_qs(parsed.query);path=parsed.path
            site='a'if self.headers['Host'].startswith('127.0.0.1:')else'b'
            hit={'site':site,'path':path,'phase':query.get('phase',[''])[0],'mode':query.get('mode',[''])[0],'cookie':self.headers.get('Cookie',''),'origin':self.headers.get('Origin','')}
            hits.append(hit)
            if path=='/ws':
                hit['single_headers']=all(len(self.headers.get_all(name,[]))==1 for name in ['Host','Upgrade','Connection','Sec-WebSocket-Key','Sec-WebSocket-Version'])
                hit['host_includes_port']=self.headers['Host'].endswith(':'+str(server.server_port))
                key=self.headers['Sec-WebSocket-Key']
                accept=base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
                self.send_response(101);self.send_header('Upgrade','websocket');self.send_header('Connection','Upgrade')
                self.send_header('Sec-WebSocket-Accept',accept)
                self.send_header('Set-Cookie','websocket=fixture; Path=/; Secure; SameSite=None; Max-Age=3600')
                self.end_headers();data=json.dumps(hit).encode()
                frame=b'\x81'+(bytes([len(data)])if len(data)<126 else b'\x7e'+len(data).to_bytes(2,'big'))+data
                self.wfile.write(frame);self.wfile.flush()
                try:
                    self.connection.settimeout(2);self.connection.recv(1024);self.wfile.write(b'\x88\x00');self.wfile.flush()
                except OSError:pass
                return
            if path=='/redirect':
                self.send_response(302);self.cors();self.send_header('Location',third+'/echo')
                self.send_header('Content-Length','0');self.end_headers();return
            if path=='/cookie-image.svg':
                data=b'<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="green"/></svg>';kind='image/svg+xml'
            elif path in ['/echo','/auth']:
                hit['authorization']=self.headers.get('Authorization')=='Basic Zml4dHVyZTpmaXh0dXJl'
                data=json.dumps(hit).encode();kind='application/json'
            else:
                role={'/worker.js':'worker','/shared.js':'shared','/frame':'frame','/run':'run'}.get(path,'seed')
                cfg={'role':role,'phase':query.get('phase',[path])[0],'label':query.get('label',[''])[0],'first':first,'third':third}
                script='const CFG='+json.dumps(cfg)+';\n'+fixture
                data=script.encode()if path.endswith('.js')else('<title>Cookie policy fixture</title><h1>Cookie policy fixture</h1><script>'+script+'</script>').encode()
                kind='application/javascript'if path.endswith('.js')else'text/html; charset=utf-8'
            self.send_response(200);self.cors();self.send_header('Content-Type',kind)
            if path=='/cookie-image.svg':self.send_header('Set-Cookie','svgresponse=fixture; Path=/; Secure; SameSite=None; Max-Age=3600')
            if path=='/seed':self.send_header('Set-Cookie',f'seed=fixture-{site}; Path=/; Secure; SameSite=None; Max-Age=3600')
            if path=='/echo':self.send_header('Set-Cookie','response=fixture; Path=/; Secure; SameSite=None; Max-Age=3600')
            if path=='/frame':self.send_header('Set-Cookie','navigation=fixture; Path=/; Secure; SameSite=None; Max-Age=3600')
            self.send_header('Cache-Control','no-store');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True
    other_server=http.server.ThreadingHTTPServer(('127.0.0.2',server.server_port),Handler);other_server.daemon_threads=True
    tls=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);tls.load_cert_chain(runtime/'server.pem',runtime/'server.key')
    server.socket=tls.wrap_socket(server.socket,server_side=True)
    other_server.socket=tls.wrap_socket(other_server.socket,server_side=True)
    first='https://127.0.0.1:'+str(server.server_port);third='https://127.0.0.2:'+str(server.server_port)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    threading.Thread(target=other_server.serve_forever,daemon=True).start()
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
    def launch(phase):
        return start([str(a.binary.resolve()),'--no-restore','--profile',str(profile),first+'/seed?phase='+phase],'browser-'+phase,env)
    app=launch('seed-a')
    def window(title):
        return wait(lambda:next((w for w in xd.screen().root.query_tree().children if title in str(w.get_wm_name()or'')),None))
    main=window('Cookie policy fixture')
    wait(lambda:any(r.get('phase')=='seed-a'for r in reports))
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
    navigate(third+'/seed?phase=seed-b','seed-b')
    navigate(first+'/seed?phase=return-a','return-a')
    service = main.get_full_property(xd.intern_atom('_KDE_NET_WM_APPMENU_SERVICE_NAME'),X.AnyPropertyType)
    menu_path = main.get_full_property(xd.intern_atom('_KDE_NET_WM_APPMENU_OBJECT_PATH'),X.AnyPropertyType)
    check(service is not None and menu_path is not None, 'native browser exports its universal menu')
    async def menu_action(label, inspect=False):
        bus = await MessageBus().connect()
        try:
            name=bytes(service.value).decode(); object_path=bytes(menu_path.value).decode()
            introspection=await bus.introspect(name,object_path)
            interface=bus.get_proxy_object(name,object_path,introspection).get_interface('com.canonical.dbusmenu')
            _, layout=await interface.call_get_layout(0,-1,[])
            def find(node):
                while isinstance(node,Variant):node=node.value
                item,properties,children=node
                if properties.get('label',Variant('s','')).value == label:return (item,properties)
                for child in children:
                    result=find(child)
                    if result is not None:return result
            found=find(layout)
            assert found is not None,label
            identifier,properties=found
            if not inspect:await interface.call_event(identifier,'clicked',Variant('i',0),0)
            return {key:value.value for key,value in properties.items()}
        finally:bus.disconnect()
    def menu(label, inspect=False):return asyncio.run(menu_action(label,inspect))
    def probe(phase,blocked):
        r=navigate(first+'/run?phase='+phase,phase)
        image_hits=[hit for hit in hits if hit['path']=='/cookie-image.svg'and hit['phase']==phase]
        check(len(image_hits)==6 and len(r['svgImages'])==6 and all(image['loaded']for image in r['svgImages']),phase+': six native SVG image loads complete')
        for site in ['a','b']:
            for mode in ['none','anonymous','use-credentials']:
                selected=[hit for hit in image_hits if hit['site']==site and hit['mode']==mode]
                check(len(selected)==1,phase+': SVG request identity '+site+'/'+mode)
                expected=site=='a'or(not blocked and mode!='anonymous')
                check(('seed=fixture-'+site in selected[0]['cookie'])==expected,phase+': SVG image cookies '+site+'/'+mode)

        check('seed=fixture-a'in r['top'] and 'seed=fixture-a'in r['shared']['cookie'],phase+': first-party document and SharedWorker retain cookies')
        for label in ['same','no-referrer']:
            f=r['frames'][label]
            check(all('seed=fixture-a'in value for value in [f['before'],f['after'],f['network']['cookie'],f['worker']['cookie'],f['worker']['child']['cookie'],f['shared']['cookie'],f['ws']['cookie']]),phase+': '+label+' frame and workers retain first-party cookies')
        for label,site in [('cross','b'),('nested','a')]:
            f=r['frames'][label];values=[f['before'],f['after'],f['network']['cookie'],f['worker']['cookie'],f['worker']['child']['cookie'],f['shared']['cookie'],f['ws']['cookie']]
            check(all(not value if blocked else 'seed=fixture-'+site in value for value in values),phase+': '+label+' frame, nested workers, named SharedWorker and WebSocket follow cookie policy')
            if f['store'].get('supported')and blocked:check(f['store'].get('error')=='SecurityError',phase+': Cookie Store respects blocking')
        check(all(not value if blocked else 'seed=fixture-b'in value for value in [r['cross']['cookie'],r['redirected']['cookie'],r['ws']['cookie']]),phase+': cross-site fetch, redirect and WebSocket sending follow policy')
        check(r['authorization']['authorization'],phase+': cookie blocking preserves explicit HTTP authorization')
        sandbox=r['frames']['sandbox']
        check(sandbox['cookieError']=='SecurityError' and (not sandbox['network']['cookie']if blocked else 'seed=fixture-a'in sandbox['network']['cookie']),phase+': sandboxed opaque document and fetch cannot bypass policy')
        if not a.baseline:
            check(r['ws']['origin']==first and r['ws']['site']=='b',phase+': WebSocket handshake uses the initiating origin')
            check(r['ws']['single_headers']and r['ws']['host_includes_port'],phase+': WebSocket handshake sends one key and the correct Host port')
        return r
    if a.baseline:
        probe('baseline',False)
    else:
        check(menu('Block third-party cookies',True).get('toggle-state')==1,'blocking is the default and the universal-menu checkbox is checked')
        probe('blocked',True)
        rows=[json.loads(line)for line in(profile/'ck.txt').read_text().splitlines()if line and not line.startswith('#')]
        b=[row for row in rows if '127.0.0.2'in row['cookie']]
        check(len(b)==1 and b[0]['cookie'].startswith('seed='),'blocked HTTP, iframe, script and WebSocket responses do not write third-party cookies')
        menu('Block third-party cookies');wait(lambda:json.loads((profile/'cookie-policy.json').read_text())['block_third_party']is False)
        check((profile/'cookie-policy.json').stat().st_mode&0o777==0o600,'cookie policy persists privately')
        check(menu('Block third-party cookies',True).get('toggle-state')==0,'allowing cookies updates the native checkbox')
        probe('allowed',False)
        key('q',True);app.wait(timeout=15);check(app.returncode==0,'first browser session closes cleanly')
        app=launch('restart');main=window('Cookie policy fixture');wait(lambda:any(r.get('phase')=='restart'for r in reports))
        service=main.get_full_property(xd.intern_atom('_KDE_NET_WM_APPMENU_SERVICE_NAME'),X.AnyPropertyType)
        menu_path=main.get_full_property(xd.intern_atom('_KDE_NET_WM_APPMENU_OBJECT_PATH'),X.AnyPropertyType)
        check(menu('Block third-party cookies',True).get('toggle-state')==0,'allow setting survives browser restart')
        probe('restart-allowed',False)
        menu('Block third-party cookies');wait(lambda:json.loads((profile/'cookie-policy.json').read_text())['block_third_party']is True)
        probe('blocked-again',True)
        policy=profile/'cookie-policy.json';policy.unlink();policy.mkdir()
        menu('Block third-party cookies');time.sleep(.3)
        check(menu('Block third-party cookies',True).get('toggle-state')==1,'failed policy save leaves blocking enabled')
        probe('save-failure',True);policy.rmdir()
        key('q',True);app.wait(timeout=15);check(app.returncode==0,'second session closes cleanly')
        policy.write_text('{invalid-cookie-policy-fixture')
        app=launch('invalid-policy');main=window('Cookie policy fixture');wait(lambda:any(r.get('phase')=='invalid-policy'for r in reports))
        service=main.get_full_property(xd.intern_atom('_KDE_NET_WM_APPMENU_SERVICE_NAME'),X.AnyPropertyType)
        menu_path=main.get_full_property(xd.intern_atom('_KDE_NET_WM_APPMENU_OBJECT_PATH'),X.AnyPropertyType)
        check(policy.read_text()=='{invalid-cookie-policy-fixture' and menu('Block third-party cookies',True).get('toggle-state')==1,'invalid saved policy is preserved and fails closed on startup')
        probe('invalid-policy-blocked',True)
    key('q',True);app.wait(timeout=15);check(app.returncode==0,'browser closes cleanly after cookie policy checks')
    (out/'report.json').write_text(json.dumps({'passed':True,'baseline':a.baseline,'binary_sha256':hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest(),'checks':checks},indent=2)+'\n')
finally:
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
