#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Native Gemini-family transport, input and identity fixtures on private desktops."""
import argparse,hashlib,json,os,signal,socket,socketserver,ssl,subprocess,sys,tempfile,threading,time,urllib.parse
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_GEMINI_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_GEMINI_TEST_BUS':'1'})
from PIL import Image,ImageChops
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--protocol',choices=['gemini','scroll','mercury','gophers','guppy'],default='gemini');p.add_argument('--wayland',action='store_true')
all_cases=['document','plain','input','sensitive','redirect','loop','fallback','temporary','permanent','tls12','cancel','identity','identity-tls12','penguin','xnedit','shared','create','certificate-accept','certificate-decline','identity-rotation','malformed-store','bad-header','unclean','restart']
view_cases=['abstract-reload','abstract-history','abstract-restore','abstract-tabs','abstract-evicted','abstract-resource']
scroll_cases=['abstract','input-link','metadata-empty','bad-metadata','binary','parameters']+view_cases
mercury_cases=['document','plain','input','sensitive','redirect','loop','fallback','temporary','permanent','cancel','bad-header','restart','store-independent','identity-unavailable','long-request','download-source']
gophers_cases=['document','plain','tls12','cancel','certificate-accept','certificate-decline','malformed-store','unclean','restart','no-identity','keep-open']
guppy_cases=['document','plain','input','redirect','loop','permanent','cancel','restart','store-independent','long-request','download-source','reorder','reorder-window','request-retry','ack-retry','eof-retry','missing-eof','sequence-41','bad-header']
p.add_argument('--cases',nargs='+',choices=all_cases+scroll_cases+mercury_cases+gophers_cases+guppy_cases)
a=p.parse_args();a.cases=a.cases or (guppy_cases if a.protocol=='guppy' else gophers_cases if a.protocol=='gophers' else mercury_cases if a.protocol=='mercury' else all_cases+(scroll_cases if a.protocol=='scroll' else []));label='Gopher TLS' if a.protocol=='gophers' else a.protocol.capitalize()
if a.protocol=='guppy' and any(c not in guppy_cases for c in a.cases):p.error('Selected case requires another protocol')
if a.protocol=='gophers' and any(c not in gophers_cases for c in a.cases):p.error('Selected case requires Gemini, Mercury or Scroll')
if a.protocol=='mercury' and any(c not in mercury_cases for c in a.cases):p.error('Selected case requires TLS or Scroll')
if a.protocol=='gemini' and any(c in scroll_cases for c in a.cases):p.error('Scroll-specific cases require --protocol scroll')
binary=a.binary.resolve();out=a.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
checks=[];runs=[];records=[];servers=[];connection=xvfb=child=wayland=None

def sha(path):
    with path.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
def check(value,name):
    checks.append({'name':name,'passed':bool(value)});assert value,name
def certificate(directory,stem,client=False):
    cert=directory/(stem+'.pem');key=directory/(stem+'.key')
    subject='/UID=fixture/CN=Gemini fixture client' if client else '/CN=localhost'
    args=['openssl','req','-x509','-newkey','rsa:2048','-nodes','-utf8','-days','30','-sha256','-subj',subject,'-addext','subjectAltName=DNS:localhost','-addext','basicConstraints=critical,CA:FALSE','-out',str(cert),'-keyout',str(key)]
    if client:args+=['-addext','extendedKeyUsage=clientAuth','-addext','keyUsage=critical,digitalSignature,keyEncipherment']
    subprocess.run(args,check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);cert.chmod(0o600);key.chmod(0o600)
    return cert,key,hashlib.sha256(ssl.PEM_cert_to_DER_cert(cert.read_text())).hexdigest()
