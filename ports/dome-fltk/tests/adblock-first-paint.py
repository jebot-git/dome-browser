#!/usr/bin/env python3
"""Verify cosmetic filtering never paints an ad before its native filter reply."""
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
if os.environ.get('DOME_COSMETIC_PAINT_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_COSMETIC_PAINT_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest
from PIL import Image
from dbus_next.aio import MessageBus
from dbus_next import Variant

repo = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=repo / 'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument("--baseline",action="store_true",help="Record any initial ad frames")
p.add_argument("--disabled",action="store_true",help="Verify the explicit blocking-disabled control")
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
 with tempfile.TemporaryDirectory(prefix='dome-cosmetic-paint-') as directory:
    runtime=Path(directory);runtime.chmod(0o700)
    frame_sets=[];requests=[]
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*_):pass
        def do_GET(self):
            path=urlsplit(self.path).path;requests.append(path)
            if path=='/bootstrap':data=b'<!doctype html><title>Paint bootstrap</title><body>Bootstrap'
            else:
                data=b'<!doctype html><title>First paint fixture</title><style nonce="fixture">body{background:white}.ad{position:fixed;left:20px;top:20px;width:100px;height:100px;background:red;display:block!important}.control{position:fixed;left:20px;top:140px;width:100px;height:100px;background:lime}</style>'
            self.send_response(200);self.send_header('Content-Type','text/html')
            self.send_header('Content-Security-Policy',"script-src 'none'; style-src 'nonce-fixture'")
            if path=='/bootstrap':self.send_header('Content-Length',str(len(data)))
            else:self.send_header('Transfer-Encoding','chunked')
            self.send_header('Cache-Control','no-store');self.end_headers()
            if path=='/bootstrap':self.wfile.write(data);return
            def chunk(value):self.wfile.write(('%x\r\n'%len(value)).encode()+value+b'\r\n');self.wfile.flush()
            chunk(data);time.sleep(.2)
            chunk(b'<div class="ad"></div><div class="control"></div>');self.wfile.write(b'0\r\n\r\n');self.wfile.flush()
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
    (profile/'adblock.txt').write_text('##.ad\n')
    if a.disabled:(profile/'adblock-disabled').write_text('explicit fixture control\n')
    app=start([str(a.binary.resolve()),'--no-restore','--profile',str(profile),origin+'/bootstrap'],'browser',env)
    main=wait(lambda:next((w for w in xd.screen().root.query_tree().children if 'Paint bootstrap'in str(w.get_wm_name()or'')),None))
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
    def navigate(path):key('l',True);type_text(origin+path);key('Return')
    def bootstrap_cleared():
        raw=main.get_image(0,120,400,400,X.ZPixmap,0xffffffff)
        im=Image.frombytes('RGB',(400,400),raw.data,'raw','BGRX')
        return not any(n>5000 and ((c[0]>240 and c[1]<10 and c[2]<10)or(c[1]>240 and c[0]<10 and c[2]<10)) for n,c in(im.getcolors(160000)or[]))
    time.sleep(.3)
    for trial in range(5):
        if trial:
            navigate('/bootstrap');wait(lambda:'Paint bootstrap'in str(main.get_wm_name()or''));time.sleep(.2)
        wait(bootstrap_cleared)
        stop=threading.Event();frames=[]
        def sample():
            reader=display.Display(env['DISPLAY']);w=reader.create_resource_object('window',main.id)
            started=time.monotonic();first_red=None
            try:
                while not stop.is_set():
                    raw=w.get_image(0,120,400,400,X.ZPixmap,0xffffffff)
                    im=Image.frombytes('RGB',(400,400),raw.data,'raw','BGRX')
                    colors=im.getcolors(160000)or[]
                    red=sum(n for n,c in colors if c[0]>240 and c[1]<10 and c[2]<10)
                    green=sum(n for n,c in colors if c[1]>240 and c[0]<10 and c[2]<10)
                    frames.append({'ms':round((time.monotonic()-started)*1000,2),'red':red,'green':green})
                    if red>5000 and first_red is None:first_red=True;im.save(out/f'trial-{trial}-ad-frame.png')
                    time.sleep(.002)
            finally:reader.close()
        thread=threading.Thread(target=sample);thread.start()
        navigate('/paint?trial='+str(trial));time.sleep(1.4);stop.set();thread.join(timeout=5)
        assert not thread.is_alive(),'capture thread did not stop'
        frame_sets.append(frames)
        g=main.get_geometry();raw=main.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
        Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX').save(out/f'trial-{trial}-end.png')
        print('PAGE',main.get_wm_name(),requests,flush=True)
        check(len(frames)>30 and any(f['green']>8000 for f in frames),'legitimate control paints during trial '+str(trial))
    red_frames=sum(f['red']>5000 for frames in frame_sets for f in frames)
    (out/'frames.json').write_text(json.dumps(frame_sets,indent=2)+'\n')
    if not a.baseline:check(red_frames>0 if a.disabled else red_frames==0,'disabled control paints the ad'if a.disabled else'no ad frame is painted before cosmetic rules apply')
    key('q',True);app.wait(timeout=15);check(app.returncode==0,'browser closes cleanly after paint capture')
    (out/'report.json').write_text(json.dumps({'passed':(red_frames>0 if a.disabled else red_frames==0),'disabled':a.disabled,'baseline':a.baseline,'red_frames':red_frames,'samples':sum(map(len,frame_sets)),'binary_sha256':hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest(),'checks':checks},indent=2)+'\n')
finally:
 (out/'frames-partial.json').write_text(json.dumps(locals().get('frame_sets',[]),indent=2)+'\n')
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
