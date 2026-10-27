#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Native DICT browsing, forms, completion and cancellation on private displays."""
import argparse,hashlib,json,os,re,signal,subprocess,sys,tempfile,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_DICT_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_DICT_TEST_BUS':'1'})
from PIL import Image,ImageChops
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
from dict_fixture import DictFixture
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--wayland',action='store_true')
cases=['definitions','numbered','matches','index','query-cancel','matching-input','database-input','strategy-input','query-input','no-match','unknown-db','unknown-strategy','keep-open','cancel','partial-cancel','truncated','missing-final','bad-count','bad-header','wide','overlong-unicode','oversize-line','document-limit','controls','unicode','escaped-word','store-independent','auth-unavailable','download-source','restart']
parser.add_argument('--cases',nargs='+',choices=cases);args=parser.parse_args();args.cases=args.cases or cases
binary=args.binary.resolve();out=args.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
records=[];checks=[];runs=[];server=DictFixture(records);connection=xvfb=wayland=child=None
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
def form():return next((w for w in windows() if str(w.get_wm_name() or '').startswith(('DICT definition','DICT matching'))),None)
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
    with tempfile.TemporaryDirectory(prefix='dome-dict-') as directory:
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
            base=f'dict://localhost:{server.server_address[1]}'
            path={'definitions':'/d:fixture:*#part','numbered':'/d:fixture:*:2','matches':'/m:scsi:*:prefix:2','index':'/','query-cancel':'/','matching-input':'/','database-input':'/','strategy-input':'/','query-input':'/d:','no-match':'/d:missing','unknown-db':'/d:fixture:unknown','unknown-strategy':'/m:fixture::unknown','cancel':'/d:slow','partial-cancel':'/d:partial-hang','auth-unavailable':'/d:fixture'}.get(name,'/d:'+name)
            if name in ['keep-open','store-independent','download-source','restart']:path='/d:fixture:wn:1'
            target=base+path
            if name=='unicode':target=base+'/d:%E4%B8%96%E7%95%8C:wn'
            if name=='escaped-word':target=base+'/d:two%20%22words%22%3A%5C:wn'
            if name=='auth-unavailable':target=target.replace('dict://','dict://fixture;AUTH@')
            if name=='store-independent':(profile/'gemini-trust.json').write_text('{"unsupported":"fixture"}')
            before=len(records);session_start=len(server.sessions);server.started.clear();server.closed.clear()
            with (folder/'browser.log').open('w') as log:
                child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log)
                try:
                    w=wait(browser,30);check(w is not None,name+': native browser opens')
                    if wayland:wayland.api('/api/action',{'op':'maximize','id':w.id});time.sleep(.3)
                    negative=name in ['truncated','missing-final','bad-count','bad-header','overlong-unicode','oversize-line','document-limit']
                    if name in ['cancel','partial-cancel']:
                        check(wait(server.started.is_set),name+': incomplete response starts');navigate(w,base+'/d:fixture')
                        check(ready('Definitions for fixture'),name+': replacement lookup completes');check(wait(server.closed.is_set),name+': cancelled socket closes promptly')
                    elif negative:
                        check(wait(lambda:len(records)>before),name+': server receives lookup');time.sleep(.6)
                        check(child.poll() is None and 'Definitions for' not in str(w.get_wm_name() or ''),name+': incomplete or invalid response is not a definition page')
                        navigate(w,base+'/d:fixture');check(ready('Definitions for fixture'),name+': browser recovers on next navigation')
                    elif name=='query-cancel':
                        check(ready('Dictionary server'),'Query cancellation starts from a rendered index');check(query_action(w,2),'Query action opens');f=wait(form);check(f is not None,'Query form is available');close(f)
                        check(wait(lambda:form() is None),'Cancelled form closes');check(len(records)==before,'Cancelling the form sends no dictionary lookup');navigate(w,base+'/d:fixture');check(ready('Definitions for fixture'),'Browser remains usable after query cancellation')
                    elif name in ['index','matching-input','database-input','strategy-input']:
                        check(ready('Dictionary server'),name+': database and strategy page paints');check(not records[before:] and form() is None,name+': rendering the index sends no lookup and opens no input prompt')
                        frame(w).save(folder/'index.png')
                        steps={'index':2,'matching-input':3,'database-input':4,'strategy-input':7}[name]
                        check(query_action(w,steps),name+': native query action is clickable');f=wait(form);check(f is not None,name+': native query form opens')
                        check(str(f.get_wm_name())==('DICT matching' if name in ['matching-input','strategy-input'] else 'DICT definition'),name+': intended query action opens the correct form')
                        if wayland:check(wait(lambda:any(p['id']==f.id and p['root']==w.id for p in wayland.scene()['popups'])),name+': query form belongs to its browser')
                        frame(f).save(folder/'form.png')
                        click(f,680,613);check(form() is not None and len(records)==before,name+': empty required word is retained in the form without sending a lookup')
                        values=['two words','wn'] if name=='index' else ['scsi','*','prefix'] if name=='matching-input' else ['fixture']
                        fill(f,values);expected='Matches for' if name in ['matching-input','strategy-input'] else 'Definitions for'
                        check(ready(expected),name+': submitted lookup paints');query=records[-1]
                        check(query['word']==values[0],name+': query preserves the complete input word')
                        if name=='database-input':check(query['database']=='wn',name+': selected database reaches DEFINE')
                        if name=='strategy-input':check(query['strategy']=='prefix',name+': selected strategy reaches MATCH')
                    elif name=='query-input':
                        f=wait(form);check(f is not None,'Empty-word convenience opens a native query form');check(len(records)==before,'No empty lookup is sent before confirmation');fill(f,['fixture','wn']);check(ready('Definitions for fixture'),'Confirmed native query renders')
                    elif name=='matches':
                        check(ready('Matches for scsi'),'Numbered match list paints');frame(w).save(folder/'matches.png')
                        check(wait(lambda:link(w,2,0)),'Match result opens its definition');check(ready('Definitions for Wide SCSI'),'Multiword match definition paints')
                        check(records[-1]['database']=='other' and records[-1]['word']=='Wide SCSI','Match navigation retains its database and complete word')
                    elif name=='auth-unavailable':
                        check(wait(lambda:painted(w)),'Unsupported optional authentication displays a native explanation');time.sleep(.3);check(len(server.sessions)==session_start,'Unsupported authentication opens no anonymous connection')
                        check(wait(lambda:snapshot(profile) and ';AUTH@' not in snapshot(profile)['tabs'][0]['url']),'Unavailable authentication does not persist user information')
                    elif name in ['no-match','unknown-db','unknown-strategy']:
                        title={'no-match':'No matching word','unknown-db':'Unknown database','unknown-strategy':'Unknown matching strategy'}[name]
                        check(ready(title),name+': server status becomes a readable native result')
                    else:
                        expected='Definitions for controls' if name=='controls' else 'Definitions for wide' if name=='wide' else 'Definitions for' if name in ['unicode','escaped-word'] else 'Definitions for fixture'
                        check(ready(expected),name+': definitions visibly paint')
                        if name in ['unicode','escaped-word']:check(records[-1]['word']==('世界' if name=='unicode' else 'two "words":\\'),name+': wire word preserves UTF-8 and escaped URI characters')
                    if name=='download-source':
                        w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L);destination=home/'Downloads/dictionary.txt'
                        check(wait(destination.exists),'Definition source saves through native downloads')
                        body=destination.read_text();check('First definition 世界' in body and '150 2 definitions' not in body,'Saved definition text omits DICT wire framing');check(destination.stat().st_mode&0o777==0o600,'Saved definition source is private')
                    if name=='store-independent':check(json.loads((profile/'gemini-trust.json').read_text())=={'unsupported':'fixture'},'DICT neither depends on nor changes TLS identity state')
                    if name=='keep-open':check(wait(lambda:any(s['quit'] for s in server.sessions[session_start:])),'DICT completes on 250 and sends QUIT without waiting for peer EOF')
                    if not negative and name!='auth-unavailable':check(wait(lambda:snapshot(profile) and snapshot(profile)['tabs'][snapshot(profile)['active']]['url'].startswith(base)),'Public DICT resource persists in the tab session')
                    frame(w).save(folder/'frame.png');close(w);check(child.wait(timeout=10)==0,name+': browser closes cleanly');child=None
                    if name=='restart':
                        old=len(records);child=subprocess.Popen([str(binary),'--software','--profile',str(profile)],env=env,stdout=log,stderr=log);w=wait(browser)
                        check(w is not None and ready('Definitions for fixture') and len(records)>old,'Restart restores and retrieves the saved DICT resource');close(w);check(child.wait(timeout=10)==0,'Restored DICT browser closes cleanly');child=None
                    if name!='store-independent':check(not (profile/'gemini-trust.json').exists(),name+': DICT creates no TLS store')
                    runs.append({'name':name,'passed':True});print('CASE',name,flush=True)
                finally:
                    if sys.exc_info()[0] is not None:
                        for v in windows():
                            try:frame(v).save(folder/f'failure-{v.id}.png')
                            except Exception:pass
                    if child and child.poll() is None:child.terminate();child.wait(timeout=10)
                    child=None
        check(not server.errors,'All fixture command exchanges are valid')
finally:
    server.shutdown();server.server_close()
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    report={'passed':len(runs)==len(args.cases) and all(c['passed'] for c in checks),'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'helper_sha256':sha(Path(__file__).with_name('dict_fixture.py')),'display_backend':'native-wayland' if args.wayland else 'private-x11','wayland_helper_sha256':sha(Path(__file__).with_name('private_wayland.py')) if args.wayland else None,'checks':checks,'runs':runs,'requests':records,'sessions':server.sessions,'scope':'Loopback native DICT UI and transport validation only; no whole-browser or complete DICT conformance claim.'}
    (out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
