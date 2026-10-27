#!/usr/bin/env python3
"""Exercise editable ck.txt persistence through HTTP, DOM and native FLTK controls."""
import argparse
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
if os.environ.get('DOME_COOKIE_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_COOKIE_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest
from PIL import Image
from dbus_next.aio import MessageBus
from dbus_next import Variant

repo = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=repo / 'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
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
 with tempfile.TemporaryDirectory(prefix='dome-cookie-file-') as directory:
    runtime = Path(directory); runtime.chmod(0o700)
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def do_POST(self):
            reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204);self.end_headers()
        def do_GET(self):
            path=self.path.split('?')[0]
            if path=='/echo':
                data=json.dumps({'cookies':self.headers.get('Cookie','')}).encode();kind='application/json'
            else:
                seed="document.cookie='script=script-fixture; Path=/; Max-Age=3600';" if path=='/seed' else ''
                data=("<title>Cookie fixture</title><h1>Cookie fixture</h1><script>"+seed+
                      "fetch('/echo').then(r=>r.json()).then(r=>fetch('/report',{method:'POST',body:JSON.stringify({path:location.pathname,script:document.cookie,network:r.cookies})}));</script>").encode();kind='text/html'
            self.send_response(200);self.send_header('Content-Type',kind)
            if path=='/seed':
                self.send_header('Set-Cookie','httpOnly=http-secret-fixture; HttpOnly; Path=/; Max-Age=3600')
                self.send_header('Set-Cookie','visible=before-fixture; Path=/; Max-Age=3600')
            self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    origin='http://127.0.0.1:'+str(server.server_port)
    env = {**os.environ, 'FLTK_BACKEND':'x11', 'XDG_RUNTIME_DIR':directory,
           'XDG_CONFIG_HOME':str(runtime/'config'), 'XDG_DATA_HOME':str(runtime/'data'),
           'XDG_CACHE_HOME':str(runtime/'cache'), 'XDG_CURRENT_DESKTOP':'Dome',
           'LIBGL_ALWAYS_SOFTWARE':'1', 'XMODIFIERS':'@im=none', 'RUST_LOG':'net=debug,warn', 'DOME_BROWSER_TRACE':'1'}
    for key in ['DISPLAY','WAYLAND_DISPLAY','WAYLAND_SOCKET','DOME_BROWSER_EXTERNAL_PLAYER']:
        env.pop(key,None)
    rd, wr = os.pipe()
    start(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'], 'xvfb', env, pass_fds=(wr,))
    os.close(wr)
    with os.fdopen(rd) as stream: env['DISPLAY'] = ':' + stream.readline().strip()
    xd = display.Display(env['DISPLAY'])
    profile=runtime/'profile';profile.mkdir(mode=0o700)
    app=start([str(a.binary.resolve()),'--no-restore','--profile',str(profile),origin+'/seed'],'browser',env)
    def window(title):
        return wait(lambda:next((w for w in xd.screen().root.query_tree().children if title in str(w.get_wm_name()or'')),None))
    main=window('Cookie fixture')
    wait(lambda:reports)
    check('httpOnly='not in reports[-1]['script'] and 'httpOnly=http-secret-fixture'in reports[-1]['network'], 'HttpOnly cookie is withheld from script but sent over HTTP')
    path=profile/'ck.txt'
    def rows():return [json.loads(line)for line in path.read_text().splitlines()if line and not line.startswith('#')]
    wait(lambda:len(rows())==3)
    check(path.stat().st_mode&0o777==0o600 and not(profile/'cookie_jar.json').exists(),'cookies persist immediately in private ck.txt')
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
    def navigate(path):
        key('l',True);type_text(origin+path);key('Return')
        return wait(lambda:next((r for r in reports if r['path']==path),None))
    def click(w,x,y):
        g=w.get_geometry();xtest.fake_input(xd,X.MotionNotify,x=g.x+x,y=g.y+y)
        xtest.fake_input(xd,X.ButtonPress,1);xtest.fake_input(xd,X.ButtonRelease,1);xd.sync()
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
                if properties.get('label',Variant('s','')).value == label:return item
                for child in children:
                    result=find(child)
                    if result is not None:return result
            identifier=find(layout)
            assert identifier is not None, label
            if not inspect:await interface.call_event(identifier,'clicked',Variant('i',0),0)
        finally:bus.disconnect()
    def menu(label, inspect=False):asyncio.run(menu_action(label,inspect))
    original=path.read_text()
    menu('View or edit ck.txt');dialog=window('Cookies - ck.txt');time.sleep(.25)
    check(dialog.get_attributes().map_state==X.IsViewable,'native Tools cookie editor opens')
    click(dialog,820,650);time.sleep(.2)
    check(path.read_text()==original,'cancel leaves cookie file unchanged')
    menu('View or edit ck.txt');dialog=window('Cookies - ck.txt');time.sleep(.25)
    key('a',True,target=dialog);key('BackSpace',target=dialog)
    type_text(original.replace('visible=before-fixture','visible=edited-fixture'))
    click(dialog,930,650)
    wait(lambda:'visible=edited-fixture'in path.read_text())
    r=navigate('/edited')
    check('visible=edited-fixture'in r['script'] and 'visible=edited-fixture'in r['network'],'native editor changes are reloaded by DOM and HTTP cookie access')
    saved=path.read_text();path.write_text('{malformed-cookie-fixture')
    r=navigate('/malformed')
    check(r['script']=='' and r['network']=='' and path.read_text()=='{malformed-cookie-fixture','malformed edits withhold persisted cookies without overwriting the file')
    path.write_text(saved);r=navigate('/repaired')
    check('visible=edited-fixture'in r['network'],'repairing ck.txt resumes cookie access')
    menu('Delete all cookies');time.sleep(.3);key('Right');key('Return');time.sleep(.3)
    r=navigate('/cleared')
    check(r['script']=='' and r['network']=='' and rows()==[],'native delete-all clears DOM, HTTP and persisted cookies')
    key('q',True);app.wait(timeout=15)
    check(app.returncode==0,'browser shuts down cleanly after cookie editing')
    log=(out/'browser.log').read_text()
    check(not any(value in log for value in ['http-secret-fixture','before-fixture','edited-fixture','script-fixture']), 'debug logs omit cookie and Set-Cookie values')
    (out/'report.json').write_text(json.dumps({'passed':True,'checks':checks},indent=2)+'\n')
finally:
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
