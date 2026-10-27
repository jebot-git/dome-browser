#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Native CCSO/Ph directory browsing and Gopher phonebook interoperability."""
import argparse,hashlib,json,os,re,signal,subprocess,sys,tempfile,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_CSO_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_CSO_TEST_BUS':'1'})
from PIL import Image,ImageChops
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
from cso_fixture import CsoFixture,GopherFixture
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--wayland',action='store_true')
cases=['index','lookup','ph','input','field-input','query-input','query-cancel','fallback','latin1','return-fields','unknown-terminal','no-match','readonly','keep-open','quit-silent','cancel','partial-cancel','truncated','bad-header','oversize-line','document-limit','record-limit','store-independent','auth-unavailable','unsupported-text','denied','gopher-menu','gopher-plus-item2','direct-item2','download-source','restart']
parser.add_argument('--cases',nargs='+',choices=cases);args=parser.parse_args();args.cases=args.cases or cases
binary=args.binary.resolve();out=args.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
records=[];checks=[];runs=[];server=CsoFixture(records);gopher=GopherFixture(server.server_address[1]);connection=xvfb=wayland=child=None
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
def form():return next((w for w in windows() if str(w.get_wm_name() or '')=='Directory lookup'),None)
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
    image=frame(w)
    # A maximize event can precede the resized Servo frame. Do not accept
    # the old viewport painted into one corner as final visual evidence.
    if wayland and w.value()['maximized'] and image.getpixel((20,150))!=image.getpixel((image.width-20,150)):return False
    return any(hi-lo>80 for lo,hi in image.crop((20,145,min(700,image.width-20),min(420,image.height-30))).getextrema())
