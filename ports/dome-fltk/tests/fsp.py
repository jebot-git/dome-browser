#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Native FSP interoperability with the unmodified author daemon on loopback."""
import argparse,hashlib,json,os,re,signal,subprocess,sys,tempfile,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_FSP_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_FSP_TEST_BUS':'1'})
from PIL import Image,ImageChops
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
from fsp_reference_fixture import FspReference
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--server-binary',type=Path,required=True);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--wayland',action='store_true')
cases=['directory','subdirectory','text','image','binary','large','password-uri','password-form','password-cancel','restart','source']
parser.add_argument('--cases',nargs='+',choices=cases);args=parser.parse_args();args.cases=args.cases or cases
binary=args.binary.resolve();out=args.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
records=[];checks=[];runs=[];server=FspReference(args.server_binary,out/'reference',records);connection=xvfb=wayland=child=None
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def check(ok,name):
    checks.append({'name':name,'passed':bool(ok)});assert ok,name;print('PASS',name,flush=True)
def wait(fn,seconds=15):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        result=fn()
        if result:return result
        time.sleep(.05)
    return None
def windows():
    if wayland:return wayland.windows()
    return [w for w in connection.screen().root.query_tree().children if w.get_attributes().map_state==X.IsViewable]
def browser():
    if wayland:return next((w for w in windows() if str(w.get_wm_name() or '').endswith(' — Dome Browser')),None)
    return next((w for w in windows() if w.get_geometry().width>=1000 and w.get_geometry().height>=700),None)
def form():return next((w for w in windows() if str(w.get_wm_name() or '').startswith(('FSP password','Save FSP file'))),None)
def key(symbol,mod=None):
    if mod:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(mod))
    code=connection.keysym_to_keycode(symbol);xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
    if mod:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(mod))
    connection.sync();time.sleep(.02 if wayland else .005)
def text(value):
    for c in value:
        symbol=XK.string_to_keysym(c) or ord(c)
        code=connection.keysym_to_keycode(symbol);shift=connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
        key(symbol,XK.XK_Shift_L if shift else None)
def click(w,x,y):
    w.set_input_focus(X.RevertToParent,X.CurrentTime);g=w.get_geometry()
    if not wayland:w.configure(stack_mode=X.Above)
    xtest.fake_input(connection,X.MotionNotify,x=g.x+x-1,y=g.y+y);connection.sync();time.sleep(.03)
    xtest.fake_input(connection,X.MotionNotify,x=g.x+x,y=g.y+y);xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync();time.sleep(.1)
def navigate(w,url):
    w.set_input_focus(X.RevertToParent,X.CurrentTime)
    if not wayland:w.configure(stack_mode=X.Above)
    key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(url);key(XK.XK_Return)
def frame(w):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    return Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
def painted(w):
    image=frame(w);return any(hi-lo>80 for lo,hi in image.crop((20,145,min(700,image.width-20),min(420,image.height-30))).getextrema())
