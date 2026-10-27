#!/usr/bin/env python3
"""Exercise SameSite selection, receipt, navigation and reload over local HTTPS."""
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
if os.environ.get('DOME_SAMESITE_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_SAMESITE_TEST_BUS': '1'})
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
            if urlsplit(self.path).path!='/report':
                self.rfile.read(int(self.headers.get('Content-Length','0')));self.do_GET();return
            reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204);self.cors();self.end_headers()
        def do_GET(self):
            parsed=urlsplit(self.path);query=parse_qs(parsed.query);path=parsed.path
            site='a'if self.headers['Host'].startswith('127.0.0.1:')else'b'
            hit={'site':site,'path':path,'cookie':self.headers.get('Cookie',''),'origin':self.headers.get('Origin',''),'method':self.command}
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
            if path in ['/redirect','/redirect303','/redirect307']:
                target=first+'/echo'if query.get('to')else third+('/redirect?to=first'if query.get('back')else'/echo')
                if path!='/redirect':target=third+'/nav?phase='+('post-303'if path=='/redirect303'else'post-307')
                self.send_response(302 if path=='/redirect'else 303 if path=='/redirect303'else 307);self.cors();self.send_header('Location',target)
                self.send_header('Content-Length','0');self.end_headers();return
            if path in ['/echo','/auth']:
                hit['authorization']=self.headers.get('Authorization')=='Basic Zml4dHVyZTpmaXh0dXJl'
                data=json.dumps(hit).encode();kind='application/json'
            else:
                role={'/worker.js':'worker','/shared.js':'shared','/frame':'frame','/run':'run','/nav':'nav','/controls':'controls'}.get(path,'seed')
                cfg={'role':role,'phase':query.get('phase',[path])[0],'label':query.get('label',[''])[0],'first':first,'third':third,'network':hit}
                script='const CFG='+json.dumps(cfg)+';\n'+fixture
                controls=''
                if path=='/controls':
                    controls=f'<a href="{third}/nav?phase=script-get" style="position:fixed;left:20px;top:80px">Navigate GET</a>'
                    for y,route,phase in [(120,'nav','script-post'),(160,'redirect303','post-303'),(200,'redirect307','post-307')]:
                        controls+=f'<form method="POST" action="{third}/{route}?phase={phase}"><button style="position:fixed;left:20px;top:{y}px">Navigate POST</button></form>'
                if path=='/nav':controls='<button onclick="location.reload()" style="position:fixed;left:20px;top:80px">Script reload</button>'
                data=script.encode()if path.endswith('.js')else('<title>Cookie policy fixture</title><h1>SameSite fixture</h1>'+controls+'<script>'+script+'</script>').encode()
                kind='application/javascript'if path.endswith('.js')else'text/html; charset=utf-8'
            self.send_response(200);self.cors();self.send_header('Content-Type',kind)
            if path=='/seed':
                for name,attr in [('strict','SameSite=Strict'),('lax','SameSite=Lax'),('none','SameSite=None'),('default',''),('invalid','SameSite=invalid'),('last_invalid','SameSite=Strict; SameSite=invalid')]:
                    self.send_header('Set-Cookie',f'{name}=fixture; Path=/; Secure; Max-Age=3600; '+attr)
                self.send_header('Set-Cookie','none_insecure=fixture; Path=/; SameSite=None; Max-Age=3600')
            if path=='/echo':self.send_header('Set-Cookie','response=fixture; Path=/; Secure; SameSite=None; Max-Age=3600')
            if path=='/frame':
                for name,attr in [('strict','SameSite=Strict'),('lax','SameSite=Lax'),('none','SameSite=None'),('default','')]:
                    self.send_header('Set-Cookie',f'set_{name}=fixture; Path=/; Secure; Max-Age=3600; '+attr)
            if path=='/nav':self.send_header('Set-Cookie','new_nav_strict=fixture; Path=/; Secure; SameSite=Strict; Max-Age=3600')
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
    (profile/'cookie-policy.json').write_text('{"block_third_party":false}');(profile/'cookie-policy.json').chmod(0o600)
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
    seed_names={'strict','lax','none','default','invalid','last_invalid'}
    def names(text):return {part.strip().split('=',1)[0]for part in text.split(';')if part.strip()}
    def expected(kind):
        if a.baseline:return seed_names|{'none_insecure'}
        return seed_names if kind=='same' else {'none'} if kind=='cross' else seed_names-{'strict'}
    def verify(text,kind,label):check(names(text)&(seed_names|{'none_insecure'})==expected(kind),label)
    def probe(phase,blocked=False):
        r=navigate(first+'/run?phase='+phase,phase)
        verify(r['top'],'same',phase+': first-party script sees all eligible cookies')
        for label in ['same','no-referrer','cross','nested']:
            f=r['frames'][label];cross=label in ['cross','nested']
            target=expected('cross'if cross else'same')
            if blocked and cross:target=set()
            values=[f['before'],f['after'],f['network']['cookie'],f['worker']['cookie'],f['worker']['child']['cookie'],f['shared']['cookie'],f['ws']['cookie']]
            check(all(names(v)&(seed_names|{'none_insecure'})==target for v in values),phase+': '+label+' DOM, HTTP, workers and WebSocket use SameSite context')
            allowed_js={'js_none'}if cross and not a.baseline else {'js_none','js_strict','js_lax','js_default'}
            if blocked and cross:allowed_js=set()
            check(names(f['after'])&{'js_none','js_strict','js_lax','js_default'}==allowed_js,phase+': '+label+' script writes respect SameSite')
            allowed_set={'set_none'}if cross and not a.baseline else {'set_none','set_strict','set_lax','set_default'}
            if blocked and cross:allowed_set=set()
            check(names(f['before'])&{'set_none','set_strict','set_lax','set_default'}==allowed_set,phase+': '+label+' HTTP responses respect SameSite')
        for key in ['cross','redirect','back','ws']:
            # The return hop is first-party for the user privacy setting, but
            # remains cross-site for SameSite because its chain crossed sites.
            allowed=set()if blocked and key!='back'else expected('cross')
            check(names(r[key]['cookie'])&(seed_names|{'none_insecure'})==allowed,phase+': '+key+' fetch/redirect/WebSocket selection')
        return r
    probe('allowed')
    if not a.baseline:
        rows=[json.loads(line)for line in(profile/'ck.txt').read_text().splitlines()if line and not line.startswith('#')]
        b=[row['cookie'].split('=',1)[0]for row in rows if '127.0.0.2'in row['cookie']]
        check(not set(b)&{'none_insecure','set_strict','set_lax','set_default','js_strict','js_lax','js_default'},'cross-site receipt cannot overwrite or introduce restricted cookies')
    def click(x,y):
        time.sleep(.4)
        g=main.get_geometry();xtest.fake_input(xd,X.MotionNotify,x=g.x+x,y=g.y+120+y+8)
        xtest.fake_input(xd,X.ButtonPress,1);xtest.fake_input(xd,X.ButtonRelease,1);xd.sync()
    def next_report(phase,count=1):
        return wait(lambda:[r for r in reports if r.get('phase')==phase][count-1]if len([r for r in reports if r.get('phase')==phase])>=count else None,seconds=40)
    def nav_from_page(control,phase,kind):
        navigate(first+'/controls?phase=controls-'+phase,'controls-'+phase)
        click(40,control)
        r=next_report(phase);assert 'error'not in r,r
        verify(r['network']['cookie'],kind,phase+': navigation sends only permitted cookies')
        check('new_nav_strict'in names(r['document']),phase+': top-level navigation may set Strict cookies')
        return r
    nav_from_page(80,'script-get','lax')
    key('r',True);r=next_report('script-get',2)
    verify(r['network']['cookie'],'lax','toolbar reload retains original cross-site status')
    click(40,80);r=next_report('script-get',3)
    verify(r['network']['cookie'],'same','script reload is same-site')
    nav_from_page(120,'script-post','cross')
    nav_from_page(160,'post-303','lax')
    nav_from_page(200,'post-307','cross')
    navigate(third+'/nav?phase=address-bar','address-bar')
    verify(next_report('address-bar')['network']['cookie'],'same','address-bar navigation has a null client')
    if not a.baseline:
        menu('Block third-party cookies');wait(lambda:json.loads((profile/'cookie-policy.json').read_text())['block_third_party']is True)
        probe('blocked',True)
    key('q',True);app.wait(timeout=15);check(app.returncode==0,'browser closes cleanly after SameSite checks')
    (out/'report.json').write_text(json.dumps({'passed':True,'baseline':a.baseline,'binary_sha256':hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest(),'checks':checks},indent=2)+'\n')
finally:
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
