#!/usr/bin/env python3
"""Drive the real browser against an isolated HTTP, notification and DomeD XR fixture."""
import asyncio
import contextlib
import http.server
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import socketserver
import struct
import subprocess
import sys
import tempfile
import threading
import time

signal.pthread_sigmask(signal.SIG_SETMASK, [])
port = Path(__file__).resolve().parents[1]
source = port.parents[1]
artifacts = source.parent / 'artifacts/servo-browser-tests'
artifacts.mkdir(exist_ok=True)

if '--services' in sys.argv:
    from dbus_next.aio import MessageBus
    from dbus_next.service import ServiceInterface, method

    class Notifications(ServiceInterface):
        def __init__(self):
            super().__init__('org.freedesktop.Notifications')

        @method()
        def Notify(self, app: 's', replaces: 'u', icon: 's', title: 's', body: 's', actions: 'as', hints: 'a{sv}', timeout: 'i') -> 'u':
            with Path(sys.argv[-1]).open('a') as f:
                f.write(json.dumps({'app': app, 'title': title, 'body': body, 'actions': actions}) + '\n')
            return 1

    async def services():
        bus = await MessageBus().connect()
        bus.export('/org/freedesktop/Notifications', Notifications())
        await bus.request_name('org.freedesktop.Notifications')
        await bus.request_name('com.canonical.AppMenu.Registrar')
        print('ready', flush=True)
        await asyncio.Future()

    asyncio.run(services())
    raise SystemExit()

if os.environ.get('DOME_BROWSER_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv], {**os.environ, 'DOME_BROWSER_TEST_BUS': '1'})

from Xlib import X, XK, display
from Xlib.ext import xtest
from PIL import Image

checks = []
events = []
def check(value, name):
    assert value, name
    checks.append(name)
    print('PASS', name, flush=True)

def wait(callback, seconds=45):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        value = callback()
        if value:
            return value
        time.sleep(.06)
    raise AssertionError('Timed out waiting for ' + repr(callback))

def received(name):
    return next((e for e in reversed(events) if e['name'] == name), None)

class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        page = (port / 'tests/page.html').read_bytes() if self.path == '/' else b'<title>Second page</title><h1>Second page</h1>'
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(page)))
        self.end_headers()
        self.wfile.write(page)

    def do_POST(self):
        events.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
        self.send_response(204)
        self.end_headers()

    def log_message(self, *args):
        pass

class XRServer(socketserver.ThreadingUnixStreamServer):
    daemon_threads = True
    def __init__(self, path):
        super().__init__(path, XRRequest)
        self.token = None
        self.begins = 0
        self.ends = 0
        self.frames = []
        self.frame_id = 0
        self.errors = []

