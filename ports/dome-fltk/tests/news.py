#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Native NNTP/news browsing on private desktops, with synthetic news only."""
import argparse,hashlib,json,os,re,signal,ssl,subprocess,sys,tempfile,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_NEWS_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_NEWS_TEST_BUS':'1'})
from PIL import Image,ImageChops
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
from news_fixture import NewsFixture,article
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--wayland',action='store_true')
cases=['index','overview','nntp','message','number','open-article','older','filter','server','choose-default','server-cancel','legacy','head-fallback','mode-switch','tls','starttls','no-starttls','untrusted','empty','no-group','no-article','cancel','partial-cancel','truncated','mismatch','oversize-line','bad-header','auth','catalogue-limit','source','large-source','attachment','restart']
parser.add_argument('--cases',nargs='+',choices=cases);args=parser.parse_args();args.cases=args.cases or cases
binary=args.binary.resolve();out=args.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
records=[];checks=[];runs=[];servers=[];connection=xvfb=wayland=child=None
def sha(p):
    with p.open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()
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
def form(title=None):return next((w for w in windows() if str(w.get_wm_name() or '') in ([title] if title else ['News server','News group filter','Save News file'])),None)
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

def setting(profile,server,transport,cert,default=True):
    endpoint=f'news://localhost:{server.server_address[1]}/'
    config={'version':1,'default':endpoint if default else None,'servers':{endpoint:{'host':'localhost','port':server.server_address[1],'transport':transport,'ca_file':str(cert) if cert else None}}}
    path=profile/'news-servers.json';path.write_text(json.dumps(config));path.chmod(0o600)
    return path
def choose_destination(w,path,folder):
    click(w,642,130)
    chooser=wait(lambda:next((v for v in windows() if v.id not in [w.id,browser().id] and 'file' in str(v.get_wm_name() or '').lower()),None))
    check(chooser is not None,'Attachment opens the native destination picker')
    chooser.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(str(path));key(XK.XK_Return)
    check(wait(lambda:chooser.id not in [v.id for v in windows()]),'Native destination picker accepts the selected path')
    click(w,680,613)

