#!/usr/bin/env python3
"""Exercise native unsupported-media handoff without launching a real player."""
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
if os.environ.get('DOME_EXTERNAL_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_EXTERNAL_TEST_BUS': '1'})
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
 with tempfile.TemporaryDirectory(prefix='dome-external-media-') as directory:
    runtime = Path(directory); runtime.chmod(0o700)
    launches = runtime / 'launches.jsonl'
    fake = runtime / 'fake-player.py'
    fake.write_text('#!/usr/bin/python3\nimport json,pathlib,sys\nwith pathlib.Path(__file__).with_name("launches.jsonl").open("a") as stream:stream.write(json.dumps(sys.argv[1:])+"\\n")\n')
    fake.chmod(0o700)
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_POST(self):
            reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204); self.end_headers()
        def do_GET(self):
            name = self.path.split('?')[0]
            sources = ''
            if name in ['/unsupported', '/multiple']:
                sources = '<video id="media" controls preload="none"><source src="/stream.mkv?token=test-secret" type="video/x-dome-unsupported">'
                if name == '/multiple': sources += '<source src="/second.bin" type="video/x-dome-other">'
                sources += '</video>'
            if name == '/blocked': sources = '<video controls preload="none"><source src="file:///etc/passwd" type="video/x-dome-unsupported"></video>'
            if name == '/link': sources = '<a href="/linked.mkv" style="position:fixed;left:20px;top:40px;width:300px;height:60px;display:block;background:#bdf">External media link</a>'
            data = (f'<title>External media {name}</title><h1>External media fixture</h1>{sources}'
                    '<script>fetch("/report",{method:"POST",body:JSON.stringify({path:location.pathname,support:document.createElement("video").canPlayType("video/x-dome-unsupported")})})</script>').encode()
            self.send_response(200); self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    origin = 'http://127.0.0.1:' + str(server.server_port)
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
    profile = runtime / 'profile'; profile.mkdir(mode=0o700)
    # Profile configuration, not an environment override, drives the real launch path.
    config = profile / 'external-player.json'
    config.write_text(json.dumps({'version':1,'executable':str(fake)})); config.chmod(0o600)
    app = start([str(a.binary.resolve()), '--no-restore', '--profile', str(profile), origin+'/unsupported'], 'browser', env)
    def window(title):
        return wait(lambda: next((w for w in xd.screen().root.query_tree().children if title in str(w.get_wm_name() or '')),None))
    main = window('External media /unsupported')
    wait(lambda: any(r['path']=='/unsupported' for r in reports))
    check(reports[-1]['support']=='', 'fixture MIME type is unsupported by native GStreamer')
    time.sleep(.5)
    check(not launches.exists(), 'page loading and media errors never launch an application automatically')
    def key(name, control=False):
        main.set_input_focus(X.RevertToParent,X.CurrentTime)
        if control: xtest.fake_input(xd,X.KeyPress,xd.keysym_to_keycode(XK.XK_Control_L))
        code=xd.keysym_to_keycode(XK.string_to_keysym(name))
        xtest.fake_input(xd,X.KeyPress,code);xtest.fake_input(xd,X.KeyRelease,code)
        if control: xtest.fake_input(xd,X.KeyRelease,xd.keysym_to_keycode(XK.XK_Control_L))
        xd.sync()
    def type_text(text):
        for char in text:
            sym = XK.string_to_keysym(char) or ord(char)
            code = xd.keysym_to_keycode(sym)
            shifted = xd.keycode_to_keysym(code,1) == sym
            if shifted: xtest.fake_input(xd,X.KeyPress,xd.keysym_to_keycode(XK.XK_Shift_L))
            xtest.fake_input(xd,X.KeyPress,code);xtest.fake_input(xd,X.KeyRelease,code)
            if shifted: xtest.fake_input(xd,X.KeyRelease,xd.keysym_to_keycode(XK.XK_Shift_L))
        xd.sync()
    def navigate(path):
        key('l',True);type_text(origin+path);key('Return')
        wait(lambda: any(r['path']==path for r in reports))
    service = main.get_full_property(xd.intern_atom('_KDE_NET_WM_APPMENU_SERVICE_NAME'),X.AnyPropertyType)
    path = main.get_full_property(xd.intern_atom('_KDE_NET_WM_APPMENU_OBJECT_PATH'),X.AnyPropertyType)
    check(service is not None and path is not None, 'native browser exports its universal menu')
    async def menu_action(label, inspect=False):
        bus = await MessageBus().connect()
        try:
            name=bytes(service.value).decode(); object_path=bytes(path.value).decode()
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
    def calls():return [json.loads(line)for line in launches.read_text().splitlines()]if launches.exists()else[]
    menu('Open media in external player');wait(lambda:len(calls())==1)
    check(calls()[0]==['--',origin+'/stream.mkv?token=test-secret'], 'unsupported media URL is passed intact as one argument after --')
    check('test-secret' not in (out/'browser.log').read_text(), 'browser logs do not expose media query tokens')
    navigate('/empty');menu('Open media in external player');wait(lambda:len(calls())==2)
    check(calls()[1]==['--',origin+'/empty'], 'pages without media elements can use the external website extractor')
    navigate('/blocked');menu('Open media in external player');wait(lambda:len(calls())==3)
    check(calls()[2]==['--',origin+'/blocked'], 'page-controlled local-file URLs are never passed to the external player')
    navigate('/multiple');menu('Open media in external player');time.sleep(.4)
    key('Down');key('Return');wait(lambda:len(calls())==4)
    check(calls()[3]==['--',origin+'/stream.mkv?token=test-secret'], 'multiple unsupported sources use a native chooser')
    configured_before = config.stat().st_mtime_ns
    menu('External player...');time.sleep(.3)
    key('a',True);type_text(str(fake));key('Return');time.sleep(.3)
    wait(lambda:config.stat().st_mtime_ns > configured_before)
    check(json.loads(config.read_text())['executable']==str(fake) and config.stat().st_mode&0o777==0o600,
          'native player configuration persists privately')
    navigate('/link')
    geometry = main.get_geometry()
    xtest.fake_input(xd,X.MotionNotify,x=geometry.x+60,y=geometry.y+180)
    xtest.fake_input(xd,X.ButtonPress,3);xtest.fake_input(xd,X.ButtonRelease,3);xd.sync();time.sleep(.3)
    root = xd.screen().root; size = root.get_geometry()
    image = root.get_image(0,0,size.width,size.height,X.ZPixmap,0xffffffff)
    Image.frombytes('RGB',(size.width,size.height),image.data,'raw','BGRX').save(out/'context-popup.png')
    popup = wait(lambda:next((w for w in xd.screen().root.query_tree().children
        if w.get_attributes().map_state==X.IsViewable and w.get_attributes().override_redirect
        and 100<w.get_geometry().width<600 and 60<w.get_geometry().height<500),None))
    popup_geometry = popup.get_geometry()
    xtest.fake_input(xd,X.MotionNotify,x=popup_geometry.x+100,y=popup_geometry.y+popup_geometry.height-10)
    xtest.fake_input(xd,X.ButtonPress,1);xtest.fake_input(xd,X.ButtonRelease,1);xd.sync()
    wait(lambda:len(calls())==5,seconds=8)
    check(calls()[4]==['--',origin+'/linked.mkv'], 'link context menu hands off directly to the external player')
    navigate('/empty');fake.chmod(0o600);menu('Open media in external player');time.sleep(.5)
    check(len(calls())==5 and app.poll()is None, 'a missing or non-executable player produces no launch and leaves browser running')
    fake.chmod(0o700)
    key('q',True);app.wait(timeout=15)
    check(app.returncode==0, 'browser shuts down cleanly after external handoffs')
    (out/'report.json').write_text(json.dumps({'checks':checks,'passed':True,'launches':len(calls())},indent=2)+'\n')
finally:
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