class XRRequest(socketserver.StreamRequestHandler):
    def handle(self):
        try:
            size = struct.unpack('<I', self.rfile.read(4))[0]
            assert 0 < size <= 65536
            request = json.loads(self.rfile.read(size))
            op = request['op']
            s = self.server
            if op == 'hello':
                result = {'ok': True, 'version': 1, 'name': 'Isolated DomeD protocol fixture', 'width': 128, 'height': 96, 'view_sizes': [[64,96],[64,96]], 'max_width': 4096, 'max_height': 4096, 'stage': True, 'period': 13888888, 'blend_modes': ['opaque']}
            elif op == 'begin_session':
                if s.token:
                    result = {'ok': False, 'error': 'busy'}
                else:
                    s.token = secrets.token_hex(24)
                    s.begins += 1
                    result = {'ok': True, 'token': s.token}
            else:
                assert s.token and secrets.compare_digest(request['token'], s.token)
                if op == 'wait_frame':
                    time.sleep(.014)
                    s.frame_id = max(s.frame_id + 1, request.get('after', 0) + 1)
                    pose = lambda x: {'position': [x,1.6,2], 'orientation': [0,0,0,1]}
                    result = {'ok': True, 'frame_id': s.frame_id, 'time': time.monotonic_ns(), 'period': 13888888, 'should_render': True, 'visible': True, 'focused': True, 'head_flags': 15, 'view_flags': 15, 'head': pose(1), 'views': [{'pose': pose(x), 'fov': {'left': -.7, 'right': .7, 'up': .7, 'down': -.7}} for x in [.968,1.032]]}
                elif op == 'end_frame':
                    if not request.get('empty'):
                        assert request['transport'] == 'rgba'
                        width, height = request['width'], request['height']
                        assert 0 < width <= 4096 and 0 < height <= 4096
                        assert request['bytes'] == width * height * 4
                        rgba = self.rfile.read(request['bytes'])
                        assert len(rgba) == request['bytes']
                        left = tuple(rgba[((height//2)*width+width//4)*4:][:4])
                        right = tuple(rgba[((height//2)*width+width*3//4)*4:][:4])
                        s.frames.append({'request': request, 'left': left, 'right': right})
                    result = {'ok': True}
                elif op == 'end_session':
                    s.token = None
                    s.ends += 1
                    result = {'ok': True}
                else:
                    raise AssertionError(op)
            response = json.dumps(result).encode()
            self.wfile.write(struct.pack('<I', len(response)) + response)
        except Exception as error:
            self.server.errors.append(repr(error))

processes = []
with tempfile.TemporaryDirectory(prefix='servo-browser-test-') as temporary:
    root = Path(temporary)
    http = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Page)
    threading.Thread(target=http.serve_forever, daemon=True).start()
    xr = XRServer(str(root/'xr.sock'))
    os.chmod(root/'xr.sock', 0o600)
    threading.Thread(target=xr.serve_forever, daemon=True).start()
    rd, wr = os.pipe()
    xvfb = subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'], pass_fds=(wr,), stderr=subprocess.DEVNULL)
    processes.append(xvfb)
    os.close(wr)
    with os.fdopen(rd) as stream:
        screen = ':' + stream.readline().strip()
    x = display.Display(screen)
    env = {**os.environ, 'DISPLAY':screen, 'FLTK_BACKEND':'x11', 'XDG_RUNTIME_DIR':str(root), 'XDG_CONFIG_HOME':str(root/'config'), 'XDG_DATA_HOME':str(root/'data'), 'XDG_CACHE_HOME':str(root/'cache'), 'XDG_CURRENT_DESKTOP':'Dome', 'DOME_WEBXR_SOCKET':str(root/'xr.sock'), 'LIBGL_ALWAYS_SOFTWARE':'1', 'XMODIFIERS':'@im=none', 'DOME_BROWSER_TRACE':'1', 'DOME_BROWSER_FRAME_CAPTURE':str(artifacts/'servo-frame.png')}
    for key in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']:
        env.pop(key,None)
    appearance = root/'config/dome/appearance.json'
    appearance.parent.mkdir(parents=True)
    appearance.write_text(json.dumps({'font':'DungGeunMo / Regular','font_size':16,'light':False,'accent':'#6babff'}))
    binary = Path(os.environ.get('DOME_BROWSER_BINARY', source/'target/debug/dome-browser'))
    origin = 'http://127.0.0.1:' + str(http.server_port)
    log = (artifacts/'browser.log').open('w')

    def windows():
        return x.screen().root.query_tree().children
    def window(title):
        return wait(lambda: next((w for w in windows() if title in str(w.get_wm_name() or '')), None))
    def browser_window():
        def mapped():
            candidates=[w for w in windows() if w.get_attributes().map_state==X.IsViewable and w.get_geometry().width>=640]
            return max(candidates,key=lambda w:w.get_geometry().width*w.get_geometry().height) if candidates else None
        return wait(mapped)

    def key(w, name, control=False, alt=False, shift=False, focus=True):
        if focus:w.set_input_focus(X.RevertToParent,X.CurrentTime)
        modifiers = []
        if control: modifiers.append('Control_L')
        if alt: modifiers.append('Alt_L')
        if shift: modifiers.append('Shift_L')
        for m in modifiers: xtest.fake_input(x,X.KeyPress,x.keysym_to_keycode(XK.string_to_keysym(m)))
        code = x.keysym_to_keycode(XK.string_to_keysym(name))
        xtest.fake_input(x,X.KeyPress,code)
        xtest.fake_input(x,X.KeyRelease,code)
        for m in reversed(modifiers): xtest.fake_input(x,X.KeyRelease,x.keysym_to_keycode(XK.string_to_keysym(m)))
        x.sync()
        time.sleep(.15)
    def type_text(text):
        for char in text:
            symbol = ord(char)
            choices = list(x.keysym_to_keycodes(symbol))
            assert choices,char
            code,index = choices[0]
            shift = x.keysym_to_keycode(XK.string_to_keysym('Shift_L'))
            if index % 2: xtest.fake_input(x,X.KeyPress,shift)
            xtest.fake_input(x,X.KeyPress,code);xtest.fake_input(x,X.KeyRelease,code)
            if index % 2: xtest.fake_input(x,X.KeyRelease,shift)
        x.sync()
        time.sleep(.2)
    def click(w,px,py):
        g=w.get_geometry()
        xtest.fake_input(x,X.MotionNotify,x=g.x+px,y=g.y+py)
        xtest.fake_input(x,X.ButtonPress,1);xtest.fake_input(x,X.ButtonRelease,1)
        x.sync();time.sleep(.2)
    def shot(w,name):
        x.sync();time.sleep(.3);g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
        image=Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
        image.save(artifacts/(name+'.png'))
        return image
    def navigate(w,url):
        key(w,'l',control=True);type_text(url);key(w,'Return')
    def menu(w,method,*args):
        service=w.get_full_property(x.intern_atom('_KDE_NET_WM_APPMENU_SERVICE_NAME'),X.AnyPropertyType)
        path=w.get_full_property(x.intern_atom('_KDE_NET_WM_APPMENU_OBJECT_PATH'),X.AnyPropertyType)
        return subprocess.check_output(['gdbus','call','--session','--dest',bytes(service.value).decode(),'--object-path',bytes(path.value).decode(),'--method','com.canonical.dbusmenu.'+method,'--',*args],text=True)
    try:
        app=subprocess.Popen([str(binary),origin+'/'],env=env,stdout=log,stderr=log)
        (artifacts/'pid').write_text(str(app.pid))
        processes.append(app)
        main=window('Servo FLTK integration test')
        wait(lambda:received('READY'))
        check(app.poll() is None,'the actual Servo page loads in native FLTK')
        wait(lambda:received('WEBGL'))
        check(received('READY')['data']['xr'],'navigator.xr is exposed')
        wait(lambda: shot(main,'initial').getpixel((520,240))[0]<100, seconds=15)
        initial=shot(main,'initial')
        check(initial.getpixel((520,240))[0]<100 and initial.getpixel((520,240))[2]>150,'Servo WebGL pixels are visible inside the FLTK page area')
        layout=menu(main,'GetLayout','0','-1','[]')
        check(all(label in layout for label in ['New Tab','Location','Reload','Full Screen']),'the universal menu exports actual browser commands')
        click(main,100,120+90)
        wait(lambda:received('CLICK'))
        check(True,'mouse input reaches page JavaScript')
        click(main,100,120+148);type_text('servo fltk')
        wait(lambda:received('INPUT') and received('INPUT')['data']['value']=='servo fltk')
        check(True,'keyboard input reaches a real HTML text field')
        key(main,'a',control=True);key(main,'x',control=True)
        cut=wait(lambda:next((e for e in events if e['name']=='INPUT' and e['data']['value']==''),None))
        key(main,'v',control=True)
        wait(lambda:any(e['name']=='INPUT' and e['data']['value']=='servo fltk' and e['data']['change']>cut['data']['change'] for e in events))
        check(True,'page cut and paste use the native system clipboard')
        navigate(main,origin+'/second.html');wait(lambda:'Second page' in str(main.get_wm_name()))
        key(main,'Left',alt=True);wait(lambda:'Servo FLTK integration test' in str(main.get_wm_name()))
        check(True,'address navigation and Alt+Left traverse Servo history')
        second=subprocess.run([str(binary),origin+'/second.html'],env=env,capture_output=True,timeout=15)
        check(second.returncode==0,'a second launch forwards its URL to the existing browser')
        wait(lambda:'Second page' in str(main.get_wm_name()))
        key(main,'w',control=True);wait(lambda:'Servo FLTK integration test' in str(main.get_wm_name()))
        check(True,'Ctrl+W closes the new tab and restores the previous page')
        notification_file=root/'notifications.jsonl'
        service=subprocess.Popen([sys.executable,__file__,'--services',str(notification_file)],env=env,stdout=subprocess.PIPE,stderr=log,text=True)
        processes.append(service);assert service.stdout.readline().strip()=='ready'
        time.sleep(.7)
        hosted=shot(main,'universal-menu')
        check(hosted.crop((0,0,500,100)).tobytes()!=initial.crop((0,0,500,100)).tobytes(),'DomeD universal-menu registration removes the local menu bar')
        key(main,'t',control=True);time.sleep(.6);key(main,'w',control=True);wait(lambda:'Servo FLTK integration test' in str(main.get_wm_name()))
        check(app.poll() is None,'browser shortcuts remain active while the local menu is collapsed')
        key(main,'F11');time.sleep(.5);main=browser_window()
        key(main,'F11');time.sleep(.5);main=browser_window()
        wait(lambda:received('RESIZE') and received('RESIZE')['data']['height']==742)
        check(True,'fullscreen transitions leave no empty local-menu strip under the DomeD host')

        appearance.write_text(json.dumps({'font':'DungGeunMo / Regular','font_size':16,'light':True,'accent':'#ba5151'}))
        wait(lambda:received('THEME') and received('THEME')['data']['light'])
        check(True,'live DomeD appearance updates the page color-scheme preference')
        shot(main,'light-appearance')
        click(main,100,90+204)
        consent=window('Site permission');key(consent,'Return')
        wait(lambda:notification_file.exists() and notification_file.stat().st_size)
        notice=json.loads(notification_file.read_text().splitlines()[-1])
        check(notice['app']=='Dome Browser' and notice['title']=='Servo desktop notification','web notifications use the session freedesktop notification server')
        wait(lambda:received('XR_SUPPORTED'))
        check(received('XR_SUPPORTED')['data']['supported'],'WebXR discovers the isolated DomeD bridge')
        click(main,100,90+260);consent=window('Enter immersive WebXR');shot(consent,'xr-consent');key(consent,'Escape')
        wait(lambda:received('XR_DENIED'))
        check(xr.begins==0,'denying native XR consent never acquires a DomeD session lease')
        click(main,100,90+260);consent=window('Enter immersive WebXR');key(consent,'Return')
        wait(lambda:received('XR_ENTERED'))
        wait(lambda:received('XR_POSE'))
        wait(lambda:any(f['left'][0]>220 and f['right'][1]>220 for f in xr.frames))
        pose=received('XR_POSE')['data']
        check(abs(pose['local']['y'])<.01 and abs(pose['floor']['y']-1.6)<.01,'WebXR local and floor spaces use the host tracking origin')
        check(abs(pose['eyes'][0]['x']+.032)<.001 and abs(pose['eyes'][1]['x']-.032)<.001,'both eyes use the actual bridge eye poses')
        check(True,'real Servo WebGL stereo frames reach DomeD through the RGBA bridge')
        shot(main,'webxr-running')
        click(main,100,90+260)
        wait(lambda:xr.ends==1)
        check(xr.token is None,'Exit XR releases only the browser lease')
        click(main,100,90+260);key(window('Enter immersive WebXR'),'Return')
        wait(lambda:xr.begins==2)
        navigate(main,origin+'/second.html')
        wait(lambda:xr.ends==2)
        check(xr.token is None,'navigation releases the active browser XR lease')
        navigate(main,origin+'/');wait(lambda:'Servo FLTK integration test' in str(main.get_wm_name()))
        time.sleep(.8)
        click(main,100,90+260);key(window('Enter immersive WebXR'),'Return')
        wait(lambda:xr.begins==3)
        key(main,'t',control=True);time.sleep(.5);key(main,'w',control=True)
        wait(lambda:'Servo FLTK integration test' in str(main.get_wm_name()))
        service.terminate();service.wait(timeout=5)
        wait(lambda:received('RESIZE') and received('RESIZE')['data']['height']==712)
        check(True,'losing the universal menu host restores the local menu')
        main.configure(width=1080,height=760);x.sync()
        wait(lambda:received('RESIZE')['data']=={'width':1080,'height':612})
        check(True,'native window resizing reaches the Servo viewport')
        key(main,'w',control=True);wait(lambda:app.poll() is not None)
        wait(lambda:xr.ends==3)
        check(xr.token is None,'closing the XR tab and final window releases the browser lease')
        check(app.returncode==0,'the browser and engine shut down cleanly')
        check(not xr.errors,'all XR protocol messages and payloads are valid')
        profile=root/'data/dome-browser'
        saved=json.loads((profile/'session.json').read_text())
        check(len(saved['tabs'])==1 and saved['tabs'][0]['url']==origin+'/', 'closing the last tab saves its page for the next launch')
        session={'version':1,'active':1,'tabs':[{'url':origin+'/','zoom':1.0},{'url':origin+'/second.html','zoom':1.25}]}
        (profile/'session.json').write_text(json.dumps(session))
        service=subprocess.Popen([sys.executable,__file__,'--services',str(notification_file)],env=env,stdout=subprocess.PIPE,stderr=log,text=True)
        processes.append(service);assert service.stdout.readline().strip()=='ready'
        outside={**env,'XDG_CURRENT_DESKTOP':'GNOME'}
        app=subprocess.Popen([str(binary)],env=outside,stdout=log,stderr=log);processes.append(app)
        main=window('Second page')
        wait(lambda:json.loads((profile/'session.json').read_text())==session)
        check(True,'restart restores tab order, active tab and zoom')
        def local_menu_visible(name):
            image=shot(main,name)
            return sum(1 for r,g,b in image.crop((8,7,350,23)).getdata() if min(r,g,b)>180)>80
        check(local_menu_visible('outside-domed-menu'),'outside DomeD the reserved menu row contains visible commands even with an external host')
        key(main,'F11');time.sleep(.5);main=browser_window()
        key(main,'F11');time.sleep(.5);main=browser_window()
        check(local_menu_visible('outside-domed-fullscreen-restored'),'leaving fullscreen restores the populated local menu')
        key(main,'s',control=True,shift=True)
        wait(lambda:(profile/'saved-session.json').exists())
        check(json.loads((profile/'saved-session.json').read_text())==session,'Save Session stores all tabs and their zoom')
        navigate(main,origin+'/');wait(lambda:'Servo FLTK integration test' in str(main.get_wm_name()))
        key(main,'r',control=True,shift=True);wait(lambda:'Second page' in str(main.get_wm_name()))
        check(True,'Restore Saved Session replaces the current tab set with the saved one')
        key(main,'d',control=True)
        def input_dialog():
            return wait(lambda:next((w for w in windows() if w.id!=main.id and str(w.get_wm_name() or '')=='Dome Browser' and w.get_attributes().map_state==X.IsViewable),None))
        dialog=input_dialog();key(dialog,'a',control=True);type_text('Dillo & Servo');key(dialog,'Return')
        dialog=input_dialog();key(dialog,'a',control=True);type_text('Tests & docs');key(dialog,'Return')
        wait(lambda:(profile/'bm.txt').exists())
        bookmarks=(profile/'bm.txt').read_text()
        check(':s1: Tests & docs' in bookmarks and 's1 '+origin+'/second.html Dillo & Servo' in bookmarks,'Ctrl+D writes native bookmarks in Dillo bm.txt syntax')
        navigate(main,origin+'/');wait(lambda:'Servo FLTK integration test' in str(main.get_wm_name()))
        mapped={w.id for w in windows() if w.get_attributes().map_state==X.IsViewable}
        key(main,'o',control=True,shift=True)
        # Keep FLTK's native popup keyboard grab. Refocusing the main window
        # between keys dismisses the menu before Return can select its item.
        def bookmark_popup():
            for candidate in windows():
                if candidate.id in mapped:continue
                attributes=candidate.get_attributes();geometry=candidate.get_geometry()
                if (attributes.map_state==X.IsViewable and attributes.override_redirect
                    and 120<geometry.width<600 and 10<geometry.height<60):return candidate
        wait(bookmark_popup)
        key(main,'Down',focus=False);key(main,'Return',focus=False)
        wait(lambda:'Second page' in str(main.get_wm_name()))
        check(True,'the native bookmark picker opens its bookmarked URL')
        history_file=profile/'hs.txt'
        wait(lambda:history_file.exists() and origin+'/second.html' in history_file.read_text().splitlines())
        check(origin+'/' in history_file.read_text().splitlines(),'hs.txt records visited URLs as plain text')
        navigate(main,origin+'/');wait(lambda:'Servo FLTK integration test' in str(main.get_wm_name()))
        key(main,'l',control=True);type_text(origin+'/sec');key(main,'Return')
        wait(lambda:'Second page' in str(main.get_wm_name()))
        check(True,'the address bar completes a known URL prefix from hs.txt')
        navigate(main,origin+'/');wait(lambda:'Servo FLTK integration test' in str(main.get_wm_name()))
        key(main,'l',control=True);type_text(origin.removeprefix('http://')+'/sec');key(main,'Return')
        wait(lambda:'Second page' in str(main.get_wm_name()))
        check(True,'host-and-port completion preserves the original HTTP scheme')


        wait(lambda:json.loads((profile/'session.json').read_text())['tabs'][1]['url']==origin+'/second.html')
        app.kill();app.wait(timeout=10)
        app=subprocess.Popen([str(binary)],env=outside,stdout=log,stderr=log);processes.append(app)
        main=window('Second page');key(main,'q',control=True);wait(lambda:app.poll() is not None)
        check(app.returncode==0,'autosaved tabs recover after abrupt process termination')
        (profile/'session.json').write_text('{broken')
        app=subprocess.Popen([str(binary)],env=outside,stdout=log,stderr=log);processes.append(app)
        main=window('Dome Browser');time.sleep(2);key(main,'q',control=True);wait(lambda:app.poll() is not None)
        check((profile/'session.json').read_text()=='{broken','corrupt session files are preserved instead of silently overwritten')
        app=subprocess.Popen([str(binary),'--no-restore'],env=outside,stdout=log,stderr=log);processes.append(app)
        main=window('Dome Browser');time.sleep(1);key(main,'q',control=True);wait(lambda:app.poll() is not None)
        check(json.loads((profile/'session.json').read_text())['tabs'][0]['url']=='about:blank','--no-restore starts a fresh session explicitly')

    finally:
        with contextlib.suppress(Exception): shot(main,'final-state')
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:process.kill();process.wait()
        http.shutdown();xr.shutdown();http.server_close();xr.server_close();
        with contextlib.suppress(Exception): x.close()
        log.close()
        (artifacts/'events.json').write_text(json.dumps(events,indent=2)+'\n')
        (artifacts/'xr-frames.json').write_text(json.dumps(xr.frames[-5:],indent=2)+'\n')
        (artifacts/'checks.json').write_text(json.dumps(checks,indent=2)+'\n')