class TLSFixture(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True
    def get_request(self):
        tcp,peer=self.socket.accept();tcp.settimeout(10)
        try:return self.context.wrap_socket(tcp,server_side=True),peer
        except BaseException:tcp.close();raise
class PlainFixture(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True
class PlainHandler(socketserver.StreamRequestHandler):
    def handle(self):
        self.rfile.readline(4097);self.wfile.write(b'Replacement native document\r\n')
class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(10)
        try:request=self.rfile.readline(16385 if a.protocol=='scroll' else 4097 if a.protocol=='gophers' else 2049 if a.protocol=='mercury' else 1027)
        except (OSError,ssl.SSLError):return
        if not request:return
        assert request.endswith(b'\r\n') and len(request)<=(16384 if a.protocol=='scroll' else 4096 if a.protocol=='gophers' else 2048 if a.protocol=='mercury' else 1026)
        uri=request[:-2].decode('ascii');metadata=False;languages=[]
        if a.protocol=='scroll':
            uri,tags=uri.rsplit(' ',1);metadata=tags.startswith('+');languages=tags.lstrip('+').split(',')
        if a.protocol=='gophers':uri=f'gophers://localhost:{self.server.server_address[1]}/0'+uri
        url=urllib.parse.urlsplit(uri);peer=self.connection.getpeercert(binary_form=True) if a.protocol!='mercury' else None
        try:trusted=json.loads((self.server.profile/'gemini-trust.json').read_text())['pins'].get(f'{url.scheme}://{url.hostname}:{url.port or (5699 if url.scheme=="scroll" else 70 if url.scheme=="gophers" else 1965)}')
        except (OSError,ValueError,KeyError):trusted=None
        records.append({'metadata':metadata,'languages':languages,'uri':uri,'path':url.path,'query':url.query,'identity':bool(peer),'identity_id':hashlib.sha256(peer).hexdigest() if peer else None,'tls':self.connection.version() if a.protocol!='mercury' else None,'sni':self.server.names.get(self.connection.fileno()),'trusted_before_request':bool(trusted)})
        path=url.path[2:] if a.protocol=='gophers' else url.path
        def wire(data,empty=False):
            if a.protocol=='gophers':return data.partition(b'\r\n')[2]+b'.\r\n'
            if a.protocol=='scroll' and data.startswith(b'2'):
                head,_,content=data.partition(b'\r\n')
                fields=b'\r\n\r\n\r\n' if empty else 'Fixture author 世界\r\n2026-10-25T12:00:00Z\r\n2026-10-25T13:00:00Z\r\n'.encode()
                return head+b'\r\n'+fields+content
            return data
        if path=='/slow':
            self.server.started.set()
            try:
                if self.connection.recv(1)==b'':self.server.closed.set()
            except (OSError,ssl.SSLError):self.server.closed.set()
            return
        if path=='/document' or path.startswith('/long/'):data=((('24 text/scroll' if a.protocol=='scroll' else '20 text/gemini')+'; lang=en\r\n# Local '+label+' fixture\nText 世界\n\n* List item\n> Quote\n```art\n  /\\_/\\\n```\n=> /plain Plain text\n')+('## Section\n### Nested\n##### Paragraph\n* Outer\n** 1. Child\n>> Nested quote\n---\n\\# Literal heading\n=: /input?old Search fixture\n' if a.protocol=='scroll' else '')).encode()
        elif path.startswith('/plain'):data=b'20 text/plain; charset=iso-8859-1\r\ncaf\xe9\r\n'
        elif path=='/input':data=b'20 text/plain\r\nInput accepted\r\n' if url.query=='local%20query' else b'10 Enter a search term\r\n'
        elif path=='/sensitive':data=b'20 text/plain\r\nSensitive input accepted\r\n' if url.query=='gemini%20secret' else b'11 Enter the local fixture secret\r\n'
        elif path=='/redirect':data=b'30 /document\r\n'
        elif path=='/loop':data=b'30 /loop\r\n'
        elif path=='/fallback':data=b'22 text/plain\r\nSuccess status group accepted\r\n'
        elif path=='/temporary':data=b'41 Maintenance\r\n'
        elif path=='/permanent':data=b'51 Missing fixture\r\n'
        elif path.startswith('/auth/'):
            data=b'20 text/plain\r\nScoped identity accepted\r\n' if peer else b'60 A client identity is required\r\n'
        elif path=='/identity-unavailable':data=b'60 A TLS identity cannot be used here\r\n'
        elif path=='/bad-header':data=b'90 Invalid local fixture response\r\n'
        elif path=='/unclean':
            self.wfile.write(wire(b'20 text/plain\r\nIncomplete transport close\r\n'));self.wfile.flush();self.connection.shutdown(socket.SHUT_RDWR);return
        elif path=='/abstract':data=b'20 application/octet-stream\r\n# Resource abstract\nAbstract body\n' if metadata else b'24 text/scroll\r\n# Local Scroll fixture\nResource body\n'
        elif path=='/input-link':data=b'24 text/scroll\r\n# Local Scroll fixture\n=: /input?old Search fixture\n'
        elif path=='/metadata-empty':data=b'24 text/scroll\r\n# Local Scroll fixture\nNo optional metadata\n'
        elif path=='/bad-metadata':
            self.wfile.write(b'24 text/scroll\r\nIncomplete metadata');self.wfile.flush()
            try:self.connection.unwrap().close()
            except (OSError,ssl.SSLError):pass
            return
        elif path=='/binary':data=b'24 application/octet-stream\r\n\0Scroll binary fixture\xff\r\n'
        else:data=('20 text/plain\r\nLocal '+label+' fixture\r\n').encode()
        if a.protocol=='mercury':self.server.bodies[path]=data.partition(b'\r\n')[2]
        self.wfile.write(wire(data,empty=path=='/metadata-empty'));self.wfile.flush()
        if path=='/keep-open':
            try:self.server.closed.set() if self.connection.recv(1)==b'' else None
            except (OSError,ssl.SSLError):pass
        if a.protocol!='mercury':
            try:self.connection.unwrap().close()
            except (OSError,ssl.SSLError):pass

def tls_server(cert,key,client_cert,tls12=False):
    server=TLSFixture(('127.0.0.1',0),Handler);server.names={};server.profile=Path('/nonexistent');server.catalog=None
    server.started=threading.Event();server.closed=threading.Event()
    def context(cert,key):
        ctx=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);ctx.minimum_version=ssl.TLSVersion.TLSv1_2
        if tls12:ctx.maximum_version=ssl.TLSVersion.TLSv1_2
        ctx.load_cert_chain(cert,key);ctx.load_verify_locations(cafile=client_cert);ctx.verify_mode=ssl.CERT_OPTIONAL
        def sni(sock,name,_):
            server.names[sock.fileno()]=name
            if server.catalog and server.catalog.exists():
                try:
                    index=json.loads(server.catalog.read_text())
                    for item in index['identities']:sock.context.load_verify_locations(cafile=item['certificate'])
                except (OSError,ValueError,KeyError,ssl.SSLError):pass
        ctx.sni_callback=sni;return ctx
    server.make_context=context;server.context=context(cert,key);servers.append(server)
    threading.Thread(target=server.serve_forever,daemon=True).start();return server
def windows():
    if wayland:return wayland.windows()
    result=[]
    for w in connection.screen().root.query_tree().children:
        try:
            if w.get_attributes().map_state==X.IsViewable:result.append(w)
        except Exception:pass
    return result

def form_window(title='Gopher+ form'):
    return next((w for w in windows() if str(w.get_wm_name() or '')==title),None)

def browser_window():
    if wayland:return next((w for w in windows() if str(w.get_wm_name() or '').endswith(' — Dome Browser')),None)
    return next((w for w in windows() if w.get_geometry().width>=1000 and w.get_geometry().height>=700),None)

def wait(fn,seconds=12):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        value=fn()
        if value:return value
        time.sleep(.05)
    return None

def key(symbol,mod=None):
    code=connection.keysym_to_keycode(symbol)
    if mod:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(mod))
    xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
    if mod:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(mod))
    connection.sync()
    if wayland:time.sleep(.02)

