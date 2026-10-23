#!/usr/bin/env python3
"""Exercise native save actions, browser cookies, XDG paths and bitmap clipboard."""
import contextlib, http.server, io, os, signal, subprocess, sys, tempfile, threading, time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_DOWNLOAD_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
              {**os.environ, 'DOME_DOWNLOAD_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest
from PIL import Image
source = Path(__file__).resolve().parents[3]
artifacts = source.parent/'artifacts/browser-download-tests'
artifacts.mkdir(parents=True, exist_ok=True)
image_bytes = io.BytesIO()
Image.new('RGB', (7, 5), (20, 100, 220)).save(image_bytes, format='PNG')
png = image_bytes.getvalue()
hits = []
class Server(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        hits.append((self.path, self.headers.get('Cookie', '')))
        data, kind, disposition = b'', 'text/html', None
        status = 200
        if self.path == '/':
            data = b'''<title>Dome Download Fixture</title><style>body{margin:20px}a,img{display:block;margin-bottom:25px}img{width:70px;height:50px}</style>
            <a href="/file">Download fixture</a><img src="/image.png"><a href="/missing">Missing file</a>
            <script>addEventListener('load',()=>fetch('/ready?crypto='+!!globalThis.crypto+'&observer='+!!globalThis.IntersectionObserver+'&sheets='+('adoptedStyleSheets' in document)))</script>'''
        elif self.path == '/file':
            status = 200 if 'fixture=secret' in self.headers.get('Cookie', '') else 403
            data, kind = b'authenticated file\x00\xff\n', 'application/octet-stream'
            disposition = "attachment; filename*=UTF-8''report%20test.bin"
        elif self.path == '/image.png': data, kind = png, 'image/png'
        elif self.path.startswith('/ready?'): data = b'ready'
        else: status, data = 404, b'missing'
        self.send_response(status)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(data)))
        if self.path == '/': self.send_header('Set-Cookie', 'fixture=secret; Path=/; HttpOnly; SameSite=Lax')
        if disposition: self.send_header('Content-Disposition', disposition)
        self.end_headers()
        self.wfile.write(data)
    def log_message(self, *args): pass

def wait(fn, seconds=45):
    end = time.monotonic()+seconds
    while time.monotonic() < end:
        result = fn()
        if result: return result
        time.sleep(.05)
    raise AssertionError('Timed out: '+repr(fn))
def check(ok, text):
    assert ok, text
    print('PASS', text, flush=True)