try:
    with tempfile.TemporaryDirectory(prefix='dome-news-') as directory:
        runtime=Path(directory);runtime.chmod(0o700)
        cert=runtime/'ca.pem';ca_key=runtime/'ca.key';private=runtime/'server.key';leaf=runtime/'server.pem';csr=runtime/'server.csr';extensions=runtime/'server.ext'
        extensions.write_text('subjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n')
        for command in [
            ['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','30','-sha256','-subj','/CN=News fixture CA','-addext','basicConstraints=critical,CA:TRUE','-addext','keyUsage=critical,keyCertSign,cRLSign','-out',str(cert),'-keyout',str(ca_key)],
            ['openssl','req','-new','-newkey','rsa:2048','-nodes','-subj','/CN=localhost','-out',str(csr),'-keyout',str(private)],
            ['openssl','x509','-req','-in',str(csr),'-CA',str(cert),'-CAkey',str(ca_key),'-CAcreateserial','-days','30','-sha256','-extfile',str(extensions),'-out',str(leaf)]]:
            subprocess.run(command,check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        for path in [cert,ca_key,private,leaf]:path.chmod(0o600)
        context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.minimum_version=ssl.TLSVersion.TLSv1_2;context.load_cert_chain(leaf,private)
        plain=NewsFixture(records);tls=NewsFixture(records,context=context,implicit=True);starttls=NewsFixture(records,context=context);servers=[plain,tls,starttls]
        rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1100x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
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
            server=tls if name in ['tls','untrusted'] else starttls if name=='starttls' else plain
            transport='tls' if name in ['tls','untrusted'] else 'starttls' if name in ['starttls','no-starttls'] else 'plain'
            config=profile/'news-servers.json'
            if name!='choose-default':setting(profile,server,transport,cert if transport!='plain' and name!='untrusted' else None)
            for service in servers:service.mode='normal';service.started.clear();service.closed.clear()
            if name in ['legacy','head-fallback','mode-switch','auth','catalogue-limit','truncated','mismatch','oversize-line','bad-header']:server.mode=name
            port=server.server_address[1];base=f'news://localhost:{port}'
            endpoint='/' if name in ['index','filter','server','server-cancel','catalogue-limit'] else '/comp.test'
            if name in ['message','tls','starttls','no-starttls','untrusted','source','large-source','attachment','restart','truncated','mismatch','oversize-line']:endpoint='/Fixture@Example.invalid'
            if name=='cancel':endpoint='/slow@id'
            if name=='partial-cancel':endpoint='/partial@id'
            if name=='empty':endpoint='/comp.empty'
            if name=='no-group':endpoint='/absent'
            if name=='no-article':endpoint='/absent@id'
            target=base+endpoint
            if name=='choose-default':target='news:comp.test'
            if name=='nntp':target=base.replace('news://','nntp://')+'/comp.test'
            if name=='number':target=base.replace('news://','nntp://')+'/comp.test/101'
            before=len(records);sessions_before=sum(len(s.sessions) for s in servers);tls_before=server.tls_attempts
            with (folder/'browser.log').open('w') as log:
                child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log)
                try:
                    w=wait(browser,40);check(w is not None,name+': native browser opens')
                    if wayland:wayland.api('/api/action',{'op':'maximize','id':w.id});time.sleep(.3)
                    invalid=name in ['truncated','mismatch','oversize-line','bad-header','untrusted']
                    if invalid:
                        check(wait(lambda:server.tls_attempts>tls_before if name=='untrusted' else len(records)>before),name+': fixture observes the attempted request')
                        time.sleep(1)
                        check(child.poll() is None and 'News fixture' not in str(w.get_wm_name() or ''),name+': incomplete result is not published as an article')
                        if name=='untrusted':check(not any(r['command'].startswith('ARTICLE') for r in records[before:]),'Untrusted TLS does not fall back to plaintext')
                        server.mode='normal';setting(profile,plain,'plain',None);navigate(w,f'news://localhost:{plain.server_address[1]}/comp.test')
                        check(ready('News: comp.test'),name+': ordinary navigation remains usable')
                    elif name in ['cancel','partial-cancel']:
                        check(wait(server.started.is_set),name+': pending article starts');navigate(w,base+'/comp.test');check(ready('News: comp.test'),name+': replacement navigation completes');check(wait(server.closed.is_set),name+': cancelled connection is released')
                    elif name=='choose-default':
                        f=wait(painted_form);check(f is not None and str(f.get_wm_name())=='News server','Authority-less URI opens native server selection')
                        check(sum(len(s.sessions) for s in servers)==sessions_before,'No server is contacted before selection')
                        click(f,180,130);text('localhost');click(f,180,206);key(XK.XK_a,XK.XK_Control_L);text(str(port));click(f,180,282);key(XK.XK_Down);key(XK.XK_Down);key(XK.XK_Return);frame(f).save(folder/'selected-server.png');click(f,680,613)
                        check(wait(config.exists),'Confirmed server selection saves its settings');(folder/'selected-settings.json').write_text(config.read_text());check(list(json.loads(config.read_text())['servers'].values())[0]['transport']=='plain','Native choice selects the intended plaintext fixture transport')
                        check(ready('News: comp.test'),'Selected default server retrieves the original group')
                        saved=json.loads(config.read_text());check(saved['default'] and list(saved['servers'].values())[0]['transport']=='plain','Explicit server and transport selection persist')
                    elif name in ['filter','server','server-cancel']:
                        check(ready('News groups'),'Native group index paints');query_action(w,2 if name=='filter' else 3);f=wait(painted_form);check(f is not None,name+': native form paints');frame(f).save(folder/'form.png')
                        if name=='filter':fill(f,['comp.*']);check(ready('News groups') and any(r['command']=='LIST ACTIVE comp.*' for r in records[before:]),'Confirmed group filter reaches LIST ACTIVE')
                        elif name=='server-cancel':close(f);check(wait(lambda:form() is None),'Cancelled server form closes');check(config.exists(),'Existing server settings remain available')
                        else:
                            click(f,680,613);check(ready('News groups'),'Saved server configuration reloads its resource');navigate(w,'news:comp.test');check(ready('News: comp.test'),'Authority-less URI reuses the configured default')
                    elif name in ['auth','no-group','no-article','no-starttls']:check(ready('News server response'),name+': server status becomes a readable page')
                    else:
                        title='News groups' if name in ['index','catalogue-limit'] else 'News fixture' if name in ['message','number','tls','starttls','source','large-source','attachment','restart'] else 'News: comp.test' if name!='empty' else 'News: comp.empty'
                        check(ready(title),name+': native resource visibly paints')
                    if name=='open-article':query_action(w,4);check(ready('News fixture'),'Overview link opens the selected numbered article')
                    if name=='older':query_action(w,8);check(wait(lambda:any(r['command']=='OVER 2-51' for r in records[before:])) and ready('News: comp.test'),'Older-page link requests only its bounded article range')
                    if name=='source':
                        w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L);dest=home/'Downloads/article.eml';check(wait(dest.exists),'Native source save streams the original article');check(wait(lambda:dest.exists() and dest.read_bytes()==article()),'Source save preserves MIME bytes and removes only NNTP framing');check(dest.stat().st_mode&0o777==0o600,'Article source is private')
                    if name=='large-source':
                        def rss():
                            try:return int(next(line.split()[1] for line in Path(f'/proc/{child.pid}/status').read_text().splitlines() if line.startswith('VmRSS:')))*1024
                            except (OSError,StopIteration):return 0
                        initial_rss=rss();peak_rss=initial_rss;server.mode='large-source';expected_size=128*1024*1024+len(b'Subject: Large fixture\r\nMessage-ID: <Fixture@Example.invalid>\r\n\r\n')
                        w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L);dest=home/'Downloads/article.eml'
                        def completed():
                            return dest.exists() and dest.stat().st_size==expected_size and server.sessions[-1]['quit']
                        end=time.monotonic()+60
                        while time.monotonic()<end and not completed():peak_rss=max(peak_rss,rss());time.sleep(.05)
                        check(completed(),'128 MiB article source completes through the native download consumer')
                        expected=hashlib.sha256(b'Subject: Large fixture\r\nMessage-ID: <Fixture@Example.invalid>\r\n\r\n');remaining=128*1024*1024
                        while remaining:
                            size=min(1000,remaining);expected.update(b'x'*(size-2)+b'\r\n');remaining-=size
                        check(sha(dest)==expected.hexdigest(),'Large original source retains its exact byte digest')
                        check(peak_rss-initial_rss<96*1024*1024,'Large source download has bounded browser memory growth')
                        (folder/'stream.json').write_text(json.dumps({'bytes':expected_size,'sha256':sha(dest),'rss_before':initial_rss,'rss_peak':peak_rss},indent=2))
                    if name=='attachment':
                        navigate(w,base+'/Fixture@Example.invalid?part=1');f=wait(painted_form);check(f is not None,'MIME attachment opens native save controls');destination=home/'attachment.bin';choose_destination(f,destination,folder);check(wait(destination.exists) and destination.read_bytes()==b'\0\1\xff','Attachment save preserves decoded binary bytes');check(destination.stat().st_mode&0o777==0o600,'Attachment save is private')
                    if name in ['tls','starttls']:check(any(r['command'].startswith('ARTICLE') and r['tls'] for r in records[before:]),name+': retrieval runs inside verified TLS')
                    if name=='starttls':check(sum(r['command']=='CAPABILITIES' for r in records[before:])>=2,'STARTTLS discards and refreshes the capability state')
                    if name=='no-starttls':check(not any(r['command'].startswith(('GROUP','ARTICLE')) for r in records[before:]),'Required STARTTLS is never downgraded')
                    frame(w).save(folder/'frame.png');close(w);check(child.wait(timeout=10)==0,name+': browser closes cleanly');child=None
                    if name=='source':check(dest.exists() and dest.read_bytes()==article(),'Completed article source survives browser shutdown')
                    if name=='large-source':check(dest.exists() and sha(dest)==expected.hexdigest(),'Completed large source survives browser shutdown')
                    if name=='restart':
                        previous=len(records);child=subprocess.Popen([str(binary),'--software','--profile',str(profile)],env=env,stdout=log,stderr=log);w=wait(browser);check(w is not None and ready('News fixture') and len(records)>previous,'Restart restores and retrieves the saved news article');close(w);check(child.wait(timeout=10)==0,'Restored news browser closes cleanly');child=None
                    check(not (profile/'gemini-trust.json').exists(),name+': news transport uses its own server settings')
                    if config.exists():check(config.stat().st_mode&0o777==0o600,name+': server settings remain private')
                    runs.append({'name':name,'passed':True});print('CASE',name,flush=True)
                finally:
                    if sys.exc_info()[0] is not None:
                        for v in windows():
                            try:frame(v).save(folder/f'failure-{v.id}.png')
                            except Exception:pass
                    if child and child.poll() is None:child.terminate();child.wait(timeout=10)
                    child=None
        check(not any(s.errors for s in servers),'All fixture exchanges use only the intended read-only commands')
finally:
    for s in servers:s.shutdown();s.server_close()
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    report={'passed':len(runs)==len(args.cases) and all(c['passed'] for c in checks),'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'fixture_sha256':sha(Path(__file__).with_name('news_fixture.py')),'backend':'native-wayland' if args.wayland else 'private-x11','checks':checks,'runs':runs,'requests':records,'errors':[e for s in servers for e in s.errors],'scope':'Private read-only news/NNTP browsing, native controls, MIME and transport integration; not full protocol or browser conformance.'}
    (out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