def shortcut(symbol,*modifiers):
    for modifier in modifiers:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(modifier))
    key(symbol)
    for modifier in reversed(modifiers):xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(modifier))
    connection.sync();time.sleep(.05)

def saved_session(profile,name='session.json'):
    try:return json.loads((profile/name).read_text())
    except (OSError,ValueError):return None

def saved_view(profile,url,abstract):
    session=saved_session(profile)
    if not session:return False
    tab=session['tabs'][session['active']]
    return tab['url']==url and tab.get('protocol_state')==('abstract' if abstract else None)

def text(value):
    for c in value:
        symbol=XK.XK_Return if c=='\n' else XK.XK_Tab if c=='\t' else (XK.string_to_keysym(c) or ord(c))
        code=connection.keysym_to_keycode(symbol)
        shifted=symbol not in [XK.XK_Return,XK.XK_Tab] and connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
        key(symbol,XK.XK_Shift_L if shifted else None)

def click(w,x,y):
    w.set_input_focus(X.RevertToParent,X.CurrentTime);g=w.get_geometry()
    if wayland:
        # XTest can omit a no-op motion when the previous window disappeared
        # under the cursor. Deliver a real motion before the new window's click.
        xtest.fake_input(connection,X.MotionNotify,x=g.x+x-1,y=g.y+y);connection.sync();time.sleep(.03)
    xtest.fake_input(connection,X.MotionNotify,x=g.x+x,y=g.y+y)
    xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync();time.sleep(.1)

