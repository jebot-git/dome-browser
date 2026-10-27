#!/usr/bin/env python3
"""Verify SVG document title notifications and SVGTitleElement binding."""
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
if os.environ.get('DOME_SVG_TITLE_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_SVG_TITLE_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest
from PIL import Image
from dbus_next.aio import MessageBus
from dbus_next import Variant

repo = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=repo / 'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument("--baseline",action="store_true",help="Record the earlier title-notification failures")
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
 with tempfile.TemporaryDirectory(prefix='dome-svg-title-') as directory:
    runtime=Path(directory);runtime.chmod(0o700)
    script=Path(__file__).with_suffix('.js').read_text()
    command={'step':0};observations=[]
    svg='<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200"><title id="primary">Initial SVG</title><title id="backup">Backup title</title><rect width="400" height="200" fill="cyan"/>'
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*_):pass
        def do_POST(self):
            reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204);self.send_header('Content-Length','0');self.end_headers()
        def do_GET(self):
            path=urlsplit(self.path).path
            if path=='/command':data=json.dumps(command).encode();kind='application/json'
            elif path=='/html':
                data=('<!doctype html><title>HTML control</title>'+svg+'</svg><script nonce="fixture">'+script+'</script>').encode();kind='text/html; charset=utf-8'
            else:
                data=(svg+'<foreignObject><script xmlns="http://www.w3.org/1999/xhtml" nonce="fixture"><![CDATA['+script+']]></script></foreignObject></svg>').encode();kind='image/svg+xml'
            self.send_response(200);self.send_header('Content-Type',kind)
            self.send_header('Content-Security-Policy',"default-src 'self'; script-src 'nonce-fixture'")
            self.send_header('Content-Length',str(len(data)));self.send_header('Cache-Control','no-store');self.end_headers();self.wfile.write(data)
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
    app=start([str(a.binary.resolve()),'--no-restore','--profile',str(profile),origin+'/svg'],'browser',env)
    main=wait(lambda:next((w for w in xd.screen().root.query_tree().children if w.get_wm_name()),None))
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
    def native_title():
        value=main.get_full_property(xd.intern_atom('_NET_WM_NAME'),X.AnyPropertyType)
        if value is None:value=main.get_full_property(xd.intern_atom('WM_NAME'),X.AnyPropertyType)
        return bytes(value.value).decode('utf-8',errors='replace').rsplit(' — ',1)[0]if value else''
    svg_titles=['Initial SVG','Text mutation','Replacement title','API title','First title','API title','Backup title','','Restored title','','Nested text','Root replacement','Root replacement','Unicode Řeka 日本語 Title','Atomic first','Unicode Řeka 日本語 Title']
    for path in ['/svg','/html']:
        offset=len(reports)if path=='/html'else 0
        if path=='/html':
            command['step']=0;key('l',True);type_text(origin+path);key('Return')
        for step,svg_title in enumerate(svg_titles):
            command['step']=step
            result=wait(lambda:next((r for r in reports[offset:]if r['step']==step and r.get('documentURL')==origin+path),None))
            assert 'error'not in result,result
            expected=svg_title if path=='/svg'else('HTML control'if step<3 else('Unicode Řeka 日本語 Title'if step>=13 else'API title'))
            until=time.monotonic()+(0.1 if a.baseline else 3)
            while time.monotonic()<until and native_title()!=(expected or'Untitled'):time.sleep(.02)
            item={**result,'path':path,'expected':expected,'native':native_title()}
            observations.append(item)
            check(result['title']==expected,f'{path} DOM title step {step}')
    (out/'observations.json').write_text(json.dumps(observations,indent=2)+'\n')
    ui_failures=[o for o in observations if o['native']!=(o['expected']or'Untitled')]
    interface_failures=[o for o in observations if not o['interface']or o['tag']!='[object SVGTitleElement]'or not o['illegalConstructor']]
    check(bool(ui_failures)if a.baseline else not ui_failures,'baseline records missing native title notifications'if a.baseline else'native window titles match DOM titles across all mutations')
    check(bool(interface_failures)if a.baseline else not interface_failures,'baseline records missing SVGTitleElement interface'if a.baseline else'SVGTitleElement interface, prototype and illegal constructor behavior')
    key('q',True);app.wait(timeout=15);check(app.returncode==0,'browser closes cleanly after title/root mutations')
    (out/'report.json').write_text(json.dumps({'passed':True,'baseline':a.baseline,'binary_sha256':hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest(),'checks':checks,'ui_failures':ui_failures,'interface_failures':interface_failures},indent=2)+'\n')
finally:
 (out/'fixture-reports.json').write_text(json.dumps(reports,indent=2)+'\n')
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
