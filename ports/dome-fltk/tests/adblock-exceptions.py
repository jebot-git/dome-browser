#!/usr/bin/env python3
"""Verify adblock site exceptions stay scoped to the top-level site."""
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
if os.environ.get('DOME_ADBLOCK_EXCEPTION_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_ADBLOCK_EXCEPTION_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest

repo = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=repo / 'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument("--baseline",action="store_true",help="Characterize the earlier embedded-origin exception leak")
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
 with tempfile.TemporaryDirectory(prefix='dome-adblock-exceptions-') as directory:
    runtime=Path(directory);runtime.chmod(0o700)
    hits=[]
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*_):pass
        def do_POST(self):
            reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204);self.send_header('Content-Length','0');self.end_headers()
        def do_GET(self):
            path=urlsplit(self.path).path
            if path=='/ad-scope.js':
                hits.append(self.path);data=b'window.adExecuted=true;';kind='application/javascript'
            else:
                phase={'/blocked-top':'blocked-top','/frame':'frame','/allow-top':'allow-top','/blocked-return':'blocked-return','/frame-return':'frame-return','/allow-frame':'allow-frame'}.get(path,'unknown')
                frame_path={'blocked-top':'frame','blocked-return':'frame-return','allow-top':'allow-frame'}.get(phase)
                frame=(f'<iframe referrerpolicy="no-referrer" src="{allowed_origin}/{frame_path}"></iframe>'if frame_path else'')
                data=(f'<!doctype html><title>Adblock exception fixture</title>{frame}'
                      f'<script src="/ad-scope.js?from={phase}"></script>'
                      '<script>addEventListener("load",()=>fetch("/report",{method:"POST",body:JSON.stringify('
                      +'Object.assign('+json.dumps({'phase':phase})+', {executed:!!window.adExecuted}))}));</script>').encode()
                kind='text/html'
            self.send_response(200);self.send_header('Content-Type',kind);self.send_header('Cache-Control','no-store')
            self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True
    origin='http://127.0.0.1:'+str(server.server_port);allowed_origin='http://localhost:'+str(server.server_port)
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
    (profile/'adblock.txt').write_text('/ad-scope.js$script\n')
    (profile/'adblock-allowlist.txt').write_text(allowed_origin+'\n')
    app=start([str(a.binary.resolve()),'--no-restore','--profile',str(profile),origin+'/blocked-top'],'browser',env)
    def window(title):return wait(lambda:next((w for w in xd.screen().root.query_tree().children if title in str(w.get_wm_name()or'')),None))
    main=window('Adblock exception fixture')
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
    wait(lambda:any(r.get('phase')=='blocked-top'for r in reports))
    wait(lambda:any(r.get('phase')=='frame'for r in reports))
    navigate(allowed_origin+'/allow-top','allow-top')
    wait(lambda:any(r.get('phase')=='allow-frame'for r in reports))
    navigate(origin+'/blocked-return','blocked-return')
    wait(lambda:any(r.get('phase')=='frame-return'for r in reports))
    key('q',True);app.wait(timeout=15)
    (out/'observations.json').write_text(json.dumps({'reports':reports,'hits':hits,'exit_code':app.returncode},indent=2)+'\n')
    check(app.returncode==0,'browser closes cleanly')
    by_phase={r['phase']:r for r in reports}
    check(not by_phase['blocked-top']['executed']and'/ad-scope.js?from=blocked-top'not in hits,'non-exempt top-level site retains network filtering')
    check(by_phase['frame']['executed']==a.baseline,'an allowlisted embedded origin cannot disable the top-level site filter')
    check(('/ad-scope.js?from=frame'in hits)==a.baseline,'blocked embedded resource never reaches the network')
    check(by_phase['allow-top']['executed']and'/ad-scope.js?from=allow-top'in hits,'exception still applies when the allowlisted site is top-level')
    check(by_phase['allow-frame']['executed'],'same-origin frame retains its exempt top-level site context without a referrer')
    check(not by_phase['frame-return']['executed']and'/ad-scope.js?from=frame-return'not in hits,'an exempt frame on a newly non-exempt page stays filtered during navigation')
    check(not by_phase['blocked-return']['executed']and'/ad-scope.js?from=blocked-return'not in hits,'leaving an exempt site restores filtering before subresources load')
    (out/'report.json').write_text(json.dumps({'passed':True,'baseline':a.baseline,'binary_sha256':hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest(),'checks':checks},indent=2)+'\n')
finally:
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
