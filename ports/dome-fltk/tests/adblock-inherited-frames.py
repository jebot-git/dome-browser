#!/usr/bin/env python3
"""Verify cosmetic filtering in inherited-origin srcdoc and about:blank frames."""
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
if os.environ.get('DOME_INHERITED_COSMETIC_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_INHERITED_COSMETIC_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest
from PIL import Image
from dbus_next.aio import MessageBus
from dbus_next import Variant

repo = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=repo / 'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument("--baseline",action="store_true",help="Record current inherited-frame filtering")
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
 with tempfile.TemporaryDirectory(prefix='dome-inherited-cosmetics-') as directory:
    runtime=Path(directory);runtime.chmod(0o700)
    fixture=Path(__file__).with_suffix('.html').read_bytes()
    child=Path(__file__).with_name('adblock-inherited-frames-child.html').read_bytes()
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def do_POST(self):
            reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204);self.end_headers()
        def do_GET(self):
            path=urlsplit(self.path).path
            data=child if path in ['/child','/child-http']else fixture
            data=data.replace(b'__BASE_URL__',('http://localhost:'+str(self.server.server_port)+'/').encode())
            if path=='/child-http':data=data.replace(b'__FRAME_ID__',b'http')
            self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8')
            self.send_header('Content-Security-Policy',"default-src 'self'; script-src 'nonce-fixture'; style-src 'nonce-fixture' 'unsafe-inline'")
            self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
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
    (profile/'adblock.txt').write_text('##.ad\n127.0.0.1##.site-ad\n##.allowed\n127.0.0.1#@#.allowed\nlocalhost#@#.ad\n')
    app=start([str(a.binary.resolve()),'--no-restore','--profile',str(profile),origin+'/progress'],'browser',env)
    main=wait(lambda:next((w for w in xd.screen().root.query_tree().children if 'Inherited frame cosmetics'in str(w.get_wm_name()or'')),None))
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
    (out/'observations.json').write_text(json.dumps(result,indent=2)+'\n')
    check('error'not in result,'fixture completes without script errors')
    by_frame={r['frame']:r for r in result['frames']}
    check(set(by_frame)=={'http','srcdoc','blank','opaque'},'ordinary, inherited and opaque frames execute and report')
    check(by_frame['http']['generic']=='none'and by_frame['http']['site']=='none','ordinary HTTP frame retains generic and hostname filtering')
    check(by_frame['srcdoc']['url']=='about:srcdoc'and by_frame['blank']['url']=='about:blank','test documents retain their native local URLs')
    check(by_frame['opaque']['messageOrigin']=='null','sandboxed srcdoc keeps an opaque origin')
    check(all(r['base'].startswith('http://localhost:')for r in by_frame.values()),'author base points at the host with a generic-hide exception')
    failures=[]
    for name in ['http','srcdoc','blank','opaque']:
        check(by_frame[name]['content']=='block',name+' preserves legitimate content')
        for filter_kind in ['generic','site','allowed']:
            expected='none'if filter_kind=='generic'or(filter_kind=='site'and name!='opaque')or(filter_kind=='allowed'and name=='opaque')else'block'
            ok=by_frame[name][filter_kind]==expected;description=name+' '+filter_kind+' display is '+expected
            if a.baseline and name!='http':
                checks.append({'check':description,'passed':ok});print(('PASS 'if ok else'FAIL ')+description,flush=True)
            else:check(ok,description)
            if not ok:failures.append(description)
    time.sleep(.2)
    g=main.get_geometry();raw=main.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX').save(out/'inherited-frames.png')
    key('q',True);app.wait(timeout=15);check(app.returncode==0,'browser closes cleanly after inherited-frame filtering')
    (out/'report.json').write_text(json.dumps({'passed':not failures,'baseline':a.baseline,'binary_sha256':hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest(),'checks':checks},indent=2)+'\n')

finally:
 (out/'fixture-reports.json').write_text(json.dumps(reports,indent=2)+'\n')
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
