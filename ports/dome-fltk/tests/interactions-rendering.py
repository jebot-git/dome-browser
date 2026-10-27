#!/usr/bin/env python3
"""Private native Wayland regression for textarea handles, fixed-panel dragging, images and SVG.
Run: uv run --no-project --with python-xlib --with pillow python ports/dome-fltk/tests/interactions-rendering.py
Requires Xvfb and the adjacent DomeD release compositor. Native media playback is excluded.
"""
import http.server,json,mimetypes,os,re,signal,socket,subprocess,sys,tempfile,threading,time,urllib.request
from pathlib import Path
from PIL import Image
from Xlib import X,display
from Xlib.ext import xtest
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_RENDERING_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_RENDERING_TEST_BUS':'1'})
source=Path(__file__).resolve().parents[3]
artifacts=Path(os.environ.get('DOME_BROWSER_TEST_OUTPUT',source.parent/'artifacts/browser-interactions-rendering'));artifacts.mkdir(parents=True,exist_ok=True)
checks=[];records=[];children=[];logs=[];xd=None
http_client=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def wait(fn,timeout=20):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        try:
            value=fn()
            if value:return value
        except (OSError,ValueError,KeyError):pass
        time.sleep(.05)
    raise AssertionError('Timed out; see '+str(artifacts))
def check(ok,name):
    checks.append({'check':name,'passed':bool(ok)});print(('PASS ' if ok else 'FAIL ')+name,flush=True);assert ok,name
def start(args,name,env,**kwargs):
    log=(artifacts/(name+'.log')).open('w');logs.append(log)
    child=subprocess.Popen(args,env=env,stdout=log,stderr=log,**kwargs);children.append(child);return child
