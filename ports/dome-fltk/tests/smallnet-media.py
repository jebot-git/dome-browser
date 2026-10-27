#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Native protocol image pixels, original-byte downloads and bitmap clipboard."""
import argparse,hashlib,io,json,os,re,signal,socket,socketserver,ssl,struct,subprocess,sys,tempfile,threading,time,urllib.parse
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_SMALLNET_MEDIA_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_SMALLNET_MEDIA_TEST_BUS':'1'})
from PIL import Image,ImageChops
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--wayland',action='store_true');p.add_argument('--cases',nargs='+')
p.add_argument('--large-transfers',action='store_true');p.add_argument('--transfer-mode',choices=['normal','slow','reset','unclean','missing-eof'],default='normal')
p.add_argument('--gophers-plus',action='store_true',help='Use explicit Gopher+ commands inside TLS')
p.add_argument('--gopher-framing',choices=['count','dot','eof'],default='count')
a=p.parse_args();binary=a.binary.resolve();out=a.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
driver_sha=sha(Path(__file__));checks=[];runs=[];records=[];servers=[];child=xvfb=connection=wayland=None
pixels=Image.new('RGB',(64,48),(231,44,91));pixels.paste((17,219,73),(32,0,64,48))
encoded=io.BytesIO();pixels.save(encoded,format='PNG');png=encoded.getvalue();encoded=io.BytesIO();pixels.save(encoded,format='GIF');gif=encoded.getvalue()
svg=b'<svg xmlns="http://www.w3.org/2000/svg" width="80" height="48"><rect width="80" height="48" fill="#00cc88"/></svg>'
binary_body=b'\0Native binary bytes\r\n.\r\n\xff\x01\n';finger_body=b'Native finger resource\r\nUTF-8 caf\xc3\xa9\r\n'
small_finger=finger_body
if a.large_transfers:
    large_size=128*1024*1024;prefix=binary_body;binary_body=(prefix+bytes(range(256))*(large_size//256+1))[:large_size];finger_line=b'Finger response text\r\n';finger_body=(b'Finger large response\r\n'+finger_line*(large_size//len(finger_line)+1))[:large_size]
    if a.gopher_framing=='dot':binary_body=binary_body[:-2]+b'\r\n'
gopher_text_body=b'.Gopher text\r\nOriginal caf\xe9\r\n'
if a.large_transfers:gopher_text_body=(gopher_text_body*(128*1024*1024//len(gopher_text_body)+1))[:128*1024*1024-2]+b'\r\n'
def dot_wire(body):return (b'.' if body.startswith(b'.') else b'')+body.replace(b'\r\n.',b'\r\n..')+b'.\r\n'
schemes=['gopher','gopher+','gophers','gemini','mercury','guppy','scroll','nex','spartan','ftp']
def check(ok,label):checks.append({'name':label,'passed':bool(ok)});assert ok,label
def wait(fn,seconds=30):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        result=fn()
        if result:return result
        time.sleep(.05)
    return None
def payload(path):
    if path.endswith('.png'):return 'image/png',png
    if path.endswith('.gif'):return 'image/gif',gif
    if path.endswith('.svg'):return 'image/svg+xml',svg
    if path.endswith('.bin'):return 'application/octet-stream',binary_body
    return 'text/plain',finger_body
class Server(socketserver.ThreadingTCPServer):allow_reuse_address=True;daemon_threads=True
class TLSServer(Server):
    def get_request(self):
        stream,peer=self.socket.accept();stream.settimeout(10)
        try:return self.context.wrap_socket(stream,server_side=True),peer
        except BaseException:stream.close();raise
class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(10);line=self.rfile.readline(16385)
        if not line:return
        kind=self.server.kind;plus=False
        if kind in ['tls','mercury']:
            uri=line[:-2].decode().split(' ',1)[0];url=urllib.parse.urlsplit(uri);kind=url.scheme;path=url.path
        elif kind=='gophers':
            fields=line[:-2].split(b'\t');path=fields[0].decode();plus=len(fields)>2
        elif kind=='gopher':
            kind='gopher+' if b'\t' in line else 'gopher';path=line.split(b'\t',1)[0].decode().strip()
        elif kind=='spartan':
            fields=line[:-2].split(b' ');path=fields[1].decode();self.rfile.read(int(fields[2]))
        else:path=line[:-2].decode()
        query=url.query if self.server.kind in ['tls','mercury'] else ''
        submitted=None
        if self.server.kind in ['gopher','gophers'] and line.endswith(b'\t1\r\n'):
            size=self.rfile.readline(32);assert size.startswith(b'+');count=int(size[1:]);assert 0<=count<=8*1024*1024
            submitted=self.rfile.read(count);assert len(submitted)==count
        record={'kind':kind,'path':path,'query':query,'submitted':submitted,'identity':bool(self.connection.getpeercert(binary_form=True)) if self.server.kind=='tls' else False};records.append(record)
        mime,body=payload(path)
        if a.large_transfers and kind=='finger' and sum(r['kind']=='finger' and r['path']==path for r in records)==1:body=small_finger
        if path.endswith('-text.txt') and kind in ['gopher','gopher+']:
            mime='text/plain';body=b'Gopher preview text\r\n' if sum(r['kind']==kind and r['path']==path for r in records)==1 else gopher_text_body
        if path=='/':
            if kind in ['gopher','gopher+','gophers']:body=(f'9Binary resource\t/{kind}-file.bin\t127.0.0.1\t{self.server.server_address[1]}'+('\t+' if kind=='gopher+' or (kind=='gophers' and a.gophers_plus) else '')+'\r\n').encode();mime='application/gopher-menu'
            else:
                action=urllib.parse.parse_qs(query).get('action',[''])[0]
                suffix='-'+action if action else ''
                body=f'# Native resource index\n=> /{kind}{suffix}-file.bin Binary resource\n'.encode();mime='text/scroll' if kind=='scroll' else 'text/gemini'
        if path=='/ask-index':body=f'9Asked binary\t/ask-file.bin\t127.0.0.1\t{self.server.server_address[1]}\t?\r\n'.encode()
        if path in ['/uri-index/','/uri-gophers-index/']:
            secure=path=='/uri-gophers-index/'
            supplied=('gophers' if secure else 'gopher+')+'://127.0.0.1:'+str(ports['gophers' if secure else 'gopher'])+'/9/uri-file.bin%09%09%2B%091%0D%0A%2B3%0D%0Aabc'
            body=f'=> {supplied} Supplied binary\n'.encode()
        if b'!+ASK' in line:body=b'+ASK:\r\n Ask: Native value\r\n AskF: Save response\tresponse.bin\r\n'
        if kind in ['gemini','mercury']:header=f'20 {mime}\r\n'.encode()
        elif kind=='scroll':header=f'24 {mime}\r\nNative image author\r\n\r\n\r\n'.encode()
        elif kind=='spartan':header=f'2 {mime}\r\n'.encode()
        elif kind=='gopher+' or plus:header=(b'+-1\r\n' if a.gopher_framing=='dot' else b'+-2\r\n' if a.gopher_framing=='eof' else b'+'+str(len(body)).encode()+b'\r\n')
        else:header=b''
        if self.server.kind in ['tls','mercury'] and '-retry-' in path:
            if '-retry-input-' in path and query!='local%20query':header=b'10 Enter fixture input\r\n';body=b''
            elif '-retry-sensitive-' in path and query!='stream%20secret':header=b'11 Enter fixture secret\r\n';body=b''
            elif '-retry-identity-' in path and not record['identity']:header=b'60 Select fixture identity\r\n';body=b''
            elif '-retry-redirect-' in path:header=f'31 /{kind}-redirected-file.bin\r\n'.encode();body=b''
        framed_dot=((kind=='gopher+' or plus) and a.gopher_framing=='dot') or (kind in ['gopher','gophers'] and not plus and (path=='/' or path.endswith('-text.txt')))
        wire_body=dot_wire(body) if framed_dot else body
        data=header+wire_body
        try:
            large=a.large_transfers and len(body)>8*1024*1024
            if large and a.transfer_mode in ['slow','reset','unclean']:
                self.wfile.write(header)
                limit=1024*1024 if a.transfer_mode=='reset' else len(wire_body)
                for offset in range(0,limit,65536):
                    self.wfile.write(wire_body[offset:offset+65536]);self.wfile.flush()
                    if a.transfer_mode=='slow':time.sleep(.012)
                if a.transfer_mode in ['reset','unclean','missing-eof']:
                    if a.transfer_mode=='reset':self.connection.setsockopt(socket.SOL_SOCKET,socket.SO_LINGER,struct.pack('ii',1,0))
                    if a.transfer_mode=='unclean':self.connection.shutdown(socket.SHUT_RDWR)
                    self.connection.close();return
            else:self.wfile.write(data);self.wfile.flush()
            if (kind=='gopher+' or plus) and a.gopher_framing in ['count','dot'] and large:
                # Completion is the frame boundary, not the server closing.
                record['client_closed_after_frame']=self.connection.recv(1)==b''
        except (BrokenPipeError,ConnectionResetError,ssl.SSLEOFError):
            record['closed_early']=True;return
        if self.server.kind in ['tls','gophers']:
            try:self.connection.unwrap().close()
            except (OSError,ssl.SSLError):pass
class FTP(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(15);listener=None
        def send(code,text):self.wfile.write(f'{code} {text}\r\n'.encode());self.wfile.flush()
        send(220,'Media fixture')
        try:
            while True:
                line=self.rfile.readline(16385)
                if not line:return
                verb,_,arg=line[:-2].partition(b' ');verb=verb.decode()
                if verb=='USER':send(331,'Password')
                elif verb=='PASS':send(230,'Anonymous fixture login')
                elif verb=='FEAT':self.wfile.write(b'211-Features\r\n UTF8\r\n MLST type*;size*;\r\n211 End\r\n');self.wfile.flush()
                elif verb in ['OPTS','TYPE','CWD']:send(200,'Selected')
                elif verb=='SIZE':send(213,str(len(payload(arg.decode())[1])))
                elif verb=='EPSV':
                    listener=socket.socket();listener.bind(('127.0.0.1',0));listener.listen(1);listener.settimeout(10);send(229,f'Extended passive (|||{listener.getsockname()[1]}|)')
                elif verb in ['RETR','MLSD']:
                    path='/'+arg.decode() if verb=='RETR' else '/';records.append({'kind':'ftp','path':path})
                    data_bytes=payload(path)[1] if verb=='RETR' else b'Type=file;Size=32; ftp-file.bin\r\n'
                    send(150,'Data');data,_=listener.accept()
                    with data:data.sendall(data_bytes)
                    listener.close();listener=None;send(226,'Complete')
                elif verb=='QUIT':send(221,'Goodbye');return
                else:raise AssertionError('Unexpected media fixture FTP command')
        finally:
            if listener:listener.close()
def service(kind,handler=Handler,context=None):
    server=(TLSServer if context else Server)(('127.0.0.1',0),handler);server.kind=kind
    if context:server.context=context
    servers.append(server);threading.Thread(target=server.serve_forever,daemon=True).start();return server.server_address[1]
def windows():
    if wayland:return wayland.windows()
    result=[]
    for w in connection.screen().root.query_tree().children:
        try:
            if w.get_attributes().map_state==X.IsViewable:result.append(w)
        except Exception:pass
    return result
def browser():
    if wayland:return next((w for w in windows() if str(w.get_wm_name() or '').endswith(' — Dome Browser')),None)
    return next((w for w in windows() if w.get_geometry().width>=1000 and w.get_geometry().height>=700),None)
def key(symbol,*modifiers):
    for modifier in modifiers:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(modifier))
    code=connection.keysym_to_keycode(symbol);xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
    for modifier in reversed(modifiers):xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(modifier))
    connection.sync();time.sleep(.03)
def text(value):
    for character in value:
        symbol=XK.string_to_keysym(character) or ord(character);code=connection.keysym_to_keycode(symbol)
        shifted=connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
        key(symbol,*([XK.XK_Shift_L] if shifted else []))
def focus(w):w.set_input_focus(X.RevertToParent,X.CurrentTime);connection.sync()
def navigate(w,url,paint=True):
    def body():
        image=frame(w);return image.crop((8,125,image.width-8,image.height-30))
    before=body().tobytes();focus(w);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(url);key(XK.XK_Return)
    if paint:
        last=None;stable=time.monotonic()
        def settled():
            nonlocal last,stable
            image=body();current=image.tobytes()
            if current!=last:last=current;stable=time.monotonic()
            return current!=before and time.monotonic()-stable>=.2 and any(hi-lo>80 for lo,hi in image.getextrema())
        check(wait(settled),'Native resource paints after navigation')
def frame(w):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff);return Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
def point(w,x,y,button=1):
    focus(w);g=w.get_geometry();xtest.fake_input(connection,X.MotionNotify,x=g.x+x-1,y=g.y+y);connection.sync();time.sleep(.03)
    xtest.fake_input(connection,X.MotionNotify,x=g.x+x,y=g.y+y);xtest.fake_input(connection,X.ButtonPress,button);connection.sync();time.sleep(.04);xtest.fake_input(connection,X.ButtonRelease,button);connection.sync();time.sleep(.15)
def color_point(w,color):
    image=frame(w);matches=[]
    for y in range(100,image.height-30):
        for x in range(8,image.width-8):
            if image.getpixel((x,y))==color:matches.append((x,y))
    if len(matches)<200:return None
    return matches[len(matches)//2]
def link_point(w,skip=0):
    image=frame(w);r,g,b=image.split();mask=ImageChops.multiply(ImageChops.subtract(b,r).point(lambda v:255 if v>=20 else 0),ImageChops.subtract(b,g).point(lambda v:255 if v>=20 else 0));pixels=mask.tobytes()
    last=-10
    for y in range(100,min(600,image.height-30)):
        match=re.search(b'\xff{12,}',pixels[y*image.width:(y+1)*image.width])
        if match:
            if y<=last+2:last=y;continue
            last=y
            if skip:skip-=1;continue
            return ((match.start()+match.end())//2,y-3)
def context_action(w,position,from_last=0):
    point(w,*position,button=3);time.sleep(.25)
    # Moving outside clears the popup's hover selection; Shift+Tab wraps from
    # no selection to the last FLTK item, including on native Wayland.
    xtest.fake_input(connection,X.MotionNotify,x=2,y=2);connection.sync();time.sleep(.1)
    for _ in range(from_last+1):key(XK.XK_Tab,XK.XK_Shift_L)
    key(XK.XK_Return)
def clipboard(env):
    if wayland:
        def offer():
            result=subprocess.run(['wl-paste','--list-types'],env=env,capture_output=True,timeout=3)
            return next((kind for kind in ['image/png','image/bmp','image/x-bmp'] if kind in result.stdout.decode().splitlines()),None)
        kind=wait(offer);check(kind is not None,'Native Wayland clipboard offers an image MIME type')
        result=subprocess.run(['wl-paste','--no-newline','--type',kind],env=env,capture_output=True,timeout=5);check(result.returncode==0,'Native Wayland clipboard bytes can be read');return result.stdout
    owner=connection.screen().root.create_window(0,0,1,1,0,X.CopyFromParent,event_mask=X.PropertyChangeMask)
    def selection(kind):
        prop=connection.intern_atom('DOME_NATIVE_MEDIA_CLIP');owner.convert_selection(connection.intern_atom('CLIPBOARD'),connection.intern_atom(kind),prop,X.CurrentTime);connection.flush()
        def received():
            while connection.pending_events():
                event=connection.next_event()
                if event.type==X.SelectionNotify:return owner.get_full_property(prop,X.AnyPropertyType) if event.property else None
        return wait(received)
    try:
        offered=selection('TARGETS');check(offered is not None,'X11 clipboard exposes targets');names=[connection.get_atom_name(int(value)) for value in offered.value]
        kind=next((kind for kind in ['image/png','image/bmp','image/x-bmp'] if kind in names),None);check(kind is not None,'X11 clipboard offers an image MIME type')
        return bytes(selection(kind).value)
    finally:owner.destroy();connection.flush()

try:
 with tempfile.TemporaryDirectory(prefix='dome-smallnet-media-') as directory:
    runtime=Path(directory);runtime.chmod(0o700);profile=runtime/'profile';profile.mkdir(mode=0o700);config=runtime/'config';config.mkdir(mode=0o700);downloads=runtime/'Downloads'
    (config/'user-dirs.dirs').write_text('XDG_DOWNLOAD_DIR="$HOME/Downloads"\n')
    cert=runtime/'server.pem';private=runtime/'server.key';subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','30','-sha256','-subj','/CN=localhost','-addext','subjectAltName=DNS:localhost','-out',str(cert),'-keyout',str(private)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.minimum_version=ssl.TLSVersion.TLSv1_2;context.load_cert_chain(cert,private)
    if a.large_transfers:
        client_cert=runtime/'client.pem';client_key=runtime/'client.key'
        subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','30','-sha256','-subj','/CN=Stream fixture/emailAddress=fixture@localhost','-addext','extendedKeyUsage=clientAuth','-out',str(client_cert),'-keyout',str(client_key)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        client_cert.chmod(0o600);client_key.chmod(0o600);context.verify_mode=ssl.CERT_OPTIONAL;context.load_verify_locations(cafile=client_cert)
        der=subprocess.run(['openssl','x509','-in',str(client_cert),'-outform','DER'],check=True,capture_output=True).stdout
        catalog=runtime/'data/de-xr/identities/index.json';catalog.parent.mkdir(parents=True,mode=0o700)
        catalog.write_text(json.dumps({'version':1,'identities':[{'id':hashlib.sha256(der).hexdigest(),'name':'Stream fixture','certificate':str(client_cert),'key':str(client_key),'address':'fixture@localhost'}]}));catalog.chmod(0o600)
    ports={kind:service(kind) for kind in ['gopher','nex','spartan','finger','mercury']};ports['tls']=service('tls',context=context);ports['gophers']=service('gophers',context=context);ports['ftp']=service('ftp',FTP)
    from guppy_fixture import GuppyFixture
    def guppy_response(url):
        path=url.path;mime,body=payload(path)
        if path=='/':
            action=urllib.parse.parse_qs(url.query).get('action',[''])[0];suffix='-'+action if action else ''
            return 6,'text/gemini',f'# Native resource index\n=> /guppy{suffix}-file.bin Binary resource\n'.encode()
        if '-retry-input-' in path and url.query!='local%20query':return 1,'Enter fixture input',b''
        if '-retry-redirect-' in path:return 3,'/guppy-redirected-file.bin',b''
        return 6,mime,body
    guppy=GuppyFixture(guppy_response,records,lambda url:'missing-eof' if a.transfer_mode=='missing-eof' and url.path.endswith('.bin') else 'normal');servers.append(guppy);ports['guppy']=guppy.server_address[1]
    def address(scheme,path,kind=None):
        port=ports['tls' if scheme in ['gemini','scroll'] else 'gopher' if scheme=='gopher+' else scheme];host='localhost' if scheme in ['gemini','scroll'] else '127.0.0.1'
        if scheme in ['gopher','gopher+','gophers']:path='/'+(kind or '1')+path
        if scheme=='gophers' and a.gophers_plus:path+='%09%09%2B'
        return f'{scheme}://{host}:{port}{path}'
    cases=[(scheme+'-'+extension,scheme,extension) for extension in ['png','svg'] for scheme in schemes]+[('gopher-gif','gopher','gif')]+[(scheme+'-binary',scheme,'binary') for scheme in schemes]+[('finger-source','finger','text'),('direct-binary-save','gemini','direct'),('direct-binary-cancel','gemini','cancel')]
    if a.large_transfers:
        cases=[(scheme+'-'+label,scheme,extension) for label,extension in [('binary','binary'),('native-save','direct'),('native-cancel','cancel')] for scheme in ['nex','spartan','gemini','mercury','guppy','scroll','gopher','gopher+','gophers']]+[('finger-source','finger','text'),('gopher-text','gopher','gopher-text'),('gopher+-text','gopher+','gopher-text')]
        cases += [(scheme+'-'+action,scheme,action) for scheme in ['gemini','scroll'] for action in ['retry-input','retry-sensitive','retry-identity','retry-redirect']]
        cases += [('guppy-'+action,'guppy',action) for action in ['retry-input','retry-redirect']]
        cases += [('mercury-'+action,'mercury',action) for action in ['retry-input','retry-sensitive','retry-redirect']]
        cases += [('gopher+-ask-download','gopher+','gopher-ask'),('gopher+-uri-download','gopher+','gopher-uri')]
        cases += [('gophers-ask-download','gophers','gopher-ask'),('gophers-uri-download','gophers','gopher-uri')]
        if a.transfer_mode=='missing-eof':cases=[case for case in cases if case[1]=='guppy' and case[2]=='binary']
        if a.transfer_mode=='unclean':cases=[case for case in cases if case[1] in ['gemini','scroll','gophers'] and case[2]=='binary']
    if a.cases:cases=[case for case in cases if case[0] in a.cases];assert len(cases)==len(a.cases),'Unknown test case'
    rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1440x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
    with os.fdopen(rd) as stream:screen=':'+stream.readline().strip()
    connection=display.Display(screen)
    if a.wayland:
        from private_wayland import PrivateWayland
        wayland=PrivateWayland(connection,screen,runtime,out)
    env={**os.environ,'HOME':directory,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','XDG_RUNTIME_DIR':directory,'XDG_DATA_HOME':str(runtime/'data'),'XDG_CONFIG_HOME':str(config),'XDG_CACHE_HOME':str(runtime/'cache'),'XDG_STATE_HOME':str(runtime/'state'),'RUST_LOG':'warn','DOME_BROWSER_TRACE':'1'}
    for name in list(env):
        if name in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','MOZ_GCTIMER','DOME_BROWSER_GC_PROFILE'] or name.startswith(('DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE','DOME_BROWSER_MEMORY_CAPTURE')):env.pop(name,None)
    if wayland:env.pop('DISPLAY',None);env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket)
    log=(out/'browser.log').open('w')
    try:child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),'about:blank'],env=env,stdout=log,stderr=log)
    finally:log.close()
    w=wait(browser);check(w is not None,'Native browser window opens')
    for name,scheme,extension in cases:
        folder=out/name;folder.mkdir(mode=0o700);since=len(records);errors_before=(out/'browser.log').read_text().count('DOWNLOAD_ERROR')
        try:
            if extension in ['png','svg','gif']:
                filename=scheme+'-picture.'+extension;path='/'+filename;url=address(scheme,path,'g' if extension=='gif' else 'I');navigate(w,url)
                color=(0,204,136) if extension=='svg' else (231,44,91);position=wait(lambda:color_point(w,color));check(position is not None,name+': original image pixels visibly render')
                check(url in str(w.get_wm_name()),name+': native resource URL remains public');check(sum(r['kind']==scheme and r['path']==path for r in records[since:])==1,name+': displaying the image needs one native retrieval')
                frame(w).save(folder/'frame.png');focus(w);key(XK.XK_s,XK.XK_Control_L)
                target=downloads/filename;expected_bytes=payload(path)[1];check(wait(lambda:target.exists() and target.read_bytes()==expected_bytes),name+': Ctrl+S saves original encoded image bytes')
                check(target.stat().st_mode&0o777==0o600,name+': saved image is private');check(url in str(w.get_wm_name()),name+': saving retains the current image page')
                if name=='gopher-png':
                    context_action(w,position,from_last=1);check(wait(lambda:(downloads/'image.png').exists() and (downloads/'image.png').read_bytes()==png),'Save Image preserves the native image bytes')
                    context_action(w,position);copied=Image.open(io.BytesIO(clipboard(env))).convert('RGB');check(copied.size==pixels.size and copied.tobytes()==pixels.tobytes(),'Copy Image preserves original dimensions and pixels');copied.save(folder/'clipboard.png')
            elif extension in ['gopher-ask','gopher-uri']:
                asked=extension=='gopher-ask';url=address(scheme if asked else 'nex','/ask-index' if asked else '/uri-gophers-index/' if scheme=='gophers' else '/uri-index/')+'#native-index';navigate(w,url)
                position=wait(lambda:link_point(w));check(position is not None,name+': native index displays its submission link');context_action(w,position,from_last=1)
                title='Gopher+ form' if asked else 'Review Gopher+ data';form=wait(lambda:next((item for item in windows() if item.get_wm_name()==title),None));check(form is not None,name+': native approval opens before submission')
                check(not any(r['submitted'] is not None for r in records[since:]),name+': no data is submitted before approval')
                destination=runtime/'selected-form-response.bin'
                if asked:
                    point(form,180,130);text('stream answer');point(form,642,206)
                    chooser=wait(lambda:next((item for item in windows() if item.id not in [w.id,form.id]),None));check(chooser is not None,name+': native response destination chooser opens')
                    focus(chooser);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(str(destination));key(XK.XK_Return)
                    check(wait(lambda:chooser.id not in [item.id for item in windows()]),name+': selected response destination is accepted')
                    destination.write_bytes(b'Previous selected destination')
                frame(form).save(folder/'native-form.png');point(form,680,613)
                target=downloads/('ask-file.bin' if asked else 'uri-file.bin')
                if a.transfer_mode=='reset':
                    check(wait(lambda:(out/'browser.log').read_text().count('DOWNLOAD_ERROR')>errors_before),name+': interrupted submitted response reports a download error')
                    check(not target.exists(),name+': interrupted response removes the download')
                    if asked:check(destination.read_bytes()==b'Previous selected destination',name+': interrupted response preserves the selected destination')
                else:
                    check(wait(lambda:target.exists() and target.read_bytes()==binary_body,seconds=60),name+': submitted response reaches the original download consumer')
                    if asked:check(destination.read_bytes()==binary_body and destination.stat().st_mode&0o777==0o600,name+': same stream saves the explicitly selected private destination')
                answers=[r['submitted'] for r in records[since:] if r['submitted'] is not None]
                check(answers==[b'stream answer\r\nselected-form-response.bin\r\n' if asked else b'abc'],name+': exactly one approved data block is submitted')
                check(json.loads((profile/'session.json').read_text())['tabs'][json.loads((profile/'session.json').read_text())['active']]['url']==url,name+': source index stays open')
                check(not list(runtime.glob('.dome-smallnet-*')),name+': temporary form destinations are removed')
            elif extension=='binary' or extension.startswith('retry-'):
                action=extension if extension.startswith('retry-') else ''
                url=address(scheme,'/')+('?action='+action if action else '')+'#native-index';navigate(w,url);position=wait(lambda:link_point(w,skip=1 if scheme=='scroll' else 0));check(position is not None,name+': native index displays its binary resource link')
                context_action(w,position,from_last=1);target=downloads/(scheme+('-'+action if action else '')+'-file.bin')
                if action=='retry-redirect':target=downloads/(scheme+'-redirected-file.bin')
                if action in ['retry-input','retry-sensitive','retry-identity']:
                    title=scheme.capitalize()+(' client identity' if action=='retry-identity' else ' input')
                    form=wait(lambda:next((item for item in windows() if item.get_wm_name()==title),None));check(form is not None,name+': continuation opens its native prompt')
                    if action!='retry-identity':point(form,180,392);text('stream secret' if action=='retry-sensitive' else 'local query')
                    frame(form).save(folder/'native-prompt.png');point(form,680,613)
                if a.large_transfers and a.transfer_mode in ['reset','unclean','missing-eof']:
                    check(wait(lambda:(out/'browser.log').read_text().count('DOWNLOAD_ERROR')>errors_before,seconds=60 if a.transfer_mode=='missing-eof' else 30),name+': interrupted download reports an error')
                    check(not target.exists(),name+': interrupted download removes its partial file')
                    check(not list(downloads.glob('*.part')),name+': interrupted download leaves no temporary download')
                else:check(wait(lambda:target.exists() and target.read_bytes()==binary_body,seconds=60),name+': Save Link preserves binary bytes without transfer framing')
                saved_session=json.loads((profile/'session.json').read_text());check(saved_session['tabs'][saved_session['active']]['url']==url,name+': Save Link keeps the index open')
                if scheme in ['gopher','gopher+']:check(any(r['kind']==scheme and r['path']=='/'+scheme+'-file.bin' for r in records[since:]),name+': file retrieval uses the requested Gopher protocol')
                if action:
                    responses=[r for r in records[since:] if r['path'].endswith('-file.bin')]
                    check(len(responses)==2,name+': exactly one continuation reaches the original file consumer')
                    if action=='retry-identity':check(not responses[0]['identity'] and responses[1]['identity'],name+': selected shared certificate is used only after approval')
                    if action=='retry-sensitive':check(not any(b'stream%20secret' in f.read_bytes() or b'stream secret' in f.read_bytes() for f in profile.glob('*.json')),name+': sensitive input is absent from saved browser state')
            elif extension=='gopher-text':
                url=address(scheme,'/'+name+'.txt','0');navigate(w,url);check(wait(lambda:url in str(w.get_wm_name())),'Gopher text preview loads');focus(w);key(XK.XK_s,XK.XK_Control_L)
                target=downloads/(name+'.txt')
                if a.transfer_mode=='reset':
                    check(wait(lambda:(out/'browser.log').read_text().count('DOWNLOAD_ERROR')>errors_before),name+': interrupted text save reports an error');check(not target.exists(),name+': interrupted text save removes the partial file')
                else:check(wait(lambda:target.exists() and target.read_bytes()==gopher_text_body,seconds=60),name+': source save streams original encoding and dot-unescaped line endings')
            elif extension=='text':
                url=address('finger','/alice');navigate(w,url);check(wait(lambda:url in str(w.get_wm_name())),'Finger resource loads');focus(w);key(XK.XK_s,XK.XK_Control_L)
                if a.large_transfers and a.transfer_mode=='reset':
                    check(wait(lambda:(out/'browser.log').read_text().count('DOWNLOAD_ERROR')>errors_before),'Finger interrupted source save reports an error');check(not (downloads/'alice').exists(),'Finger interrupted source save removes its partial file')
                else:check(wait(lambda:(downloads/'alice').exists() and (downloads/'alice').read_bytes()==finger_body,seconds=60),'Finger Ctrl+S preserves original text encoding and line endings')
            else:
                url=address(scheme,'/'+name+'.bin','9');navigate(w,url,paint=False);save_title='Save '+('Gopher TLS' if scheme=='gophers' else scheme.capitalize())+' file';form=wait(lambda:next((item for item in windows() if item.get_wm_name()==save_title),None));check(form is not None,name+': binary navigation opens its native save form')
                if extension=='cancel':
                    point(form,530,613);check(wait(lambda:not any(item.get_wm_name()==save_title for item in windows())),name+': native cancel closes the save form')
                    if a.large_transfers:
                        if scheme=='guppy':
                            def released():
                                probe=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)
                                try:probe.bind(('127.0.0.1',records[since]['peer_port']));return True
                                except OSError:return False
                                finally:probe.close()
                            check(wait(released),name+': native cancel releases the UDP source port')
                        else:check(wait(lambda:any(r.get('closed_early') for r in records[since:])),name+': native cancel releases the pending transport')
                else:
                    point(form,642,130);chooser=wait(lambda:next((item for item in windows() if item.id not in [w.id,form.id]),None));check(chooser is not None,'Native binary destination chooser opens')
                    target=runtime/(name+'.bin' if a.large_transfers else 'chosen-response.bin');focus(chooser);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(str(target));key(XK.XK_Return)
                    check(wait(lambda:chooser.id not in [item.id for item in windows()]),'Native binary destination is selected');point(form,680,613);check(wait(lambda:target.exists() and target.read_bytes()==binary_body,seconds=60),'Binary navigation saves original file bytes')
                    if a.large_transfers:
                        check(wait(lambda:json.loads((profile/'session.json').read_text())['tabs'][json.loads((profile/'session.json').read_text())['active']]['url']==url and url in str(w.get_wm_name())),name+': completed save commits its original resource address')
                        focus(w);key(XK.XK_s,XK.XK_Control_L);source=downloads/(name+'.bin')
                        check(wait(lambda:source.exists() and source.read_bytes()==binary_body,seconds=60),name+': Ctrl+S streams the complete original resource')
            if a.large_transfers and (scheme=='gopher+' or (scheme=='gophers' and a.gophers_plus)) and extension!='cancel' and a.gopher_framing in ['count','dot'] and a.transfer_mode!='reset':
                check(wait(lambda:any(r.get('client_closed_after_frame') for r in records[since:])),name+': framed response completes while its server waits for client EOF')
            check(child.poll() is None,name+': browser remains running');runs.append({'name':name,'passed':True});print('PASS',name,flush=True)
        except BaseException:
            frame(w).save(folder/'failure-frame.png')
            if wayland:(folder/'failure-scene.json').write_text(json.dumps(wayland.scene(),indent=2)+'\n')
            raise
    if wayland:wayland.api('/api/action',{'op':'close','id':w.id})
    else:w.send_event(protocol.event.ClientMessage(window=w,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);connection.sync()
    check(not guppy.errors,'Guppy fixture completes without errors')
    check(child.wait(timeout=20)==0,'Browser closes cleanly after native image and download workflows');child=None
finally:
    if child is not None and child.poll() is None:child.kill();child.wait()
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    for server in servers:server.shutdown();server.server_close()
    (out/'report.json').write_text(json.dumps({'binary_sha256':sha(binary),'driver_sha256':driver_sha,'display_backend':'native-wayland' if a.wayland else 'private-x11','wayland_helper_sha256':sha(Path(__file__).with_name('private_wayland.py')) if a.wayland else None,'runs':runs,'checks':checks,'cases':[case[0] for case in cases] if 'cases' in globals() else [],'passed':bool(checks) and len(runs)==len(cases) and all(c['passed'] for c in checks),'large_transfers':a.large_transfers,'transfer_mode':a.transfer_mode,'gopher_framing':a.gopher_framing,'gophers_plus':a.gophers_plus,'transfer_bytes':len(binary_body),'scope':('Private 128 MiB native saves, Gopher+ framed/form responses and chooser workflows; preview limits remain separate. Mode: '+a.transfer_mode) if a.large_transfers else 'Private native protocol fixtures: raster/SVG pixels, original source saves, native binary Save Link, clipboard and clean close; no physical XR claim.'},indent=2)+'\n')
