#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Native Wayland input beneath the scroll cue and rendered fade acceptance."""
import argparse, hashlib, json, os, queue, subprocess, sys, tempfile, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from PIL import Image, ImageChops, ImageStat
from Xlib import X
from Xlib.ext import xtest
from wayland_touch_fixture import TouchWayland

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,required=True)
p.add_argument('--compositor',type=Path,required=True)
p.add_argument('--output',type=Path,required=True)
p.add_argument('--dark',action='store_true')
a=p.parse_args()
if os.environ.get('DOME_SCROLL_INPUT_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_SCROLL_INPUT_BUS':'1'})
out=a.output.resolve();out.mkdir(exist_ok=False)
def sha(path):
    with path.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
binary_hash=sha(a.binary.resolve());phases=queue.Queue();events=[];checks=[];error=None
script='''const wait=ms=>new Promise(r=>setTimeout(r,ms));
const settle=async()=>{await new Promise(requestAnimationFrame);await new Promise(requestAnimationFrame);await wait(80)};
const send=async name=>{await fetch('/phase',{method:'POST',body:JSON.stringify({name,width:innerWidth,visible:innerHeight,total:document.scrollingElement.scrollHeight,y:scrollY,dpr:devicePixelRatio})})};
for(const name of ['pointerdown','pointermove','pointerup','pointercancel','click','wheel'])document.addEventListener(name,e=>{
fetch('/event',{method:'POST',body:JSON.stringify({name,type:e.pointerType||'',target:e.target.id,trusted:e.isTrusted,x:e.clientX,y:e.clientY,scroll:scrollY})});},{passive:true});
(async()=>{await wait(600);await send('baseline');
let position=1600;for(const name of ['mouse-click','wheel','touch-tap','touch-drag','fade']){scrollTo(0,position+=200);await settle();await send(name);await wait(1100);}
await send('done');})();'''
class Site(BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_GET(self):
        bg='#111' if a.dark else '#fff'
        body=(f'<!doctype html><meta charset="utf-8"><title>Scroll cue input fixture</title><style>html,body{{margin:0;background:{bg}}}body{{height:5000px}}#edge{{position:fixed;right:0;top:0;bottom:0;width:40px}}</style><div id="edge"></div><script>'+script+'</script>').encode()
        self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    def do_POST(self):
        data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if self.path=='/event':events.append(data)
        else:
            ack=threading.Event();phases.put((data,ack));ack.wait(20)
        self.send_response(204);self.end_headers()
def check(value,name,**details):
    row={'name':name,'passed':bool(value),**details};checks.append(row);print(json.dumps(row),flush=True);assert value,row
def wait(fn,seconds=3):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        assert app.poll() is None,'Browser exited unexpectedly'
        value=fn()
        if value:return value
        time.sleep(.015)
    raise AssertionError('Timed out waiting for DOM input')
app=fixture=server=log=None
try:
    with tempfile.TemporaryDirectory(prefix='dome-scroll-input-') as tmp:
        runtime=Path(tmp);runtime.chmod(0o700)
        fixture=TouchWayland(a.compositor.resolve(),runtime,out)
        server=ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=server.serve_forever,daemon=True).start()
        env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')}
        env.update(HOME=tmp,XDG_RUNTIME_DIR=tmp,XDG_CONFIG_HOME=tmp+'/config',XDG_DATA_HOME=tmp+'/data',XDG_STATE_HOME=tmp+'/state',XDG_CACHE_HOME=tmp+'/cache',FLTK_BACKEND='wayland',WAYLAND_DISPLAY=fixture.wayland.socket,LIBGL_ALWAYS_SOFTWARE='1',DOME_WEBXR_SOCKET=tmp+'/no-host.sock',RUST_LOG='warn')
        for name in ['DISPLAY','WAYLAND_SOCKET','FLTK_SCALING_FACTOR']:env.pop(name,None)
        log=(out/'browser.log').open('w');app=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',tmp+'/profile',f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log)
        baseline=None
        while True:
            data,ack=phases.get(timeout=20)
            try:
                window=fixture.wayland.wait(lambda:next((w for w in fixture.wayland.windows() if w.value()['client_pid']==app.pid),None))
                g=window.get_geometry()
                # Establish 1:1 private surface geometry before any input evidence.
                check(abs(g.width/data['width']-data['dpr'])<.08,'Native surface matches page pixels: '+data['name'],surface_width=g.width,page_width=data['width'],dpr=data['dpr'])
                top=round(g.height-28-data['visible']*data['dpr'])
                def capture():
                    raw=window.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
                    return Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
                def crop(image):return image.crop((g.width-12,top,g.width,g.height-28))
                if data['name']=='baseline':
                    baseline=crop(capture());continue
                if data['name']=='done':break
                # Never introduce keyboard-focus latency while the transient cue is visible.
                host=fixture.wayland.host.get_geometry()
                xtest.fake_input(fixture.connection,X.MotionNotify,x=host.x+g.x+g.width//2,y=host.y+g.y+top+20);fixture.connection.sync();time.sleep(.03)
                image=capture();edge=crop(image);box=ImageChops.difference(baseline,edge).getbbox()
                image.save(out/(data['name']+'.png'))
                check(box is not None,'Cue visible before '+data['name'])
                track=edge.height-12;length=max(18,track*data['visible']/data['total']);expected=6+(track-length)*data['y']/(data['total']-data['visible'])
                check(abs(box[1]-expected)<4,'Cue follows document metrics before '+data['name'],box=box,expected=expected)
                px=g.width-7;py=top+int((box[1]+box[3])/2)
                check(g.width-12+box[0]<=px<g.width-12+box[2] and box[1]+top<=py<box[3]+top,'Input point lies inside painted cue: '+data['name'],point=[px,py])
                start=len(events)
                def selected(name,kind=None):return [e for e in events[start:] if e['name']==name and (kind is None or e['type']==kind)]
                host=fixture.wayland.host.get_geometry()
                def mouse(kind):
                    xtest.fake_input(fixture.connection,X.MotionNotify,x=host.x+g.x+px,y=host.y+g.y+py)
                    xtest.fake_input(fixture.connection,X.ButtonPress,kind);xtest.fake_input(fixture.connection,X.ButtonRelease,kind);fixture.connection.sync()
                def touch(action,point=None):
                    fixture.command(action,1,None if point is None else [g.x+point[0],g.y+point[1]]);fixture.command('frame')
                if data['name']=='mouse-click':
                    mouse(1);wait(lambda:selected('click'));row=selected('click')[-1]
                    check(row['trusted'] and row['target']=='edge','Trusted click reaches underlying DOM edge',event=row)
                elif data['name']=='wheel':
                    mouse(5);wait(lambda:selected('wheel'));row=selected('wheel')[-1]
                    check(row['trusted'] and row['target']=='edge','Trusted wheel reaches underlying DOM edge',event=row)
                    time.sleep(.2);mouse(1);wait(lambda:selected('click'))
                    check(selected('click')[-1]['scroll']>data['y'],'Wheel beneath cue scrolls root document')
                elif data['name'] in ['touch-tap','touch-drag']:
                    touch('down',[px,py]);wait(lambda:selected('pointerdown','touch'))
                    row=selected('pointerdown','touch')[-1]
                    check(row['trusted'] and row['target']=='edge','Trusted touch reaches underlying DOM edge: '+data['name'],event=row)
                    if data['name']=='touch-drag':
                        for delta in [15,35,65,100]:touch('move',[px,py-delta]);time.sleep(.03)
                    touch('up');wait(lambda:selected('pointerup','touch') or selected('pointercancel','touch'))
                    if data['name']=='touch-tap':
                        wait(lambda:selected('click'));check(selected('click')[-1]['trusted'] and selected('click')[-1]['target']=='edge','Native tap beneath cue retains compatibility click')
                    else:
                        time.sleep(.2);mouse(1);wait(lambda:selected('click'))
                        check(selected('click')[-1]['scroll']>data['y']+20,'Native touch drag beneath cue scrolls document',scroll=selected('click')[-1]['scroll'])
                else:
                    begun=time.monotonic();trace=[]
                    for delay in [0,.25,.42,.50,.58,.66,.74,.90,1.15]:
                        remaining=delay-(time.monotonic()-begun)
                        if remaining>0:time.sleep(remaining)
                        edge=crop(capture());diff=ImageChops.difference(baseline,edge);patch=diff.crop(box)
                        energy=sum(ImageStat.Stat(patch).mean)/3
                        trace.append({'elapsed':time.monotonic()-begun,'contrast':energy})
                    (out/'fade-trace.json').write_text(json.dumps(trace,indent=2)+'\n')
                    initial=trace[0]['contrast']
                    check(initial>10,'Cue has visible contrast',contrast=initial)
                    check(any(initial*.1<r['contrast']<initial*.9 for r in trace),'Rendered cue has intermediate fade opacity',trace=trace)
                    check(trace[-1]['contrast']<1 and trace[-2]['contrast']<1,'Cue disappears and stays absent after motion stops',trace=trace[-2:])
                    check(all(b['contrast']<=a['contrast']+3 for a,b in zip(trace,trace[1:])),'Rendered fade does not brighten without motion')
            finally:ack.set()
        fixture.wayland.api('/api/action',{'op':'close','id':window.id});app.wait(timeout=10)
        check(app.returncode==0,'Browser closes cleanly',exit_code=app.returncode)
        check(sha(a.binary.resolve())==binary_hash,'Candidate unchanged during validation')
except BaseException as ex:
    error=repr(ex);raise
finally:
    if app and app.poll() is None:app.terminate();app.wait(timeout=8)
    if server:server.shutdown();server.server_close()
    if log:log.close()
    if fixture:fixture.close()
    (out/'events.json').write_text(json.dumps(events,indent=2)+'\n')
    (out/'report.json').write_text(json.dumps({'passed':error is None and bool(checks),'error':error,'checks':checks,'binary_sha256':binary_hash,'script_sha256':sha(Path(__file__)),'dark_page':a.dark,'backend':'wayland','scope':'Private compositor, shipping FLTK and Servo input routes, trusted native mouse/wheel/touch beneath transient overlay and screenshot fade','physical_device_claim':False},indent=2)+'\n')
