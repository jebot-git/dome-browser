#!/usr/bin/env python3
"""Probe native input in SVG navigation and a controlled contenteditable editor."""
import contextlib, http.server, json, os, signal, subprocess, sys, tempfile, threading, time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_RICH_TEXT_BUS') != '1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],
              {**os.environ,'DOME_RICH_TEXT_BUS':'1'})
from Xlib import X, XK, display
from Xlib.ext import xtest
port=Path(__file__).resolve().parents[1]
mode='native-svg' if '--native-svg' in sys.argv else 'compat' if '--compat' in sys.argv else 'native'
out=Path(os.environ.get('DOME_BROWSER_TEST_OUTPUT',port.parents[2]/'artifacts/browser-rich-text-tests'/mode));out.mkdir(parents=True,exist_ok=True)
events=[]
page='''<!doctype html><meta charset=utf-8><title>Rich text fixture</title>
<style>body{font:18px sans-serif}#plain{position:absolute;left:24px;top:30px;width:120px;height:50px;background:#aac}
svg{position:absolute;left:24px;top:110px}#server{width:100%;height:100%;background:#aca}
#editor{position:absolute;left:200px;top:200px;width:500px;min-height:60px;border:2px solid blue}
#editor p{margin:0}#ordinary{position:absolute;left:200px;top:110px;width:300px;height:40px}
#basic,#blocked{position:absolute;left:200px;top:300px;width:500px;min-height:50px;border:2px solid green}#blocked{top:400px}</style>
<div id=plain role=button tabindex=0><span>Plain server</span></div>
<svg width=120 height=60><foreignObject width=120 height=60><div xmlns="http://www.w3.org/1999/xhtml" id=server role=treeitem tabindex=-1><span>SVG server</span></div></foreignObject></svg>
<input id=ordinary placeholder="Ordinary input">
<div id=editor contenteditable=true role=textbox data-slate-editor=true data-slate-node=value tabindex=0><p data-slate-node=element><span data-slate-node=text><span data-slate-leaf=true><span data-slate-string=true>initial</span></span></span></p></div>
<div id=basic contenteditable=true>basic</div><div id=blocked contenteditable=true>unchanged</div>
<script>
let pendingReports=[],reporting=false;
function flushReports(){
 if(reporting||!pendingReports.length)return;
 reporting=true;
 const batch=pendingReports.splice(0);
 fetch('/event',{method:'POST',body:JSON.stringify(batch)}).finally(()=>{reporting=false;flushReports()});
}
function report(name,data={}){pendingReports.push({name,data});queueMicrotask(flushReports)}
plain.onclick=()=>report('PLAIN');server.onclick=e=>report('SERVER',{trusted:e.isTrusted});
for(const name of ['pointerdown','mousedown','mouseup','click','focusin','keydown','keypress','beforeinput','input','selectionchange'])
 document.addEventListener(name,e=>report(name,{target:e.target.id||e.target.nodeName,active:document.activeElement.id,
 key:e.key,code:e.code,keyCode:e.keyCode,charCode:e.charCode,button:e.button,buttons:e.buttons,inputType:e.inputType,
 data:e.data,editorText:editor.textContent,basicText:basic.textContent,blockedText:blocked.textContent,
 ordinaryValue:ordinary.value,which:e.which,selection:String(getSelection()),ranges:getSelection().rangeCount}),true);
blocked.addEventListener('beforeinput',e=>e.preventDefault());
editor.addEventListener('beforeinput',e=>{
 if(e.inputType==='insertText'){
  e.preventDefault();
  if(e.data){const s=getSelection(),r=s.getRangeAt(0);r.deleteContents();const n=document.createTextNode(e.data);r.insertNode(n);r.setStartAfter(n);r.collapse(true);s.removeAllRanges();s.addRange(r);}
 }
 report('EDITOR_INPUT',{data:e.data,text:editor.textContent});
});
report('READY',{contentEditable:editor.isContentEditable,caretRange:typeof document.caretRangeFromPoint,
 caretPosition:typeof document.caretPositionFromPoint,svgType:document.querySelector('svg').constructor.name,
 foreign:[...document.querySelector('svg').children].map(e=>({name:e.localName,child:e.firstElementChild?.constructor.name})),
 svgRect:document.querySelector('svg').getBoundingClientRect().toJSON(),
 nativeServerHit:!!document.elementFromPoint(50,135)?.closest('#server')});
</script>'''
class Server(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        content=page
        if '--native-svg' in sys.argv:
            content+=(port/'tests/svg-native.html').read_text()
        if '--compat' in sys.argv:
            shim=(port/'src/discord-compatibility.js').read_text()
            content=content.replace('<script>','<script>new Function("location",'+json.dumps(shim)+')({protocol:"https:",hostname:"discord.com"});',1)
        data=content.encode();self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def do_POST(self):
        batch=json.loads(self.rfile.read(int(self.headers['Content-Length'])));events.extend(batch);self.send_response(204);self.end_headers()
    def log_message(self,*args):pass
def wait(fn,seconds=90):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if globals().get('app') is not None and app.poll() is not None:
            raise AssertionError(f'Browser exited with {app.returncode}; inspect {out / "browser.log"}')
        value=fn()
        if value:return value
        time.sleep(.06)
    raise AssertionError('Timed out waiting for native fixture')
with tempfile.TemporaryDirectory(prefix='browser-rich-text-') as d:
    root=Path(d);root.chmod(0o700)
    rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1400x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
    with os.fdopen(rd) as f:screen=':'+f.readline().strip()
    x=display.Display(screen)
    env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','XDG_RUNTIME_DIR':d,
         'XDG_CONFIG_HOME':d+'/config','XDG_DATA_HOME':d+'/data','XDG_CACHE_HOME':d+'/cache','XDG_CURRENT_DESKTOP':'GNOME',
         'DOME_BROWSER_FRAME_CAPTURE':str(out/'frame.png')}
    for key in ('WAYLAND_DISPLAY','WAYLAND_SOCKET'):env.pop(key,None)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server);threading.Thread(target=server.serve_forever,daemon=True).start()
    app=None
    def key(name,ctrl=False):
        code=x.keysym_to_keycode(XK.string_to_keysym(name));control=x.keysym_to_keycode(XK.string_to_keysym('Control_L'))
        if ctrl:xtest.fake_input(x,X.KeyPress,control)
        xtest.fake_input(x,X.KeyPress,code);xtest.fake_input(x,X.KeyRelease,code)
        if ctrl:xtest.fake_input(x,X.KeyRelease,control)
        x.sync();time.sleep(.1)
    def click(px,py):
        g=main.get_geometry();main.set_input_focus(X.RevertToParent,X.CurrentTime)
        xtest.fake_input(x,X.MotionNotify,x=g.x+px,y=g.y+py+120)
        xtest.fake_input(x,X.ButtonPress,1);xtest.fake_input(x,X.ButtonRelease,1);x.sync();time.sleep(.4)
    try:
        with (out/'browser.log').open('w') as log:
            binary=os.environ.get('DOME_BROWSER_BINARY',str(port.parents[1]/'target/release/dome-browser'))
            (out/'frame.png').unlink(missing_ok=True)
            app=subprocess.Popen([binary,f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log)
            wait(lambda:any(e['name']=='READY' for e in events))
            wait(lambda:(out/'frame.png').exists())
            main=wait(lambda:next((w for w in x.screen().root.query_tree().children if 'Rich text fixture' in str(w.get_wm_name())),None))
            # A frame capture can contain the initial blank viewport. Wait for
            # the actual control to paint before sending the first native click.
            # The point is inside #plain's background, below its text.
            def control_painted():
                pixel=main.get_image(30,190,1,1,X.ZPixmap,0xffffffff).data
                return pixel[:3]==bytes((204,170,170))
            wait(control_painted)
            g=main.get_geometry();raw=main.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff).data
            rgb=bytearray(g.width*g.height*3);rgb[0::3]=raw[2::4];rgb[1::3]=raw[1::4];rgb[2::3]=raw[0::4]
            (out/'window.ppm').write_bytes(f'P6\n{g.width} {g.height}\n255\n'.encode()+rgb)
            click(50,50);click(50,135);click(250,130);key('x');click(240,220);key('y');
            click(240,320);key('z');click(240,420);key('w');
            wait(lambda:any(e['name']=='beforeinput' and e['data'].get('target')=='blocked' for e in events))
            if '--native-svg' in sys.argv:
                def latest(name):return next((e['data'] for e in reversed(events) if e['name']==name),None)
                wait(lambda:latest('SVG_READY'))
                click(830,68)
                click(820,215);key('v');wait(lambda:latest('SVG_INPUT'))
                click(820,265)
                click(830,410)
                click(1100,400);wait(lambda:latest('TOP_DISABLED'))
                click(830,410)
                click(790,550);click(860,550);click(1050,550);click(1180,550)
                click(1100,60);wait(lambda:latest('PAN_READY'))
                click(850,108)
                click(1100,120);wait(lambda:latest('MOVE_READY'))
                click(920,115)
                click(1100,180);wait(lambda:latest('GROUP_READY'))
                click(920,115);click(860,680)
                click(1100,660);wait(lambda:latest('ADDED'));click(60,550)
                click(1100,710);wait(lambda:latest('REMOVED'));click(60,550)
                g=main.get_geometry()
                xtest.fake_input(x,X.MotionNotify,x=g.x+820,y=g.y+300+120)
                xtest.fake_input(x,X.ButtonPress,1);x.sync();wait(lambda:latest('CAPTURE_DOWN'))
                xtest.fake_input(x,X.MotionNotify,x=g.x+1120,y=g.y+350+120)
                xtest.fake_input(x,X.ButtonRelease,1);x.sync();wait(lambda:latest('CAPTURE_UP'))
            key('q',True);app.wait(timeout=15)
        summary={n:[e['data'] for e in events if e['name']==n] for n in ['READY','PLAIN','SERVER','focusin','keydown','keypress','beforeinput','input','EDITOR_INPUT']}
        print(json.dumps(summary,indent=2),flush=True)
        assert len(summary['PLAIN'])==1,'ordinary server control activates once'
        if '--compat' in sys.argv or '--native-svg' in sys.argv:
            assert len(summary['SERVER'])==1,'SVG server control activates once'
        if '--native-svg' in sys.argv:
            assert summary['SERVER'][0]['trusted'],'SVG click must be a native trusted event'
            assert summary['READY'][0]['nativeServerHit'],'elementFromPoint must hit embedded HTML too'
            clicks=[e['data'] for e in events if e['name']=='SVG_CLICK']
            ids=[e['id'] for e in clicks]
            assert ids.count('scaledButton')==4 and ids.count('svgTop')==1 and ids.count('svgBottom')==1 and ids.count('svgRotated')==1,ids
            assert ids.count('svgClipped')==1 and not any(i in ids for i in ['svgDisabled','svgZero','svgDefinition']),ids
            assert ids.count('svgDynamic')==1 and not latest('REMOVED')['hit'],'adding/removing SVG objects must update native hit targets'
            assert all(e['trusted'] for e in clicks),'all embedded control clicks must remain trusted'
            assert latest('SVG_INPUT')['value']=='v' and latest('SVG_INPUT')['trusted'],'embedded input must focus and type natively'
            r=latest('PAN_READY')['rect']
            assert abs(r['x']-800)<1 and abs(r['y']-84)<1 and abs(r['width']-100)<1 and abs(r['height']-48)<1,r
            r=latest('MOVE_READY')['rect']
            assert abs(r['x']-820)<1 and abs(r['y']-84)<1 and abs(r['width']-120)<1 and abs(r['height']-48)<1,r
            r=latest('GROUP_READY')['rect']
            assert abs(r['x']-828)<1 and abs(r['y']-90)<1 and abs(r['width']-120)<1 and abs(r['height']-48)<1,r
            assert latest('CAPTURE_UP')['trusted'],'pointer capture survives leaving the SVG'
            print('PASS native SVG geometry, trusted clicks, focus and typing, overlap, clipping, mutation and pointer capture',flush=True)
        assert any(e.get('data')=='y' for e in summary['EDITOR_INPUT']),'controlled rich text must receive typed character data'
        assert any(e.get('target')=='basic' and e.get('data')=='z' and e.get('basicText','').count('z')==1 for e in summary['input']),'native rich text inserts once and reports inserted data'
        assert not any(e.get('target')=='blocked' for e in summary['input']),'cancelled beforeinput must prevent insertion'
        assert any(e.get('target')=='blocked' and e.get('blockedText')=='unchanged' for e in summary['beforeinput']),'cancelled editor preserves its contents'
        assert any(e.get('target')=='ordinary' and e.get('ordinaryValue')=='x' for e in summary['input']),'ordinary text field input remains functional'
        assert any(e.get('key')=='y' and e.get('charCode')==ord('y') and e.get('which')==ord('y') for e in summary['keypress']),'native keypress exposes legacy character codes'
        print('PASS controlled/native rich text, cancellation, ordinary input, legacy keyboard events and navigation',flush=True)
    finally:
        (out/'events.json').write_text(json.dumps(events,indent=2)+'\n')
        if app and app.poll() is None:app.kill();app.wait()
        server.shutdown();server.server_close();x.close();xvfb.terminate();xvfb.wait(timeout=5)