with tempfile.TemporaryDirectory(prefix='dome-download-') as temporary:
    root = Path(temporary)
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0),Server)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    rd, wr = os.pipe()
    xvfb = subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1400x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL)
    os.close(wr)
    with os.fdopen(rd) as stream: screen = ':'+stream.readline().strip()
    x = display.Display(screen)
    config = root/'config'
    config.mkdir()
    downloads = root/'Custom Downloads'
    (config/'user-dirs.dirs').write_text(f'XDG_DOWNLOAD_DIR="{downloads}"\n')
    env = {**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1',
           'XDG_CONFIG_HOME':str(config),'XDG_DATA_HOME':str(root/'data'), 'DOME_BROWSER_TRACE':'1'}
    env.pop('WAYLAND_DISPLAY',None)
    env.pop('WAYLAND_SOCKET',None)
    log = (artifacts/'browser.log').open('w')
    binary = Path(os.environ.get('DOME_BROWSER_BINARY', source/'target/debug/dome-browser'))
    app = subprocess.Popen([str(binary),'--software','--profile',str(root/'profile'),f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log)
    def windows(): return x.screen().root.query_tree().children
    def key(name, control=False):
        code = x.keysym_to_keycode(XK.string_to_keysym(name))
        ctrl = x.keysym_to_keycode(XK.string_to_keysym('Control_L'))
        if control: xtest.fake_input(x,X.KeyPress,ctrl)
        xtest.fake_input(x,X.KeyPress,code);xtest.fake_input(x,X.KeyRelease,code)
        if control: xtest.fake_input(x,X.KeyRelease,ctrl)
        x.sync();time.sleep(.15)
    def context(px,py,up=0):
        g=main.get_geometry()
        xtest.fake_input(x,X.MotionNotify,x=g.x+px,y=g.y+py)
        xtest.fake_input(x,X.ButtonPress,3);xtest.fake_input(x,X.ButtonRelease,3);x.sync();time.sleep(.5)
        popup = wait(lambda: next((w for w in windows() if w.id != main.id and w.get_attributes().map_state == X.IsViewable and w.get_geometry().width > 100 and w.get_geometry().height > 60), None))
        g = popup.get_geometry()
        raw = x.screen().root.get_image(0,0,1400,1000,X.ZPixmap,0xffffffff)
        Image.frombytes('RGB',(1400,1000),raw.data,'raw','BGRX').save(artifacts/'context-menu.png')
        print('POPUP',g.x,g.y,g.width,g.height,flush=True)
        # FLTK menu rows use the configured UI font; click the final row.
        xtest.fake_input(x,X.MotionNotify,x=g.x+g.width//2,y=g.y+g.height-12-up*((g.height-3)//6))
        xtest.fake_input(x,X.ButtonPress,1);xtest.fake_input(x,X.ButtonRelease,1)
        x.sync();time.sleep(.3)
    def clipboard(target):
        owner = x.screen().root.create_window(0,0,1,1,0,X.CopyFromParent,event_mask=X.PropertyChangeMask)
        prop = x.intern_atom('DOME_TEST_CLIP')
        owner.convert_selection(x.intern_atom('CLIPBOARD'),x.intern_atom(target),prop,X.CurrentTime)
        x.flush()
        try:
            def receive():
                while x.pending_events():
                    event = x.next_event()
                    if event.type == X.SelectionNotify:
                        if not event.property: raise AssertionError('Clipboard target unavailable: '+target)
                        return owner.get_full_property(prop,X.AnyPropertyType)
            result = wait(receive)
            return result
        finally: owner.destroy();x.flush()
    try:
        main=wait(lambda:next((w for w in windows() if 'Dome Download Fixture' in str(w.get_wm_name() or '')),None))
        main.set_input_focus(X.RevertToParent,X.CurrentTime);x.sync()
        wait(lambda:any(p.startswith('/ready?') for p,_ in hits))
        check(any(p=='/ready?crypto=true&observer=true&sheets=true' for p,_ in hits),'GitHub-required browser APIs are exposed')
        time.sleep(.7)
        # Native chrome reserves 120 pixels with the local menu visible.
        context(90,147)
        saved=downloads/'report test.bin'
        wait(lambda:saved.exists() and saved.stat().st_size>0)
        check(saved.read_bytes()==b'authenticated file\x00\xff\n','Save Link preserves binary bytes and uses browser HttpOnly cookies')
        context(90,147)
        wait(lambda:(downloads/'report test (1).bin').exists())
        check(saved.read_bytes()==b'authenticated file\x00\xff\n','Repeated saves preserve the existing file')
        context(45,207,up=1)
        wait(lambda:(downloads/'image.png').exists())
        check((downloads/'image.png').read_bytes()==png,'Save Image preserves original encoded image')
        context(45,207)
        time.sleep(.5)
        targets=clipboard('TARGETS')
        names=[x.get_atom_name(int(atom)) for atom in targets.value]
        target=next((t for t in ['image/png','image/bmp','image/x-bmp'] if t in names),None)
        check(target is not None,'Copy Image offers an actual image clipboard MIME type')
        copied=Image.open(io.BytesIO(bytes(clipboard(target).value))).convert('RGB')
        check(copied.size==(7,5) and copied.getpixel((3,2))==(20,100,220),'Clipboard preserves original image dimensions and pixels')
        key('s',control=True)
        wait(lambda:(downloads/'page.html').exists())
        check(b'Dome Download Fixture' in (downloads/'page.html').read_bytes(),'Ctrl+S saves the page source in the XDG Downloads directory')
        before=set(downloads.iterdir())
        context(80,270)
        wait(lambda:any(p=='/missing' for p,_ in hits))
        time.sleep(.5)
        check(set(downloads.iterdir())==before,'HTTP errors do not leave a downloaded file')
        print('All download and clipboard checks passed',flush=True)
    finally:
        app.terminate()
        with contextlib.suppress(subprocess.TimeoutExpired):app.wait(timeout=10)
        if app.poll() is None:app.kill();app.wait()
        xvfb.terminate();xvfb.wait(timeout=10)
        server.shutdown();log.close()