def close(w):
    if wayland:wayland.api('/api/action',{'op':'close','id':w.id});return
    w.send_event(protocol.event.ClientMessage(window=w,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);connection.sync()

def capture(w,path):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX').save(path)

def painted(w):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    frame=Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
    # Stay inside the original content area during a native maximize. A white
    # loading surface beside the newly exposed dark area is not painted text.
    return any(hi-lo>80 for lo,hi in frame.crop((20,145,min(700,g.width-20),min(420,g.height-30))).getextrema())

def navigate(w,url):
    # Xvfb has no window manager: explicitly bring the browser above the
    # nonmodal form before clicking its address bar.
    if wayland:
        w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(url);key(XK.XK_Return);return
    w.configure(stack_mode=X.Above);connection.sync();time.sleep(.2)
    click(w,w.get_geometry().width//2,55);key(XK.XK_a,XK.XK_Control_L);text(url);time.sleep(.15);key(XK.XK_Return)

def activate_link(w,skip=0,capture_path=None):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    frame=Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
    # Native controls resize before Servo presents its resized canvas. Do not
    # select coordinates from the old, narrow page while maximizing completes.
    if wayland and w.value()['maximized'] and frame.getpixel((20,150))!=frame.getpixel((g.width-20,150)):return False
    r,green,b=frame.split()
    mask=ImageChops.multiply(ImageChops.subtract(b,r).point(lambda v:255 if v>=20 else 0),ImageChops.subtract(b,green).point(lambda v:255 if v>=20 else 0));pixels=mask.tobytes();last=-10
    for y in range(120,min(650,g.height)):
        match=__import__('re').search(b'\xff{12,}',pixels[y*g.width:(y+1)*g.width])
        if match:
            if y<=last+2:last=y;continue
            last=y
            if skip:skip-=1;continue
            if capture_path:
                frame.save(capture_path)
                capture_path.with_suffix('.json').write_text(json.dumps({'x':(match.start()+match.end())//2,'y':y-3})+'\n')
            click(w,(match.start()+match.end())//2,y-3);return True
    return False

def pick_file(form,y,path,folder,label):
    click(form,642,y)
    chooser=wait(lambda:next((w for w in windows() if w.id!=form.id and 'file' in str(w.get_wm_name() or '').lower()),None),4)
    if chooser is None:
        chooser=next((w for w in windows() if w.id not in [form.id,browser_window().id]),None)
    check(chooser is not None,label+': native file chooser opened')
    capture(chooser,folder/(label+'-chooser.png'))
    chooser.set_input_focus(X.RevertToParent,X.CurrentTime);connection.sync();key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(str(path));key(XK.XK_Return)
    if not wait(lambda:chooser.id not in [w.id for w in windows()],4):
        capture(chooser,folder/(label+'-chooser-pending.png'))
        raise AssertionError(label+': native chooser selection needs review')

def fill_input(form,value,folder):
    if wayland:(folder/'input-scene.json').write_text(json.dumps(wayland.scene(),indent=2)+'\n')
    click(form,180,392);text(value);capture(form,folder/'native-input.png');click(form,680,613)
def choose_identity(form,mode,cert,key_path,folder,remember=True):
    # Empty fixture stores offer Create first and Import PEM files second.
    if mode=='import':
        click(form,700,392);time.sleep(.2)
        key(XK.XK_Down);time.sleep(.2);key(XK.XK_Return);time.sleep(.2)
    if remember:click(form,36,436)
    capture(form,folder/'identity-source.png');click(form,680,613)
    if mode=='import':
        picker=wait(lambda:form_window('Select PEM identity'));check(picker is not None,'Native PEM import opened')
        pick_file(picker,392,cert,folder,'certificate');pick_file(picker,468,key_path,folder,'key')
        capture(picker,folder/'native-identity.png');click(picker,680,613)
    elif mode=='create':
        creation=wait(lambda:form_window('Create TLS identity'));check(creation is not None,'Native shared identity creation opened')
        click(creation,180,130);key(XK.XK_a,XK.XK_Control_L);text('Generated fixture')
        click(creation,180,206);text('new@localhost');capture(creation,folder/'native-create.png');click(creation,680,613)
def ready(url):return wait(lambda:browser_window() and url in str(browser_window().get_wm_name() or ''))
def native_closed():return wait(lambda:not any(str(w.get_wm_name() or '').startswith((label+' input',label+' client identity','Select PEM','Create TLS',label+' certificate','Save '+label)) for w in windows()))

def write_private(path,value):
    path.parent.mkdir(parents=True,exist_ok=True,mode=0o700);path.write_text(json.dumps(value));path.chmod(0o600)
def pins(profile):return json.loads((profile/'gemini-trust.json').read_text())

try:
    with tempfile.TemporaryDirectory(prefix='dome-gemini-') as directory:
        runtime=Path(directory);runtime.chmod(0o700)
        server_cert,server_key,server_id=certificate(runtime,'server')
        next_cert,next_key,next_id=certificate(runtime,'server-next')
        client_cert,client_key,client_id=certificate(runtime,'client',True)
        if a.protocol=='guppy':
            from guppy_fixture import GuppyFixture
            def guppy_response(url):
                path=url.path
                if path=='/input' and url.query!='local%20query':return 1,'Enter a search term',b''
                if path=='/redirect':return 3,'/document',b''
                if path=='/loop':return 3,'/loop',b''
                if path=='/permanent':return 4,'Missing fixture',b''
                if path=='/plain':return 6,'text/plain; charset=iso-8859-1',b'caf\xe9\r\n'
                if path=='/input':return 6,'text/plain',b'Input accepted\r\n'
                return 6,'text/gemini; lang=en',b'# Local Guppy fixture\nText '+ '世界'.encode()+b'\n=> /plain Plain text\n'
            primary=GuppyFixture(guppy_response,records,lambda url:'slow' if url.path=='/slow' else url.path.strip('/') if url.path.strip('/') in ['reorder','reorder-window','request-retry','ack-retry','eof-retry','missing-eof','sequence-41','bad-header'] else 'normal');legacy=primary;servers.append(primary)
        elif a.protocol=='mercury':
            primary=PlainFixture(('127.0.0.1',0),Handler);legacy=primary;primary.names={};primary.bodies={}
            primary.started=threading.Event();primary.closed=threading.Event();primary.make_context=lambda *_:None
            servers.append(primary);threading.Thread(target=primary.serve_forever,daemon=True).start()
        else:primary=tls_server(server_cert,server_key,client_cert);legacy=tls_server(server_cert,server_key,client_cert,True)
        plain=PlainFixture(('127.0.0.1',0),PlainHandler);servers.append(plain);threading.Thread(target=plain.serve_forever,daemon=True).start()
        rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1440x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
        with os.fdopen(rd) as f:screen=':'+f.readline().strip()
        connection=display.Display(screen)
        if a.wayland:
            from private_wayland import PrivateWayland
            wayland=PrivateWayland(connection,screen,runtime,out)
        for name in a.cases:
            folder=out/name;folder.mkdir(mode=0o700);case=runtime/name;case.mkdir(mode=0o700);profile=case/'profile';profile.mkdir(mode=0o700)
            data=case/'data';config=case/'config';data.mkdir(mode=0o700);config.mkdir(mode=0o700)
            env={**os.environ,'HOME':str(case),'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','XDG_RUNTIME_DIR':directory,'XDG_DATA_HOME':str(data),'XDG_CONFIG_HOME':str(config),'XDG_CACHE_HOME':str(case/'cache'),'XDG_STATE_HOME':str(case/'state'),'RUST_LOG':'warn'}
            for k in list(env):
                if k in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','MOZ_GCTIMER','DOME_BROWSER_GC_PROFILE'] or k.startswith(('DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE','DOME_BROWSER_MEMORY_CAPTURE')):env.pop(k,None)
            server=legacy if name in ['tls12','identity-tls12'] else primary
            for s in [primary,legacy]:s.profile=profile;s.catalog=data/'de-xr/identities/index.json';s.started.clear();s.closed.clear()
            port=server.server_address[1];base=f'{a.protocol}://localhost:{port}'+('/0' if a.protocol=='gophers' else '');before=len(records);identity_case=name in ['identity','identity-tls12','penguin','xnedit','shared','create','identity-rotation']
            endpoint='/auth/' if identity_case else '/slow' if name=='cancel' else '/abstract' if name in view_cases else '/document' if name in ['document','tls12','restart','malformed-store','store-independent','download-source','no-identity'] else '/plain' if name.startswith('certificate-') else '/'+name
            target=base+endpoint
            if name=='sensitive':target+='?old'
            if name=='redirect':target+='?carry'
            if name=='document':target+='#fragment'
            if name in view_cases:target+='?lookup=two%20words#2'
            if name=='long-request':target=base+'/long/'+('x'*1400)
            if name=='parameters':target=base+'/document;parameter=1'
            if a.protocol=='guppy':env['DOME_BROWSER_TRACE']='1'
            if a.protocol=='scroll':env.update(LANGUAGE='cs_CZ:en_US',LC_ALL='C.UTF-8')
            if wayland:env.pop('DISPLAY',None);env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket)
            unchanged=None
            if name=='penguin':
                source=data/'penguin-mail/misfin/state.json';write_private(source,{'identities':[{'id':client_id,'address':'fixture@localhost','certificate':client_cert.read_text(),'key':client_key.read_text(),'inbox_url':''}],'pins':{},'recipients':{},'messages':[]});unchanged=(source,sha(source))
            if name=='xnedit':
                source=config/'xnedit-xr/remote.json';write_private(source,{'identity':{'certificate':str(client_cert),'key':str(client_key),'penguin_store':'','penguin_id':''},'pins':{}});unchanged=(source,sha(source))
            if name=='shared':
                source=data/'de-xr/identities/index.json';write_private(source,{'version':1,'identities':[{'id':client_id,'name':'Fixture identity','certificate':str(client_cert),'key':str(client_key),'address':'fixture@localhost'}]});source.parent.chmod(0o700);unchanged=(source,sha(source))
            if name=='no-identity':
                decoded=ssl._ssl._test_decode_cert(str(server_cert))
                pin={'fingerprint':server_id,'not_before':int(ssl.cert_time_to_seconds(decoded['notBefore'])),'expires':int(ssl.cert_time_to_seconds(decoded['notAfter']))}
                authority=f'gophers://localhost:{port}'
                write_private(profile/'gemini-trust.json',{'version':1,'pins':{authority:pin},'identities':[{'authority':authority,'path':'/','server_pin':server_id,'identity':{'kind':'pem','certificate':str(client_cert),'key':str(client_key),'fingerprint':client_id}}]})
            if name in ['malformed-store','store-independent']:write_private(profile/'gemini-trust.json',{'unsupported':'fixture'} )
            with (folder/'browser.log').open('w') as log:
                launch_started=time.monotonic();child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log)
                try:
                    browser=wait(browser_window,30);launch_seconds=time.monotonic()-launch_started
                    if browser is None:
                        diagnostics={'child_exit_code':child.poll(),'windows':[{'title':str(w.get_wm_name()),'width':w.get_geometry().width,'height':w.get_geometry().height} for w in windows()]}
                        if wayland:diagnostics['scene']=wayland.scene()
                        (folder/'launch-diagnostics.json').write_text(json.dumps(diagnostics,indent=2)+'\n')
                    check(browser is not None,name+': native browser opened')
                    if wayland and (name in ['abstract','input-link'] or name in view_cases):
                        # Exercise the user's maximize action so both links
                        # fit inside the private compositor's small work area.
                        wayland.api('/api/action',{'op':'maximize','id':browser.id});time.sleep(.3)
                        check(browser.value()['maximized'],name+': browser maximized for link selection')
                    if name in ['input','sensitive']:
                        form=wait(lambda:form_window(f'{label} input'));check(form is not None,name+': native input prompt opened')
                        if wayland:check(wait(lambda:any(popup['id']==form.id and popup['root']==browser.id for popup in wayland.scene()['popups'])),name+': native prompt stays attached to its browser')
                        fill_input(form,'gemini secret' if name=='sensitive' else 'local query',folder)
                        expected='gemini%20secret' if name=='sensitive' else 'local%20query'
                        check(wait(lambda:any(r['path']==endpoint and r['query']==expected for r in records[before:])),name+': input replaces the original query using percent encoding')
                        check(ready(base+endpoint if name=='sensitive' else base+endpoint+'?'+expected),name+': response uses the expected public address')
                    elif identity_case:
                        form=wait(lambda:form_window(f'{label} client identity'));check(form is not None,name+': native identity selection opened')
                        mode='create' if name=='create' else 'existing' if name in ['penguin','xnedit','shared'] else 'import'
                        choose_identity(form,mode,client_cert,client_key,folder)
                        check(ready(target),name+': selected identity loads the resource')
                        peer=next((r for r in reversed(records[before:]) if r['path']=='/auth/'),None)
                        check(peer is not None and peer['identity'],name+': server receives the selected client certificate')
                        if name=='create':
                            index=json.loads((data/'de-xr/identities/index.json').read_text());check(len(index['identities'])==1,'Browser creation publishes one shared identity')
                            item=index['identities'][0];check(item['address']=='new@localhost','Browser identity is usable as a Misfin certificate')
                            check(peer['identity_id']==item['id'],'Generated identity used by TLS matches the shared catalog')
                            for file in [data/'de-xr/identities/index.json',Path(item['certificate']),Path(item['key'])]:check(file.stat().st_mode&0o777==0o600,'Generated identity files are private')
                        reference=pins(profile)['identities'][0]['identity']
                        check((reference.get('fingerprint') or reference.get('id'))==peer['identity_id'],name+': remembered reference pins the approved client certificate')
                        if unchanged:check(sha(unchanged[0])==unchanged[1],name+': source identity store remains unchanged')
                        if name in ['penguin','xnedit']:check(not (data/'de-xr/identities/index.json').exists(),name+': reuse does not create another identity')
                        start=len(records);navigate(browser,base+'/auth/child');check(ready(base+'/auth/child'),'Remembered identity works below its approved path')
                        check(any(r['identity'] and r['path']=='/auth/child' for r in records[start:]),'Approved child path receives the same identity')
                        start=len(records);navigate(browser,base+'/auth-other/');check(ready(base+'/auth-other/'),'Browser opens a path outside the identity scope')
                        check(any(not r['identity'] and r['path']=='/auth-other/' for r in records[start:]),'Identity is not sent to a path with only a matching prefix')
                        if name=='identity-rotation':
                            primary.context=primary.make_context(next_cert,next_key);start=len(records);navigate(browser,base+'/auth/child')
                            changed=wait(lambda:form_window(f'{label} certificate changed'));check(changed is not None,'Changed server key requires native review')
                            check(len(records)==start,'No Gemini request is sent before changed-key approval');click(changed,680,613)
                            renewed=wait(lambda:form_window(f'{label} client identity'));check(renewed is not None,'Changed server key requires renewed identity approval')
                            check(not any(r['identity'] for r in records[start:]),'Existing identity is not sent to the replacement server key');close(renewed)
                    elif name=='identity-unavailable':
                        check(wait(lambda:bool(records[before:])),'Mercury identity response was requested')
                        time.sleep(.5);check(not any('identity' in str(w.get_wm_name() or '').lower() for w in windows()),'Mercury never opens TLS identity controls')
                        check(not (data/'de-xr/identities').exists(),'Mercury does not create identities')
                        navigate(browser,base+'/plain');check(ready(base+'/plain'),'Mercury remains usable after an unsupported identity response')
                    elif name=='store-independent':
                        check(wait(lambda:f'Local {label} fixture' in str(browser.get_wm_name() or '')),'Mercury does not depend on TLS trust settings')
                        check(pins(profile)=={'unsupported':'fixture'},'Mercury leaves existing trust settings unchanged')
                    elif name=='parameters':
                        time.sleep(.5);check(not records[before:],'Scroll URI parameters are rejected before requesting a resource')
                    elif name=='binary':
                        form=wait(lambda:form_window('Save Scroll file'));check(form is not None,'Scroll binary response offers an explicit native save')
                        destination=case/'response.bin';destination.write_bytes(b'Previous file');click(form,642,130)
                        chooser=wait(lambda:next((w for w in windows() if w.id not in [browser.id,form.id]),None));check(chooser is not None,'Native binary save chooser opened')
                        chooser.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(str(destination));key(XK.XK_Return);time.sleep(.3)
                        confirmation=next((w for w in windows() if w.id not in [browser.id,form.id,chooser.id]),None)
                        if confirmation:confirmation.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_Return)
                        check(wait(lambda:chooser.id not in [w.id for w in windows()]),'Binary save destination selected');click(form,680,613)
                        check(wait(lambda:destination.read_bytes()==b'\0Scroll binary fixture\xff\r\n'),'Scroll binary data excludes metadata and preserves all file bytes')
                        check(destination.stat().st_mode&0o777==0o600,'Scroll saved file is private')
                    elif name in ['cancel','missing-eof']:
                        check(wait(lambda:bool(records[before:])) if name=='missing-eof' else server.started.wait(10),'Fixture begins an incomplete response')
                        navigate(browser,f'nex://127.0.0.1:{plain.server_address[1]}/replacement.txt')
                        check(ready(f'nex://127.0.0.1:{plain.server_address[1]}/replacement.txt'),'Navigation replaces a pending Gemini response')
                        if a.protocol=='guppy':
                            def released():
                                probe=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)
                                try:probe.bind(('127.0.0.1',records[before]['peer_port']));return True
                                except OSError:return False
                                finally:probe.close()
                            check(wait(released,4) and child.poll() is None,'Cancellation releases the Guppy UDP source port')
                        else:check(server.closed.wait(4) and child.poll() is None,'Navigation releases the old TLS connection before application close')
                    elif name=='loop':
                        check(wait(lambda:len(records[before:])>=6),'Gemini redirect limit is exercised');time.sleep(.3)
                        check(len(records[before:])==6,'Gemini follows at most five redirects');check(child.poll() is None,'Redirect limit leaves the browser running')
                    elif name=='malformed-store':
                        time.sleep(.5);check(not records[before:],'Malformed local trust settings prevent requests')
                        check(pins(profile)=={'unsupported':'fixture'},'Malformed local trust settings are preserved')
                        navigate(browser,f'nex://127.0.0.1:{plain.server_address[1]}/replacement.txt');check(ready(f'nex://127.0.0.1:{plain.server_address[1]}/replacement.txt'),'Other native protocols remain usable')
                    elif name in ['bad-header','unclean','bad-metadata']:
                        check(wait(lambda:bool(records[before:])),'Local response fixture was requested');time.sleep(.5);check(child.poll() is None,name+': browser handles the response without an unexpected exit')
                    else:
                        expected_title=target if a.protocol=='gophers' else f'Local {label} fixture' if name in ['document','tls12','redirect','restart','abstract','input-link','metadata-empty','long-request','download-source','reorder','reorder-window','request-retry','ack-retry','eof-retry','sequence-41'] or name in view_cases else target
                        check(wait(lambda:expected_title in str(browser.get_wm_name() or '')),name+': response reaches the native window')
                        if name in ['abstract','input-link'] or name in view_cases:
                            check(wait(lambda:painted(browser)),'Scroll page is painted before selecting a native action')
                            check(len(records[before:])==1,'Scroll does not automatically fetch the abstract or input link')
                            check(wait(lambda:activate_link(browser,skip=1 if name=='input-link' else 0,capture_path=folder/'selected-action.png')),'Scroll exposes the requested native action link')
                            if name=='abstract' or name in view_cases:
                                check(wait(lambda:any(r['metadata'] for r in records[before:])),'Explicit abstract action sends the metadata request modifier')
                                check(wait(lambda:'Resource abstract' in str(browser.get_wm_name())),'Abstract renders as Scrolltext independently of the resource MIME')
                                if name in view_cases:time.sleep(.3)
                                if name=='abstract-reload':
                                    start=len(records);browser.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_r,XK.XK_Control_L)
                                    check(wait(lambda:any(r['metadata'] for r in records[start:])),'Reload retains the abstract request modifier')
                                    check(wait(lambda:saved_view(profile,target,True)),'Reload saves the abstract view alongside its unchanged public URL')
                                    check(wait(lambda:'Resource abstract' in str(browser.get_wm_name())),'Reload displays the abstract again')
                                    start=len(records);key(XK.XK_s,XK.XK_Control_L)
                                    destination=case/'Downloads/abstract'
                                    check(wait(lambda:destination.exists() and destination.read_bytes()==b'# Local Scroll fixture\nResource body\n'),'Saving from an abstract still preserves the original resource bytes')
                                    check(any(not r['metadata'] for r in records[start:]),'Explicit source download does not inherit the abstract navigation modifier')
                                elif name=='abstract-resource':
                                    start=len(records);check(wait(lambda:activate_link(browser)),'Abstract exposes its Open resource action')
                                    check(wait(lambda:any(not r['metadata'] for r in records[start:])),'Open resource explicitly retrieves the resource despite its matching URI and fragment')
                                    check(wait(lambda:saved_view(profile,target,False)),'Opening the resource clears the abstract view state')
                                    check(wait(lambda:'Local Scroll fixture' in str(browser.get_wm_name())),'Open resource displays the original document')
                                elif name=='abstract-history':
                                    browser.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_Left,XK.XK_Alt_L)
                                    check(wait(lambda:'Local Scroll fixture' in str(browser.get_wm_name()) and saved_view(profile,target,False)),'Back selects the resource entry at the same public URL')
                                    key(XK.XK_Right,XK.XK_Alt_L)
                                    check(wait(lambda:'Resource abstract' in str(browser.get_wm_name()) and saved_view(profile,target,True)),'Forward selects the distinct abstract entry at the same public URL')
                                    navigate(browser,base+'/plain');check(ready(base+'/plain'),'Another resource can be visited after an abstract')
                                    key(XK.XK_Left,XK.XK_Alt_L);check(wait(lambda:'Resource abstract' in str(browser.get_wm_name()) and saved_view(profile,target,True)),'Back from another resource restores the abstract')
                                    start=len(records);key(XK.XK_r,XK.XK_Control_L);check(wait(lambda:any(r['metadata'] for r in records[start:])),'Reload of a cached abstract retains its view state')
                                elif name=='abstract-evicted':
                                    for step in range(22):
                                        page=f'nex://127.0.0.1:{plain.server_address[1]}/step-{step:02}'
                                        navigate(browser,page);check(ready(page),f'History eviction setup: page {step+1} loads')
                                    start=len(records)
                                    for _ in range(22):key(XK.XK_Left,XK.XK_Alt_L);time.sleep(.08)
                                    check(wait(lambda:any(r['metadata'] for r in records[start:]),30),'Traversing to an evicted abstract performs a fresh metadata fetch')
                                    check(wait(lambda:'Resource abstract' in str(browser.get_wm_name()) and saved_view(profile,target,True),30),'Evicted abstract restores its public URI and view without an expired action token')
                                elif name in ['abstract-restore','abstract-tabs']:
                                    check(wait(lambda:saved_view(profile,target,True)),'Abstract view reaches the autosaved session')
                                    if name=='abstract-tabs':
                                        browser.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_t,XK.XK_Control_L);navigate(browser,target)
                                        check(wait(lambda:'Local Scroll fixture' in str(browser.get_wm_name()) and saved_view(profile,target,False)),'A second tab at the same URI loads the resource independently')
                                        key(XK.XK_Tab,XK.XK_Control_L);check(wait(lambda:'Resource abstract' in str(browser.get_wm_name())),'Switching tabs retains the first tab abstract')
                                    shortcut(XK.XK_s,XK.XK_Control_L,XK.XK_Shift_L)
                                    check(wait(lambda:saved_session(profile,'saved-session.json') and saved_session(profile,'saved-session.json')['tabs'][0].get('protocol_state')=='abstract'),'Explicit Save Session records the abstract mode')
                                    navigate(browser,base+'/plain');check(ready(base+'/plain'),'Saved view can be left before restoration')
                                    start=len(records);shortcut(XK.XK_r,XK.XK_Control_L,XK.XK_Shift_L)
                                    check(wait(lambda:any(r['metadata'] for r in records[start:])),'Restore Saved Session requests the abstract view')
                                    check(wait(lambda:'Resource abstract' in str(browser.get_wm_name()) and saved_view(profile,target,True)),'Saved-session restoration retains the abstract and public address')
                                    close(browser);check(child.wait(timeout=10)==0,'Abstract session closes cleanly before restart');child=None
                                    start=len(records);child=subprocess.Popen([str(binary),'--software','--profile',str(profile)],env=env,stdout=log,stderr=log);browser=wait(browser_window,30)
                                    check(browser is not None and wait(lambda:'Resource abstract' in str(browser.get_wm_name()) and saved_view(profile,target,True),30),'Automatic session restoration retains the abstract after process restart')
                                    check(any(r['metadata'] for r in records[start:]),'Restart retrieves the abstract without relying on a previous action token')
                                    if name=='abstract-tabs':
                                        session=saved_session(profile);check(len(session['tabs'])==2 and session['tabs'][0].get('protocol_state')=='abstract' and session['tabs'][1].get('protocol_state') is None,'Mixed views at the same URL keep independent saved tab state')
                                        key(XK.XK_Tab,XK.XK_Control_L);check(wait(lambda:'Local Scroll fixture' in str(browser.get_wm_name()) and saved_view(profile,target,False)),'Restored second tab remains a resource view')
                                if name in view_cases:
                                    check(all(r['query']=='lookup=two%20words' and '#' not in r['uri'] for r in records[before:] if r['path']=='/abstract'),'View navigation preserves the lookup query and omits the fragment on the wire')
                                    capture(browser,folder/'view-result.png')
                            else:
                                form=wait(lambda:form_window('Scroll input'));check(form is not None,'Static Scroll input link opens its native prompt')
                                fill_input(form,'local query',folder);check(ready(base+'/input?local%20query'),'Input link replaces the old query with the entered text')
                        if name=='redirect':check(any(r['path']=='/document' and not r['query'] for r in records[before:]),'Gemini redirect does not inherit the old query')
                        if name.startswith('certificate-'):
                            old=pins(profile);primary.context=primary.make_context(next_cert,next_key);start=len(records);navigate(browser,base+'/plain2')
                            form=wait(lambda:form_window(f'{label} certificate changed'));check(form is not None,'Certificate rotation opens native review');capture(form,folder/'native-certificate.png')
                            check(len(records)==start,'Changed certificate sends no application request before approval')
                            if name=='certificate-decline':
                                close(form);navigate(browser,f'nex://127.0.0.1:{plain.server_address[1]}/replacement.txt');check(ready(f'nex://127.0.0.1:{plain.server_address[1]}/replacement.txt'),'Declining certificate rotation permits replacement navigation');check(pins(profile)==old,'Declined certificate does not replace the saved pin')
                            else:
                                click(form,680,613);check(ready(base+'/plain2'),'Approved new certificate loads its resource');check(pins(profile)['pins'][f'{a.protocol}://localhost:{port}']['fingerprint']==next_id,'Approved certificate pin is stored')
                    if a.protocol not in ['mercury','guppy'] and name not in ['malformed-store','parameters']:
                        check(all(r['trusted_before_request'] for r in records[before:]),'Every Gemini request follows a persisted certificate pin')
                        check((profile/'gemini-trust.json').stat().st_mode&0o777==0o600,'Gemini trust settings are private')
                    if a.protocol=='scroll' and name not in ['malformed-store','parameters']:
                        check(all(r['languages']==['cs-CZ','en-US'] for r in records[before:]),'Scroll sends the ordered native BCP47 language preferences')
                    if a.protocol=='gophers':
                        check(all(not r['identity'] for r in records[before:]),'Gopher TLS does not send client identities')
                        if name=='keep-open':check(server.closed.wait(4),'Dot-framed TLS preview completes before the server closes')
                    if a.protocol in ['mercury','guppy']:
                        check(all(r['tls'] is None and not r['identity'] and r['uri'].startswith(a.protocol+'://') for r in records[before:]),'Mercury uses its own plaintext request URI')
                        if name!='store-independent':check(not (profile/'gemini-trust.json').exists(),'Mercury creates no TLS trust state')
                    if name=='document':check(any('#' not in r['uri'] and (a.protocol in ['mercury','guppy'] or r['sni']=='localhost') for r in records[before:]),'Request omits its fragment and TLS uses DNS SNI')
                    if a.protocol=='guppy':
                        check(not primary.errors,'Guppy fixture completes without errors')
                        if name=='redirect':check('SMALLNET_NOTICE Guppy redirected to ' in (folder/'browser.log').read_text(),'Guppy redirect is reported to the native status bar')
                        if name in ['request-retry','ack-retry','eof-retry','reorder','reorder-window']:
                            if name=='request-retry':check(len([r for r in records[before:] if r['path']==endpoint])>=2,'Lost Guppy request is retried')
                            elif name=='eof-retry':check(wait(lambda:any(r.get('eof_acks',0)>=2 for r in records[before:])),'Lost EOF acknowledgement is re-sent')
                            elif name=='ack-retry':check(wait(lambda:any(r.get('retransmissions',0)>=1 and r.get('complete') for r in records[before:])),'Duplicate data is acknowledged again')
                    if name=='long-request':check(any(len(r['uri'])>1024 for r in records[before:]),'Mercury accepts reference-compatible requests longer than Gemini requests')
                    if name=='download-source':
                        browser.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L)
                        destination=case/'Downloads/document'
                        check(wait(lambda:destination.exists() and destination.read_bytes()==primary.bodies['/document']),'Mercury source save preserves the original body without its status header')
                        check(destination.stat().st_mode&0o777==0o600,'Mercury source save is private')
                    if name in ['tls12','identity-tls12']:check(any(r['tls']=='TLSv1.2' for r in records[before:]),'TLS 1.2 remains supported')
                    if native_closed():
                        # These cases deliberately reject the response, so no
                        # document is expected to paint. Their protocol and
                        # process outcomes are checked above.
                        renders=name not in ['loop','bad-header','unclean','bad-metadata','parameters']
                        visible=wait(lambda:painted(browser)) if renders else False
                        capture(browser,folder/'frame.png')
                        if renders:check(visible,name+': response visibly painted')
                    close(browser);check(child.wait(timeout=10)==0,name+': clean browser close');child=None
                    if name=='restart':
                        saved=(profile/('session.json' if a.protocol in ['mercury','guppy'] else 'gemini-trust.json')).read_bytes();child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log)
                        browser=wait(browser_window);check(browser is not None and wait(lambda:(target if a.protocol=='gophers' else f'Local {label} fixture') in str(browser.get_wm_name() or '')),'Saved pin works after browser restart');check((profile/'session.json').read_bytes()==saved if a.protocol in ['mercury','guppy'] else (profile/'gemini-trust.json').read_bytes()==saved,'Restart retains the saved protocol state');close(browser);check(child.wait(timeout=10)==0,'Restarted browser closes cleanly');child=None
                    for file in profile.rglob('*'):
                        if file.is_file() and file.suffix in ['.json','.txt']:check(b'gemini secret' not in file.read_bytes() and b'gemini%20secret' not in file.read_bytes(),name+': sensitive input absent from '+file.name)
                    runs.append({'name':name,'passed':True,'launch_seconds':launch_seconds});print('PASS',name,flush=True)
                finally:
                    if sys.exc_info()[0] is not None and wayland:
                        (folder/'failure-scene.json').write_text(json.dumps(wayland.scene(),indent=2)+'\n')
                        for window in windows():capture(window,folder/f'failure-window-{window.id}.png')
                    if child is not None and child.poll() is None:child.kill();child.wait()
                    child=None;primary.context=primary.make_context(server_cert,server_key)
finally:
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    for server in servers:server.shutdown();server.server_close()
    (out/'report.json').write_text(json.dumps({'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'protocol':a.protocol,'display_backend':'native-wayland' if a.wayland else 'private-x11','wayland_helper_sha256':sha(Path(__file__).with_name('private_wayland.py')) if a.wayland else None,'cases':a.cases,'runs':runs,'checks':checks,'passed':len(runs)==len(a.cases) and all(c['passed'] for c in checks),'scope':'Private local Gemini/Scroll TLS or Mercury TCP fixtures, native input controls, protocol response/cancellation/source saves and clean exit; TLS identity controls remain encrypted-transport-only. No complete all-smallnet claim.'},indent=2)+'\n')