try:
 with tempfile.TemporaryDirectory(prefix='dome-interactions-rendering-') as directory:
    runtime=Path(directory);runtime.chmod(0o700);media=runtime/'media';media.mkdir()
    Image.new('RGB',(80,60),(220,40,180)).save(media/'static.webp',lossless=True)
    frames=[Image.new('RGB',(80,60),color) for color in [(240,20,30),(20,220,30),(20,30,240)]]
    frames[0].save(media/'animated.webp',save_all=True,append_images=frames[1:],duration=300,loop=0,lossless=True)
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):
            records.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))));self.send_response(200);self.end_headers()
        def do_GET(self):
            if self.path=='/frame':
                data=b"<body style='margin:0;height:1000px;background:#bdf'>Underlying cross-origin iframe<script>setInterval(()=>fetch('/report',{method:'POST',body:JSON.stringify({kind:'iframe-state',y:scrollY})}),250);document.addEventListener('mousedown',e=>fetch('/report',{method:'POST',body:JSON.stringify({kind:'iframe-mousedown',x:e.clientX,y:e.clientY})}))</script>"
                self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(data);return
            path=media/self.path.lstrip('/')
            if path.is_file():data=path.read_bytes();kind=mimetypes.guess_type(str(path))[0]
            else:data=Path(__file__).with_name('interaction-rendering.html').read_text().replace('FRAME_PORT',str(self.server.server_port)).encode();kind='text/html; charset=utf-8'
            self.send_response(200);self.send_header('Content-Type',kind or 'application/octet-stream');self.end_headers();self.wfile.write(data)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
    env={**os.environ,'XDG_RUNTIME_DIR':directory,'XDG_CONFIG_HOME':str(runtime/'config'),'XDG_DATA_HOME':str(runtime/'data'),'XDG_STATE_HOME':str(runtime/'state'),'XDG_CACHE_HOME':str(runtime/'cache'),
         'XDG_CURRENT_DESKTOP':'Dome','LIBGL_ALWAYS_SOFTWARE':'1','RUST_LOG':'warn','XMODIFIERS':'@im=none','DOME_LOGIN_SESSION':'0','DOME_START_XR':'0','DOME_START_TRAY':'0','DOME_START_KDECONNECT':'0','DOME_SESSION_RESTORE':'0','DOME_RENDERER':'gles'}
    for key in ['DISPLAY','WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(key,None)
    rd,wr=os.pipe();start(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'],'xvfb',env,pass_fds=(wr,));os.close(wr)
    with os.fdopen(rd) as pipe:env['DISPLAY']=':'+pipe.readline().strip()
    with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    base='http://127.0.0.1:'+str(port);env['DOME_PREVIEW_PORT']=str(port)
    def api(path,data=None):
        with http_client.open(urllib.request.Request(base+path,data=json.dumps(data).encode() if data else None,headers={'Content-Type':'application/json'}),timeout=2) as response:return json.load(response)
    start([os.environ.get('DOME_COMPOSITOR',str(source.parent/'DomeD/target/release/dome-compositor')),'--backend','nested'],'compositor',env)
    wait(lambda:api('/api/state'))
    env['WAYLAND_DISPLAY']=re.search(r'Dome Wayland socket: (\S+)',(artifacts/'compositor.log').read_text())[1];env['FLTK_BACKEND']='wayland'
    start([os.environ.get('DOME_BROWSER_BINARY',str(source/'target/release/dome-browser')),'--no-restore','--profile',str(runtime/'profile'),'http://127.0.0.1:'+str(server.server_port)+'/'],'browser',env)
    browser=wait(lambda:next((w for w in api('/api/state')['scene']['windows'] if w['app_id']=='org.domed.Browser'),None))
    def state():return next((v for v in reversed(records) if v['kind']=='state'),None)
    api('/api/action',{'op':'desktop_resize','id':browser['id'],'w':940,'h':680});wait(lambda:state() and state()['w']==940 and state()['sw']==80 and state()['aw']==80)
    metrics=state()['screen']
    check(metrics[:2]==[1280,800] and all(v>0 for v in metrics[2:]) and state()['outer']==[940,680],'Screen and outer-window APIs expose the native display and window dimensions')
    xd=display.Display(env['DISPLAY']);host=wait(lambda:next((w for w in xd.screen().root.query_tree().children if str(w.get_wm_name() or '').startswith('Dome')),None))
    host.set_input_focus(X.RevertToParent,X.CurrentTime);xd.sync()
    def position(x,y):return (int(browser['desktop']['x']+x),int(browser['desktop']['y']+680-state()['h']-28+y))
    def move(x,y):
        px,py=position(x,y);xtest.fake_input(xd,X.MotionNotify,x=px,y=py);xd.sync();time.sleep(.1)
    def button(down):xtest.fake_input(xd,X.ButtonPress if down else X.ButtonRelease,1);xd.sync();time.sleep(.12)
    def resize_handle_pixels(x,y):
        px,py=position(x-10,y-10)
        raw=host.get_image(px,py,10,10,X.ZPixmap,0xffffffff)
        image=Image.frombytes('RGB',(10,10),raw.data,'raw','BGRX')
        return sum(n for n,c in image.getcolors(100) if max(abs(v-136) for v in c)<5)
    def drag(x,y,dx,dy,resize=False):
        move(x,y)
        if resize:
            before=resize_handle_pixels(x,y)
            wait(lambda:resize_handle_pixels(x,y)>=3)
            records.append({'kind':'native-resize-ready','x':x,'y':y,'initial_pixels':before,'ready_pixels':resize_handle_pixels(x,y)})
        button(True)
        for i in range(1,11):move(x+dx*i/10,y+dy*i/10)
        button(False);time.sleep(.35)
    if '--probe-resize' in sys.argv:
        move(218,108);time.sleep(1)
        (artifacts/'resize-probe.json').write_text(json.dumps(state(),indent=2)+'\n')
        g=host.get_geometry();raw=host.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
        Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX').save(artifacts/'resize-probe.png')
        print(json.dumps(state(),indent=2),flush=True)
        raise SystemExit(0)
    drag(218,108,80,70,resize=True)
    check(wait(lambda:state()['bw']>250 and state()['bh']>130),'Textarea corner handle resizes both dimensions using native Wayland input')
    locked_size=(state()['nw'],state()['nh']);drag(778,88,60,40)
    check((state()['nw'],state()['nh'])==locked_size,'Inline resize:none disables the compatibility handle')
    drag(400,90,100,70)
    check(wait(lambda:state()['px']==420 and state()['py']==150),'A fixed panel above a cross-origin iframe receives drag input instead of the iframe')
    previous=sum(v['kind']=='iframe-mousedown' for v in records)
    move(750,120);button(True);button(False)
    check(wait(lambda:sum(v['kind']=='iframe-mousedown' for v in records)>previous),'Unobscured cross-origin iframe still receives its own mouse input')
    for i in range(2):
        xtest.fake_input(xd,X.ButtonPress,5);xtest.fake_input(xd,X.ButtonRelease,5);xd.sync();time.sleep(.12)
    check(wait(lambda:any(v['kind']=='iframe-state' and v['y']>0 for v in records)) and state()['y']==0,'Wheel scrolling targets the unobscured iframe without moving the outer document')
    move(880,350)
    for i in range(4):
        xtest.fake_input(xd,X.ButtonPress,5);xtest.fake_input(xd,X.ButtonRelease,5);xd.sync();time.sleep(.12)
    wait(lambda:state()['y']>0);drag(470,162,-80,40)
    check(wait(lambda:state()['px']==340 and state()['py']==190),'Fixed-panel dragging remains correct after the outer document scrolls')
    drag(548,298,70,40,resize=True)
    check(wait(lambda:state()['pbw']>250 and state()['pbh']>95),'Textarea resizing works inside a fixed panel after outer-page scrolling')
    webp=set();static=set()
    for n in range(16):
        g=host.get_geometry();raw=host.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
        frame=Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
        webp.add(frame.getpixel(position(180,state()['h']-100)));static.add(frame.getpixel(position(60,state()['h']-100)))
        if n in [0,5,10]:frame.save(artifacts/('frame-'+str(n)+'.png'))
        time.sleep(.17)
    check(len(webp)>=3,'Animated WebP visibly presents all three decoded frames')
    check(all(max(abs(a-b) for a,b in zip(color,(220,40,180)))<5 for color in static),'Static WebP renders its expected RGB pixels')
    g=host.get_geometry();raw=host.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    frame=Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
    check(frame.getpixel(position(830,300))==(20,220,80),'Inline SVG inherits stylesheet fill/currentColor from the surrounding page')
    check(frame.getpixel(position(875,300))==(230,40,70),'Inline SVG descendant paint rules retain their stylesheet colors')
    gradient=frame.getpixel(position(910,300))
    check(gradient[0]>50 and gradient[1]<5 and gradient[2]>50,'Inline SVG local gradient references survive paint-style preservation')

finally:
    if xd is not None:xd.close()
    for child in reversed(children):
        if child.poll() is None:child.terminate()
    for child in reversed(children):
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
    for log in logs:log.close()
    (artifacts/'report.json').write_text(json.dumps({'mode':'resize-probe'if'--probe-resize'in sys.argv else'rendering','checks':checks,'records':records},indent=2)+'\n')