def painted_form():
    w=form()
    if w and any(hi-lo>50 for lo,hi in frame(w).crop((476,596,744,630)).getextrema()):return w
    return None
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
    with tempfile.TemporaryDirectory(prefix='dome-cso-') as directory:
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
            base=f'{"ph" if name=="ph" else "cso"}://localhost:{server.server_address[1]}'
            endpoint='/' if name in ['index','input','field-input','query-cancel','gopher-menu','gopher-plus-item2','direct-item2','denied'] else '/?q=fixture'
            term={'cancel':'slow','partial-cancel':'partial-hang','no-match':'missing'}.get(name,name)
            if name in ['cancel','partial-cancel','no-match','truncated','bad-header','oversize-line','document-limit','record-limit','unknown-terminal','readonly']:endpoint='/?q='+term
            if name=='query-input':endpoint='/?q='
            if name=='latin1':endpoint='/?q=Ren%C3%A9e&field=name'
            if name=='return-fields':endpoint='/?q=fixture&field=alias&return=name%2Cemail'
            if name=='unsupported-text':endpoint='/?q=%E4%B8%96%E7%95%8C'
            target=base+endpoint
            if name=='auth-unavailable':target=target.replace('cso://','cso://fixture@')
            if name=='direct-item2':target=f'gopher://localhost:{server.server_address[1]}/2ignored%20selector'
            if name=='gopher-plus-item2':target=f'gopher+://localhost:{server.server_address[1]}/2ignored%20selector'
            if name=='gopher-menu':target=f'gopher://127.0.0.1:{gopher.server_address[1]}/1'
            server.mode=name if name in ['fallback','denied','quit-silent'] else 'normal';server.started.clear();server.closed.clear();before=len(records);session_start=len(server.sessions)
            if name=='store-independent':(profile/'gemini-trust.json').write_text('{"unsupported":"fixture"}')
            with (folder/'browser.log').open('w') as log:
                child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log)
                try:
                    w=wait(browser,30);check(w is not None,name+': native browser opens')
                    if wayland:wayland.api('/api/action',{'op':'maximize','id':w.id});time.sleep(.3)
                    negative=name in ['truncated','bad-header','oversize-line','document-limit','record-limit']
                    if name=='gopher-menu':
                        check(wait(lambda:bool(gopher.requests)) and wait(lambda:painted(w)),'Gopher menu visibly loads');query_action(w,1)
                        check(ready('Directory server'),'Gopher type 2 opens the native directory index')
                        check(not any(r.get('verb')=='query' for r in records[before:]),'A phonebook selector is not executed as a directory query')
                    elif name in ['cancel','partial-cancel']:
                        check(wait(server.started.is_set),name+': response starts');navigate(w,base+'/?q=fixture');check(ready('Directory results for fixture'),name+': replacement navigation completes');check(wait(server.closed.is_set),name+': cancelled socket is released')
                    elif negative:
                        check(wait(lambda:any(r.get('verb')=='query' for r in records[before:])),name+': server receives the request');time.sleep(.6)
                        check(child.poll() is None and 'Directory results for' not in str(w.get_wm_name() or ''),name+': invalid or excessive result is not published as a completed page');navigate(w,base+'/?q=fixture');check(ready('Directory results for fixture'),name+': browser remains usable')
                    elif name in ['auth-unavailable','unsupported-text']:
                        check(wait(lambda:painted(w)),name+': native explanation paints');time.sleep(.3);check(len(server.sessions)==session_start,name+': unsupported lookup opens no connection')
                    elif name in ['input','field-input','query-cancel']:
                        check(ready('Directory server'),name+': field index visibly paints');check(form() is None and not any(r.get('verb')=='query' for r in records[before:]),name+': index does not open a form or send a lookup')
                        frame(w).save(folder/'index.png');query_action(w,3 if name=='field-input' else 2);f=wait(painted_form);check(f is not None,name+': native lookup form opens')
                        if wayland:check(wait(lambda:any(p['id']==f.id and p['root']==w.id for p in wayland.scene()['popups'])),name+': form is associated with its browser')
                        frame(f).save(folder/'form.png')
                        if name=='query-cancel':
                            close(f);check(wait(lambda:form() is None),'Cancelled query form closes');check(not any(r.get('verb')=='query' for r in records[before:]),'Cancelling a native form sends no lookup');navigate(w,base+'/?q=fixture');check(ready('Directory results for fixture'),'Navigation works after query cancellation')
                        else:
                            click(f,680,613);check(form() is not None and not any(r.get('verb')=='query' for r in records[before:]),'Empty required search stays in the form')
                            fill(f,['two words'] if name=='field-input' else ['two words','name','name,email']);check(ready('Directory results for two words'),name+': confirmed lookup visibly paints')
                            q=next(r for r in reversed(records) if r.get('verb')=='query');check(q['term']=='two words' and q['field']=='name',name+': selected field and complete search text reach the server')
                            if name=='input':check(q['returns']==['name','email'],'Return-field selection reaches the directory')
                    elif name=='query-input':
                        f=wait(painted_form);check(f is not None,'Empty query URI opens a native lookup form');check(not any(r.get('verb')=='query' for r in records[before:]),'Empty URI query is not sent before confirmation');fill(f,['fixture']);check(ready('Directory results for fixture'),'Confirmed URI lookup paints')
                    elif name in ['index','direct-item2','gopher-plus-item2','denied']:
                        check(ready('Directory server'),name+': directory index or status visibly paints')
                        if name in ['direct-item2','gopher-plus-item2']:check(not any(r.get('verb')=='query' for r in records[before:]),name+': Gopher selector is not used as raw query data')
                    else:check(ready('Directory results for'),name+': directory results visibly paint')
                    if name=='latin1':
                        q=next(r for r in reversed(records) if r.get('verb')=='query');check(q['term']=='Renée' and bytes.fromhex(q['wire_hex']).find(b'Ren\xe9e')>=0,'Lookup uses true ISO-8859-1 bytes and retains the UTF-8 public URI')
                    if name=='return-fields':check(records[-1]['field']=='alias' and records[-1]['returns']==['name','email'],'URI return fields become only the requested read-only query')
                    if name in ['keep-open','quit-silent']:check(wait(lambda:any(s['quit'] for s in server.sessions[session_start:])),'Completed response sends bounded orderly quit without waiting for peer EOF')
                    if name=='store-independent':check(json.loads((profile/'gemini-trust.json').read_text())=={'unsupported':'fixture'},'Directory browsing does not read or modify TLS identity settings')
                    if name=='download-source':
                        w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L);dest=home/'Downloads/directory.txt';check(wait(dest.exists),'Directory source saves through native downloads');check('Renée' in dest.read_text() and '-200:' not in dest.read_text(),'Saved source is decoded text without directory wire framing');check(dest.stat().st_mode&0o777==0o600,'Directory source is saved privately')
                    frame(w).save(folder/'frame.png');close(w);check(child.wait(timeout=10)==0,name+': browser closes cleanly');child=None
                    if name=='restart':
                        previous=len(records);child=subprocess.Popen([str(binary),'--software','--profile',str(profile)],env=env,stdout=log,stderr=log);w=wait(browser);check(w is not None and ready('Directory results for fixture') and len(records)>previous,'Restart restores and retrieves the public directory resource');close(w);check(child.wait(timeout=10)==0,'Restored directory browser closes cleanly');child=None
                    if name!='store-independent':check(not (profile/'gemini-trust.json').exists(),name+': no TLS identity store is created')
                    runs.append({'name':name,'passed':True});print('CASE',name,flush=True)
                finally:
                    if sys.exc_info()[0] is not None:
                        for v in windows():
                            try:frame(v).save(folder/f'failure-{v.id}.png')
                            except Exception:pass
                    if child and child.poll() is None:child.terminate();child.wait(timeout=10)
                    child=None
        check(not server.errors,'All fixture exchanges use the intended read-only commands')
finally:
    server.shutdown();server.server_close();gopher.shutdown();gopher.server_close()
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    report={'passed':len(runs)==len(args.cases) and all(c['passed'] for c in checks),'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'helper_sha256':sha(Path(__file__).with_name('cso_fixture.py')),'display_backend':'native-wayland' if args.wayland else 'private-x11','wayland_helper_sha256':sha(Path(__file__).with_name('private_wayland.py')) if args.wayland else None,'checks':checks,'runs':runs,'requests':records,'sessions':server.sessions,'scope':'Private CSO/Ph browsing, encoding, framing, native input, Gopher phonebook navigation and cancellation only; not full protocol or whole-browser conformance.'}
    (out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
