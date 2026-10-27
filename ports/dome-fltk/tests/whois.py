#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Native WHOIS/RWhois protocol and reference-server interoperability."""
import argparse,hashlib,json,os,re,signal,subprocess,sys,tempfile,time,urllib.parse
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_WHOIS_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_WHOIS_TEST_BUS':'1'})
from PIL import Image,ImageChops
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
from whois_fixture import WhoisFixture,WHOIS_TEXT,RWHOIS_RESULT
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--reference-prefix',type=Path);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--wayland',action='store_true')
cases=['whois-index','whois-form','whois-cancel-form','whois-results','whois-latin1','whois-source','whois-large-save','whois-cancel-load','whois-restart','rwhois-index','rwhois-form','rwhois-cancel-form','rwhois-results','rwhois-info','rwhois-object-reference','rwhois-referral','rwhois-partial','rwhois-missing','rwhois-incomplete','rwhois-source','rwhois-large-source','rwhois-cancel-load','rwhois-restart','rwhois-directive-term','reference-results','reference-form','reference-source','reference-missing','reference-restart']
parser.add_argument('--cases',nargs='+',choices=cases);args=parser.parse_args();args.cases=args.cases or [n for n in cases if args.reference_prefix or not n.startswith('reference-')]
binary=args.binary.resolve();out=args.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
records=[];checks=[];runs=[];whois=WhoisFixture(records);rwhois=WhoisFixture(records,'rwhois');leaf=WhoisFixture(records,'rwhois');leaf.tag='leaf';rwhois.referral_port=leaf.server_address[1];reference=None;services=[whois,rwhois,leaf];connection=xvfb=wayland=child=None
if args.reference_prefix:
    from rwhois_reference_fixture import RwhoisReference
    reference=RwhoisReference(args.reference_prefix,out/'reference');services.append(reference)
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
def form():return next((w for w in windows() if str(w.get_wm_name() or '') in ('WHOIS lookup','RWhois lookup','Save WHOIS file')),None)
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
    if wayland:
        body=image.crop((20,145,min(700,image.width-20),min(600,image.height-30)))
        # A blank white first buffer and the compositor's cursor must not
        # satisfy document readiness. Every directory page has a blue action.
        return sum(1 for r,g,b in body.getdata() if b-r>20 and b-g>20)>30
    return any(hi-lo>80 for lo,hi in image.crop((20,145,min(700,image.width-20),min(420,image.height-30))).getextrema())
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
    with tempfile.TemporaryDirectory(prefix='dome-whois-') as directory:
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
            author=name.startswith('reference-');scheme='whois' if name.startswith('whois-') else 'rwhois';service=reference if author else whois if scheme=='whois' else rwhois;label='WHOIS' if scheme=='whois' else 'RWhois';base=f'{scheme}://127.0.0.1:{service.server_address[1]}'
            query='domain a.com' if author else 'Smith' if scheme=='whois' else 'domain example'
            if name.endswith(('index','form')):query=None
            if name=='whois-latin1':query='Renée'
            if name.endswith('cancel-load'):query='slow'
            if name=='rwhois-directive-term':query='-limit 3'
            for suffix in ['info','referral','partial','missing','incomplete']:
                if name=='rwhois-'+suffix:query=suffix
            if name=='reference-missing':query='domain absent.a.com'
            if name in ['whois-large-save','rwhois-large-source']:query='large'
            path='/auth-area=a.com' if author else '/'
            target=base+path+('?' + urllib.parse.urlencode({'q':query,**({'encoding':'latin1'} if name=='whois-latin1' else {})}) if query is not None else '')
            before=len(records);author_before=len(reference.children) if author else 0;service.started.clear() if not author else None;service.closed.clear() if not author else None
            with (folder/'browser.log').open('w') as log:
                child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log)
                try:
                    w=wait(browser,30);check(w is not None,name+': native browser opens')
                    if wayland:wayland.api('/api/action',{'op':'maximize','id':w.id});time.sleep(.3)
                    if query is None:
                        check(ready(label+' lookup'),name+': inert directory landing paints');check(len(reference.children)==author_before if author else len(records)==before,name+': landing sends no query')
                        if name.endswith('form'):
                            query_action(w,1);f=wait(form);check(f is not None,name+': owner-bound native lookup form opens')
                            if name.endswith('cancel-form'):
                                close(f);check(wait(lambda:form() is None),name+': lookup form closes');time.sleep(.3);check(len(records)==before,name+': cancellation sends no lookup');check(wait(lambda:snapshot(profile) and snapshot(profile)['tabs'][0]['url']==target),name+': cancellation retains public directory address')
                            else:
                                query='domain a.com' if author else 'Smith' if scheme=='whois' else 'domain example';fill(f,[query]);check(ready(label+' results'),name+': approved lookup renders');target=base+path+'?'+urllib.parse.urlencode({'q':query})
                    elif name.endswith('cancel-load'):
                        check(service.started.wait(10),name+': pending lookup starts');query='Smith' if scheme=='whois' else 'domain example';target=base+'/?'+urllib.parse.urlencode({'q':query});navigate(w,target);check(ready(label+' results'),name+': replacement lookup renders');check(wait(service.closed.is_set,4),name+': replacement promptly releases the pending socket')
                    elif name=='whois-large-save':
                        f=wait(form);check(f is not None,name+': preview overflow offers a native streaming destination');destination=home/'large.txt';click(f,642,130);chooser=wait(lambda:next((v for v in windows() if v.id not in [w.id,f.id]),None));check(chooser is not None,name+': native save chooser opens');chooser.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(str(destination));key(XK.XK_Return);check(wait(lambda:chooser.id not in [v.id for v in windows()]),name+': explicit destination selected');click(f,680,613);check(wait(destination.exists,40),name+': large response saves');check(destination.read_bytes()==whois.large,name+': streamed response preserves every byte');check(sum(r['command']=='large'for r in records[before:])==1,name+': overflow streams the same connection without resending the query')
                    elif name=='rwhois-directive-term':
                        time.sleep(.4);check(child.poll() is None and len(records)==before,name+': a directive is rejected as a lookup before any connection')
                    elif name in ['rwhois-incomplete','rwhois-large-source']:
                        check(wait(lambda:any(r['command']==query for r in records[before:])),name+': fixture receives the lookup');time.sleep(.5);check(child.poll() is None and label+' results' not in str(w.get_wm_name() or ''),name+': unfinished or oversized result is not accepted as a complete preview')
                        if name=='rwhois-incomplete':
                            query='domain example';target=base+'/?'+urllib.parse.urlencode({'q':query});navigate(w,target);check(ready(label+' results'),name+': subsequent lookup recovers')
                    else:check(ready(label+' results'),name+': complete directory result paints')
                    if name=='rwhois-object-reference':
                        query_action(w,2);check(wait(lambda:any(r['command']=='ID=one.example'for r in records[before:])),'Typed object reference opens an explicit ID lookup');check(ready('RWhois results'),'Object reference result paints');target=base+'/?q=ID%3Done.example'
                    if name=='rwhois-referral':
                        check(not any(r['endpoint']=='leaf' for r in records[before:]),'Rendering a referral makes no unsolicited lookup');query_action(w,2);check(wait(lambda:any(r['endpoint']=='leaf' and r['command']=='referral' for r in records[before:])),'Selected referral preserves the complete lookup');check(ready('RWhois results'),'Selected referral result renders');target=f'rwhois://127.0.0.1:{leaf.server_address[1]}/auth-area=example.invalid?q=referral&hop=1'
                    if name.endswith('source'):
                        w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L);destination=home/'Downloads'/(scheme+'.txt')
                        expected=reference.expected if author else rwhois.large+b'%ok\r\n' if name=='rwhois-large-source' else WHOIS_TEXT if scheme=='whois' else RWHOIS_RESULT
                        check(wait(lambda:destination.exists() and destination.stat().st_size==len(expected),40),name+': native source download finishes')
                        check(destination.read_bytes()==expected,name+': source preserves exact response bytes and original line endings');check(destination.stat().st_mode&0o777==0o600,name+': saved source is private')
                    if name=='whois-latin1':check(any(r['wire_hex']==b'Ren\xe9e\r\n'.hex()for r in records[before:]),'Selected Latin-1 lookup is encoded once for the wire')
                    if name=='rwhois-directive-term':check(wait(lambda:snapshot(profile) and all(t['url']!=target for t in snapshot(profile)['tabs'])),name+': rejected directive expression is excluded from saved resource addresses')
                    else:check(wait(lambda:snapshot(profile) and snapshot(profile)['tabs'][0]['url']==target),name+': session stores the public lookup address')
                    time.sleep(.3);frame(w).save(folder/'frame.png');close(w);check(child.wait(timeout=10)==0,name+': browser closes cleanly');child=None
                    if name.endswith('restart'):
                        child=subprocess.Popen([str(binary),'--software','--profile',str(profile)],env=env,stdout=log,stderr=log);w=wait(browser,30);check(w is not None and ready(label+' results'),'Restart restores and retrieves the saved lookup');close(w);check(child.wait(timeout=10)==0,'Restored lookup closes cleanly');child=None
                    if not author:
                        local=records[before:];check(all(r['command'] in ['-rwhois V-1.5 DomeBrowser/0.1','-display dump','-limit 100','-forward off','-quit'] or not r['command'].startswith('-')for r in local),name+': observed commands stay within lookup and read/session controls')
                    runs.append({'name':name,'passed':True});print('CASE',name,flush=True)
                finally:
                    if sys.exc_info()[0] is not None:
                        for v in windows():
                            try:frame(v).save(folder/f'failure-{v.id}.png')
                            except Exception:pass
                    if child and child.poll() is None:child.terminate();child.wait(timeout=10)
finally:
    for service in services:service.shutdown();service.server_close()
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    (out/'requests.json').write_text(json.dumps(records,indent=2)+'\n')
    (out/'report.json').write_text(json.dumps({'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'fixture_sha256':sha(Path(__file__).with_name('whois_fixture.py')),'native_wayland':args.wayland,'reference_prefix':str(args.reference_prefix) if args.reference_prefix else None,'reference_server_sha256':sha(reference.binary) if reference else None,'cases':runs,'checks':checks,'passed':len(runs)==len(args.cases) and all(c['passed']for c in checks),'scope':'Loopback RFC transcript fixtures and unmodified Fedora/ARIN reference daemon; native directory forms, framing, records, referrals, source saves, large transfers, cancellation and restoration.'},indent=2)+'\n')
