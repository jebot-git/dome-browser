#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise explicit new-view commands from inert smallnet documents."""
import argparse, contextlib, hashlib, json, os, signal, socketserver, subprocess, sys, tempfile, threading, time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_SMALLNET_VIEW_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv], {**os.environ, 'DOME_SMALLNET_VIEW_TEST_BUS':'1'})
from PIL import Image
from Xlib import X, XK, display, protocol
from Xlib.ext import xtest
from private_wayland import PrivateWayland
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--wayland', action='store_true')
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False,mode=0o700)
checks=[];hits=[];failure=None;app=None

def check(ok,name):
    checks.append({'name':name,'passed':bool(ok)});print(('PASS ' if ok else 'FAIL ')+name,flush=True);assert ok,name

def wait(fn,seconds=12):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if app and app.poll() is not None:raise AssertionError('Browser exited '+str(app.returncode))
        v=fn()
        if v:return v
        time.sleep(.04)
    raise AssertionError('Timed out waiting for native smallnet view')

def stop(proc):
    if proc.poll() is None:
        proc.terminate()
        try:proc.wait(timeout=8)
        except subprocess.TimeoutExpired:proc.kill();proc.wait()

class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True
class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(5);line=self.rfile.readline(4096);kind=self.server.kind;port=self.server.server_address[1]
        path=line.split(b' ')[1].decode() if kind=='spartan' else line.decode().rstrip('\r\n')
        hits.append((kind,path))
        if kind=='gopher':
            body=(f'0Native new view\t/destination\t127.0.0.1\t{port}\r\n.\r\n').encode() if path=='' else b'Destination text\r\n.\r\n'
        else:
            body=b'=> /destination Native new view\n' if path=='/' else b'Destination text\n'
            if kind=='spartan':body=b'2 text/gemini\r\n'+body
        self.wfile.write(body)
try:
    with contextlib.ExitStack() as stack:
        runtime=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-smallnet-new-view-')));runtime.chmod(0o700)
        servers={}
        for kind in ['spartan','nex','gopher']:
            server=Server(('127.0.0.1',0),Handler);server.kind=kind;servers[kind]=server
            threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
        rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1400x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr)
        with os.fdopen(rd) as pipe:screen=':'+pipe.readline().strip()
        xd=display.Display(screen);stack.callback(xd.close)
        wl=PrivateWayland(xd,screen,runtime,out) if a.wayland else None
        if wl:stack.callback(wl.close)
        env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')}
        env.update(HOME=str(runtime),XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME=str(runtime/'config'),XDG_DATA_HOME=str(runtime/'data'),XDG_CACHE_HOME=str(runtime/'cache'),XDG_STATE_HOME=str(runtime/'state'),LIBGL_ALWAYS_SOFTWARE='1',DOME_BROWSER_TRACE='1',RUST_LOG='warn')
        env.pop('WAYLAND_SOCKET',None)
        if wl:env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wl.socket);env.pop('DISPLAY',None)
        else:env.update(FLTK_BACKEND='x11',DISPLAY=screen);env.pop('WAYLAND_DISPLAY',None)
        log=stack.enter_context((out/'browser.log').open('w'));profile=runtime/'profile'
        app=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',str(profile),'about:blank'],env=env,stdout=log,stderr=log);stack.callback(stop,app)
        def windows():return wl.windows() if wl else xd.screen().root.query_tree().children
        def main_window():return next((w for w in windows() if (wl or w.get_attributes().map_state==X.IsViewable) and 'Dome Browser' in str(w.get_wm_name() or '')),None)
        window=wait(main_window);window.set_input_focus(X.RevertToParent,X.CurrentTime);xd.sync()
        def key(name,ctrl=False):
            if ctrl:xtest.fake_input(xd,X.KeyPress,xd.keysym_to_keycode(XK.XK_Control_L))
            code=xd.keysym_to_keycode(XK.string_to_keysym(name));xtest.fake_input(xd,X.KeyPress,code);xtest.fake_input(xd,X.KeyRelease,code)
            if ctrl:xtest.fake_input(xd,X.KeyRelease,xd.keysym_to_keycode(XK.XK_Control_L))
            xd.sync()
        def navigate(url):
            key('l',True);key('a',True)
            for c in url:
                sym=XK.string_to_keysym(c) or ord(c);code=xd.keysym_to_keycode(sym);shift=xd.keycode_to_keysym(code,0)!=sym and xd.keycode_to_keysym(code,1)==sym
                if shift:xtest.fake_input(xd,X.KeyPress,xd.keysym_to_keycode(XK.XK_Shift_L))
                xtest.fake_input(xd,X.KeyPress,code);xtest.fake_input(xd,X.KeyRelease,code)
                if shift:xtest.fake_input(xd,X.KeyRelease,xd.keysym_to_keycode(XK.XK_Shift_L))
            key('Return')
        for kind,server in servers.items():
            origin=f'{kind}://127.0.0.1:{server.server_address[1]}'
            source=origin+('/1' if kind=='gopher' else '/')
            target=origin+('/0%2Fdestination' if kind=='gopher' else '/destination')
            navigate(source);wait(lambda:(kind,'' if kind=='gopher' else '/') in hits)
            wait(lambda:source in str(window.get_wm_name()));check(True,kind+' source document loads');time.sleep(.4)
            g=window.get_geometry()
            # First link in the native monospace smallnet presentation. Both
            # gopher's list indentation and gemtext place its text here.
            x=g.x+g.width//2-220;y=g.y+(158 if wl else 188)
            xtest.fake_input(xd,X.MotionNotify,x=x,y=y);xtest.fake_input(xd,X.ButtonPress,3);xtest.fake_input(xd,X.ButtonRelease,3);xd.sync()
            if wl:wait(lambda:wl.scene()['popups'])
            else:wait(lambda:any(w.get_attributes().map_state==X.IsViewable and w.get_attributes().override_redirect for w in windows()))
            time.sleep(.15)
            root=xd.screen().root;size=root.get_geometry();image=root.get_image(0,0,size.width,size.height,X.ZPixmap,0xffffffff)
            Image.frombytes('RGB',(size.width,size.height),image.data,'raw','BGRX').save(out/(kind+'-menu.png'))
            key('Down')
            for _ in range(32):key('Up')
            key('Return')
            wait(lambda:(kind,'/destination') in hits)
            check(True,kind+' context menu retrieves the link in a new view')
            wait(lambda:target in str(window.get_wm_name()))
            key('w',True);wait(lambda:source in str(window.get_wm_name()))
            check(True,kind+' closing the new view restores its original document')
        if wl:wl.api('/api/action',{'op':'close','id':window.id})
        else:window.send_event(protocol.event.ClientMessage(window=window,client_type=xd.intern_atom('WM_PROTOCOLS'),data=(32,[xd.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])));xd.sync()
        check(app.wait(timeout=15)==0,'Browser closes cleanly after native view operations')
        session=json.loads((profile/'session.json').read_text());check(len(session['tabs'])==1,'New views close without duplicating or replacing the source tab')
except BaseException as e:failure=str(e);raise
finally:
    (out/'report.json').write_text(json.dumps({'passed':failure is None and bool(checks),'failure':failure,'checks':checks,'requests':hits,'wayland':a.wayland,'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'binary_sha256':hashlib.file_digest(a.binary.resolve().open('rb'),'sha256').hexdigest(),'scope':'Private profile, bus and display with loopback smallnet servers; smallnet sandbox remains enabled'},indent=2)+'\n')