def ready(title):return wait(lambda:browser() and title in str(browser().get_wm_name() or '') and painted(browser()))
def close(w):
    if wayland:wayland.api('/api/action',{'op':'close','id':w.id})
    else:w.send_event(protocol.event.ClientMessage(window=w,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);connection.sync()
def link(w,row=0,column=0):
    image=frame(w)
    if wayland and w.value()['maximized'] and image.getpixel((20,150))!=image.getpixel((image.width-20,150)):return False
    red,green,blue=image.split()
    mask=ImageChops.multiply(ImageChops.subtract(blue,red).point(lambda v:255 if v>=20 else 0),ImageChops.subtract(blue,green).point(lambda v:255 if v>=20 else 0))
    pixels=mask.tobytes();groups=[]
    for y in range(160,min(720,image.height-30)):
        found=list(re.finditer(b'\xff{12,}',pixels[y*image.width:(y+1)*image.width]))
        if found:
            if groups and y-groups[-1][0]<=2:groups[-1]=(y,found)
            else:groups.append((y,found))
    if len(groups)<=row or len(groups[row][1])<=column:return False
    y,matches=groups[row];match=matches[column];click(w,(match.start()+match.end())//2,y-4);return True
def query_action(w,steps):
    # Focus the page before its first link, then exercise real DOM keyboard
    # navigation. Root actions share a line, so glyph fragments are not link
    # boundaries. The result-link case below still exercises pointer selection.
    click(w,30,140)
    for _ in range(steps):key(XK.XK_Tab)
    key(XK.XK_Return)
    return True
def fill(w,values):
    for i,value in enumerate(values):click(w,180,130+76*i);key(XK.XK_a,XK.XK_Control_L);text(value)
    click(w,680,613)
def snapshot(profile):
    try:return json.loads((profile/'session.json').read_text())
    except (OSError,ValueError):return None
try:
    with tempfile.TemporaryDirectory(prefix='dome-fsp-') as directory:
        runtime=Path(directory);runtime.chmod(0o700);rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1100x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
        with os.fdopen(rd) as stream:screen=':'+stream.readline().strip()
        connection=display.Display(screen)
        if args.wayland:
            from private_wayland import PrivateWayland
            wayland=PrivateWayland(connection,screen,runtime,out)
        for name in args.cases:
            folder=out/name;folder.mkdir();home=runtime/name;home.mkdir();profile=home/'profile';profile.mkdir(mode=0o700)
            env={**os.environ,'HOME':str(home),'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','XDG_RUNTIME_DIR':directory,'XDG_DATA_HOME':str(home/'data'),'XDG_CONFIG_HOME':str(home/'config'),'XDG_CACHE_HOME':str(home/'cache'),'XDG_STATE_HOME':str(home/'state'),'RUST_LOG':'warn'}
            for k in list(env):
                if k in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','MOZ_GCTIMER','DOME_BROWSER_GC_PROFILE'] or k.startswith(('DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE','DOME_BROWSER_MEMORY_CAPTURE')):env.pop(k,None)
            if wayland:env.pop('DISPLAY',None);env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket)
            base=f'fsp://127.0.0.1:{server.port}'
            path={'directory':'/','subdirectory':'/subfolder/','text':'/read%20me.txt','source':'/read%20me.txt','restart':'/read%20me.txt','image':'/pixel.png','binary':'/file.bin','large':'/large.bin'}.get(name,'/protected/private.txt')
            target=base+path
            if name=='password-uri':target=target.replace('fsp://','fsp://fixture-password@')
            before=len(records)
            with (folder/'browser.log').open('w') as log:
                child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log)
                try:
                    w=wait(browser,30);check(w is not None,name+': native browser opens')
                    if wayland:wayland.api('/api/action',{'op':'maximize','id':w.id});time.sleep(.3)
                    if name in ['binary','large']:
                        f=wait(form);check(f is not None,name+': native destination review opens');check(not any(r['command']==0x42 for r in records[before:]),name+': file body waits for destination approval')
                        destination=home/'saved.bin';click(f,642,130);chooser=wait(lambda:next((v for v in windows() if v.id not in [w.id,f.id]),None));check(chooser is not None,name+': native save chooser opens')
                        chooser.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(str(destination));key(XK.XK_Return)
                        check(wait(lambda:chooser.id not in [v.id for v in windows()]),name+': destination is explicitly selected');click(f,680,613)
                        expected=server.large if name=='large' else server.data
                        check(wait(lambda:destination.exists() and destination.stat().st_size==len(expected),45),name+': native streaming save finishes')
                        check(destination.read_bytes()==expected,name+': every binary byte is preserved')
                        check(destination.stat().st_mode&0o777==0o600,name+': destination uses private permissions')
                    elif name in ['password-form','password-cancel']:
                        check(ready('FSP resource unavailable'),name+': protected resource has an account action');query_action(w,1);f=wait(form);check(f is not None,name+': native password form opens')
                        if name=='password-cancel':
                            count=len(records);close(f);check(wait(lambda:form() is None),name+': password form closes');time.sleep(.4);check(len(records)==count,name+': cancellation sends no password or new request')
                            check(wait(lambda:snapshot(profile) and snapshot(profile)['tabs'][0]['url']==base+path),name+': cancellation preserves the public resource address')
                        else:
                            fill(f,['fixture-password']);check(ready(base+path),name+': approved password loads the resource');check(any(r['fixture_password_matches'] for r in records[before:]),name+': only the approved load receives the fixture password')
                    else:
                        title='FSP archive' if name in ['directory','subdirectory'] else base+path
                        check(ready(title),name+': reference resource visibly renders')
                    if name=='image':
                        def image_pixels():
                            return sum(1 for r,g,b in frame(w).getdata() if g-r>50 and g-b>30)
                        check(wait(lambda:image_pixels()>1000),name+': reference PNG decodes and paints its green pixels')
                    if name=='source':
                        w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L);destination=home/'Downloads'/'read me.txt'
                        check(wait(destination.exists),name+': native source download completes');check(destination.read_bytes()==server.text,name+': source retains exact UTF-8 and CRLF bytes')
                    if name=='directory':
                        commands={r['command'] for r in records[before:]};check({0x4d,0x47,0x41}.issubset(commands),'Directory reads status, directory flags/readme, and complete directory blocks');check(sum(r['command']==0x41 for r in records[before:])>1,'Reference directory spans multiple blocks')
                    check(wait(lambda:snapshot(profile) and snapshot(profile)['tabs'][0]['url']==base+path),name+': session retains public resource URL')
                    frame(w).save(folder/'frame.png');close(w);check(child.wait(timeout=10)==0,name+': browser closes cleanly');child=None
                    if name=='restart':
                        old=len(records);child=subprocess.Popen([str(binary),'--software','--profile',str(profile)],env=env,stdout=log,stderr=log);w=wait(browser,30);check(w is not None and ready(base+path) and len(records)>old,'Restart restores native FSP resource');close(w);check(child.wait(timeout=10)==0,'Restored FSP browser closes cleanly');child=None
                    local=records[before:];check(all(r['command'] in [0x4d,0x47,0x41,0x42,0x4a]for r in local),name+': all observed browser commands are read/session commands')
                    check(any(r['command']==0x4a for r in local),name+': browser terminates its FSP session with BYE')
                    if name.startswith('password'):
                        stored=b''.join(p.read_bytes()for p in profile.rglob('*')if p.is_file());check(b'fixture-password' not in stored and b'fixture-password' not in (folder/'browser.log').read_bytes(),name+': password is absent from saved browser state and diagnostics')
                    runs.append({'name':name,'passed':True});print('CASE',name,flush=True)
                finally:
                    if sys.exc_info()[0] is not None:
                        for v in windows():
                            try:frame(v).save(folder/f'failure-{v.id}.png')
                            except Exception:pass
                    if child and child.poll() is None:child.terminate();child.wait(timeout=10)
finally:
    server.close()
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    (out/'requests.json').write_text(json.dumps(records,indent=2)+'\n')
    (out/'report.json').write_text(json.dumps({'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'fixture_sha256':sha(Path(__file__).with_name('fsp_reference_fixture.py')),'native_wayland':args.wayland,'reference_server_sha256':sha(args.server_binary),'cases':runs,'checks':checks,'passed':len(runs)==len(args.cases) and all(c['passed']for c in checks),'scope':'Native read-only FSP interoperability with the author daemon, private archive, unmodified observed datagrams.'},indent=2)+'\n')
