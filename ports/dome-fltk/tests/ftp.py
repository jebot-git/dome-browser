#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Read-only FTP browser workflows using private local servers and an X server."""
import argparse,hashlib,json,os,re,signal,socket,socketserver,ssl,subprocess,sys,tempfile,threading,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_FTP_TEST_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_FTP_TEST_BUS':'1'})
from PIL import Image,ImageChops
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--wayland',action='store_true')
cases=['directory','directory-link','directory-no-slash','plain','type-a','type-i','type-d','encoded-path','empty-component','names-fallback','passive-fallback','auth','uri-user','uri-password','account','binary','save-cancel','server-failure','size-limit','short-transfer','cancel','stream-save','stream-no-size','stream-slow','stream-short','stream-final-error','stream-cancel','stream-save-link','stream-redirect']
p.add_argument('--cases',nargs='+',choices=cases,default=cases);a=p.parse_args();driver_sha=hashlib.sha256(Path(__file__).read_bytes()).hexdigest();binary=a.binary.resolve();out=a.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
checks=[];runs=[];records=[];child=connection=xvfb=wayland=None;servers=[]
stream_cases=[name for name in cases if name.startswith('stream-')]
large_size=128*1024*1024;large_chunk=bytes(range(256))*256
large_sha=hashlib.sha256(large_chunk*(large_size//len(large_chunk))).hexdigest()
stream_observations=[]
body='FTP local fixture\nUnicode café 世界\n'.encode();binary_body=b'\0FTP binary fixture\xff\r\n.\r\n'
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def check(ok,label):checks.append({'name':label,'passed':bool(ok)});assert ok,label
class Server(socketserver.ThreadingTCPServer):allow_reuse_address=True;daemon_threads=True
class FTP(socketserver.StreamRequestHandler):
 def handle(self):
    self.connection.settimeout(120);listener=None;user='';kind='I';mode=self.server.mode
    def send(code,text):self.wfile.write(f'{code} {text}\r\n'.encode());self.wfile.flush()
    self.wfile.write(b'220-Local FTP fixture\r\nFixture continuation\r\n220 Ready\r\n');self.wfile.flush()
    try:
     while True:
        line=self.rfile.readline(16385)
        if not line:return
        assert line.endswith(b'\r\n');command=line[:-2];verb,_,arg=command.partition(b' ');verb=verb.decode('ascii')
        records.append((mode,verb,b'(provided)' if verb=='PASS' else arg))
        if verb=='USER':
            user=arg.decode();send(332 if mode=='account' and user=='anonymous' else 530 if mode in ['auth','account'] and user=='anonymous' else 331,'Credentials')
        elif verb=='PASS':
            accepted=mode not in ['auth','uri-user','uri-password','account'] or (user=='fixture' and arg==b'local secret')
            send(332 if accepted and mode=='account' else 230 if accepted else 530,'Account result')
        elif verb=='ACCT':send(230 if arg==b'local-account' else 530,'Account result')
        elif verb=='FEAT':self.wfile.write(b'211-Features\r\n UTF8\r\n MLST type*;size*;modify*;\r\n211 End\r\n');self.wfile.flush()
        elif verb=='OPTS':send(200,'UTF8 selected')
        elif verb=='TYPE':kind=arg.decode();send(200,'Type selected')
        elif verb=='CWD':send(550 if arg==b'missing.txt' else 250,'Directory result')
        elif verb=='SIZE':
            if mode in ['stream-no-size','stream-cancel']:send(500,'Size not available')
            elif mode in stream_cases:send(213,str(large_size))
            elif mode=='size-limit':send(213,str(8*1024*1024+1))
            elif mode=='cancel':send(500,'Size not supported')
            elif mode in ['directory-no-slash','server-failure']:send(550,'Not a file')
            else:send(213,str((len(binary_body) if mode in ['binary','save-cancel'] else len(body))+(3 if mode=='short-transfer' else 0)))
        elif verb in ['EPSV','PASV']:
            if verb=='EPSV' and mode=='passive-fallback':send(502,'Use passive mode');continue
            if listener:listener.close()
            listener=socket.socket();listener.bind(('127.0.0.1',0));listener.listen(1);listener.settimeout(10);port=listener.getsockname()[1]
            send(229,f'Entering extended passive mode (|||{port}|)') if verb=='EPSV' else send(227,f'Entering passive mode (127,0,0,2,{port//256},{port%256})')
        elif verb in ['MLSD','NLST','RETR']:
            if verb=='MLSD' and mode=='names-fallback':send(502,'Use names');continue
            if verb=='RETR' and mode in ['directory-no-slash','server-failure']:send(550,'Requested file unavailable');continue
            assert listener is not None;send(150,'Opening data connection');data,peer=listener.accept();data.settimeout(10);check(peer[0]=='127.0.0.1','FTP data client remains on the private fixture')
            try:
                if mode=='cancel' and verb=='RETR':
                    data.sendall(b'Pending FTP response');self.server.started.set()
                    if data.recv(1)==b'':self.server.data_closed.set()
                    if self.connection.recv(1)==b'':self.server.control_closed.set()
                    return
                if mode in stream_cases and verb=='RETR':
                    limit=large_size//len(large_chunk) if mode not in ['stream-short','stream-cancel'] else 128 if mode=='stream-short' else 4
                    for _ in range(limit):
                        data.sendall(large_chunk)
                        if mode=='stream-slow':time.sleep(.012)
                    self.server.started.set()
                    if mode=='stream-cancel':
                        if data.recv(1)==b'':self.server.data_closed.set()
                        if self.connection.recv(1)==b'':self.server.control_closed.set()
                        return
                elif mode=='stream-save-link' and verb=='MLSD':payload=b'Type=file;Size=134217728; fixture.bin\r\n'
                elif verb=='MLSD':payload=b'Type=cdir; .\r\nType=pdir; ..\r\nType=file;Size=31;Modify=20261025120000; readme.txt\r\nType=dir; child\r\nType=file; caf\xc3\xa9.txt\r\nType=file; less<than>.txt\r\n'
                elif verb=='NLST':payload=b'readme.txt\r\ncaf\xc3\xa9.txt\r\n'
                else:payload=binary_body if mode in ['binary','save-cancel'] else body
                if mode not in stream_cases or verb!='RETR':data.sendall(payload)
            finally:data.close()
            send(451 if mode=='stream-final-error' else 226,'Transfer complete')
            if mode in stream_cases:self.server.finished.set()
        elif verb=='QUIT':send(221,'Goodbye');return
        else:raise AssertionError('Unexpected command in read-only fixture')
    except (BrokenPipeError,ConnectionResetError,socket.timeout):pass
    finally:
        if listener:listener.close()
class Nex(socketserver.StreamRequestHandler):
 def handle(self):
    path=self.rfile.readline(4096).strip()
    if path==b'/index/' and self.server.ftp.mode=='stream-redirect':self.wfile.write(f'=> gemini://localhost:{self.server.gemini_port}/jump.bin Redirected large file\r\n'.encode())
    else:self.wfile.write(b'Replacement native page\r\n')
class TLSRedirectServer(Server):
 def get_request(self):
    stream,peer=self.socket.accept();stream.settimeout(10)
    try:return self.context.wrap_socket(stream,server_side=True),peer
    except BaseException:stream.close();raise
class GeminiRedirect(socketserver.StreamRequestHandler):
 def handle(self):
    self.rfile.readline(4096)
    self.wfile.write(f'30 ftp://127.0.0.1:{self.server.ftp.server_address[1]}/fixture.bin\r\n'.encode())
def windows():
 if wayland:return wayland.windows()
 result=[]
 for w in connection.screen().root.query_tree().children:
    try:
        if w.get_attributes().map_state==X.IsViewable:result.append(w)
    except Exception:pass
 return result
def window(title=None):
 if wayland and title is None:
    candidates=[w for w in windows() if w.value()['app_id']=='org.domed.Browser' and w.get_wm_name() not in ['FTP account','Save FTP file']]
    return max(candidates,key=lambda w:w.get_geometry().width,default=None)
 return next((w for w in windows() if (str(w.get_wm_name() or '')==title if title else w.get_geometry().width>=1000)),None)
def wait(fn,seconds=12):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
    value=fn()
    if value:return value
    time.sleep(.05)
 return None
def key(symbol,mod=None):
 if mod:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(mod))
 code=connection.keysym_to_keycode(symbol);xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
 if mod:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(mod))
 connection.sync();time.sleep(.02)
def text(value):
 for c in value:
    symbol=XK.string_to_keysym(c) or ord(c);code=connection.keysym_to_keycode(symbol)
    shift=connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
    key(symbol,XK.XK_Shift_L if shift else None)
def click(w,x,y):
 w.set_input_focus(X.RevertToParent,X.CurrentTime);g=w.get_geometry()
 if wayland:
    xtest.fake_input(connection,X.MotionNotify,x=g.x+x-1,y=g.y+y);connection.sync();time.sleep(.03)
 xtest.fake_input(connection,X.MotionNotify,x=g.x+x,y=g.y+y);xtest.fake_input(connection,X.ButtonPress,1);connection.sync();time.sleep(.04);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync();time.sleep(.15)
def close(w):
 if wayland:wayland.api('/api/action',{'op':'close','id':w.id});return
 w.send_event(protocol.event.ClientMessage(window=w,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);connection.sync()
def frame(w):
 g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff);return Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
def painted(w):
 image=frame(w).crop((20,145,w.get_geometry().width-20,min(650,w.get_geometry().height-30)))
 colors=image.getcolors(image.width*image.height)
 if not colors:return False
 background=max(colors)[1]
 # A pointer or a window border is not evidence that document text painted.
 return sum(count for count,color in colors if max(abs(a-b) for a,b in zip(color,background))>80)>500
def navigate(w,url):
 if wayland:
    w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L)
 else:click(w,480,55)
 key(XK.XK_a,XK.XK_Control_L);text(url);key(XK.XK_Return)
def link_position(w):
 image=frame(w);r,g,b=image.split();mask=ImageChops.multiply(ImageChops.subtract(b,r).point(lambda v:255 if v>=20 else 0),ImageChops.subtract(b,g).point(lambda v:255 if v>=20 else 0));pixels=mask.tobytes()
 for y in range(100,min(600,image.height-30)):
    match=re.search(b'\xff{12,}',pixels[y*image.width+8:(y+1)*image.width-8])
    if match:return ((match.start()+match.end())//2+8,y-3)
 return None
def first_link(w):
 position=link_position(w)
 if position:click(w,*position);return True
 return False

try:
 with tempfile.TemporaryDirectory(prefix='dome-ftp-') as directory:
    runtime=Path(directory);runtime.chmod(0o700);ftp=Server(('127.0.0.1',0),FTP);nex=Server(('127.0.0.1',0),Nex);gemini=TLSRedirectServer(('127.0.0.1',0),GeminiRedirect)
    certificate=runtime/'server.pem';key_file=runtime/'server.key';subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','30','-sha256','-subj','/CN=localhost','-addext','subjectAltName=DNS:localhost','-out',str(certificate),'-keyout',str(key_file)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    gemini.context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);gemini.context.minimum_version=ssl.TLSVersion.TLSv1_2;gemini.context.load_cert_chain(certificate,key_file);nex.ftp=ftp;gemini.ftp=ftp;nex.gemini_port=gemini.server_address[1];servers=[ftp,nex,gemini]
    for s in servers:threading.Thread(target=s.serve_forever,daemon=True).start()
    ftp.started=threading.Event();ftp.finished=threading.Event();ftp.data_closed=threading.Event();ftp.control_closed=threading.Event()
    read,write=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(write),'-screen','0','1440x1000x24','-nolisten','tcp'],pass_fds=(write,),stderr=subprocess.DEVNULL);os.close(write)
    with os.fdopen(read) as f:screen=':'+f.readline().strip()
    connection=display.Display(screen)
    if a.wayland:
        from private_wayland import PrivateWayland
        wayland=PrivateWayland(connection,screen,runtime,out)
    for name in a.cases:
        folder=out/name;folder.mkdir(mode=0o700);case=runtime/name;case.mkdir(mode=0o700);profile=case/'profile';profile.mkdir(mode=0o700);ftp.mode=name;before=len(records);ftp.started.clear();ftp.finished.clear();ftp.data_closed.clear();ftp.control_closed.clear()
        path='/' if name in ['directory','directory-link','names-fallback','passive-fallback','stream-save-link'] else '/pub' if name=='directory-no-slash' else '/parent/pattern;type=d' if name=='type-d' else '/%2Fpub/a%20b.txt' if name=='encoded-path' else '//pub/readme.txt' if name=='empty-component' else '/missing.txt' if name=='server-failure' else '/fixture.bin' if name in ['binary','save-cancel'] or name in stream_cases else '/readme.txt'
        if name=='type-a':path+=';type=a'
        if name=='type-i':path+=';type=i'
        public=f'nex://127.0.0.1:{nex.server_address[1]}/index/' if name=='stream-redirect' else f'ftp://127.0.0.1:{ftp.server_address[1]}'+path;target=public
        if name in ['uri-user','uri-password']:target=public.replace('ftp://','ftp://fixture'+(':local%20secret' if name=='uri-password' else '')+'@')
        env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','HOME':str(case),'XDG_RUNTIME_DIR':str(runtime),'XDG_DATA_HOME':str(case/'data'),'XDG_CONFIG_HOME':str(case/'config'),'XDG_CACHE_HOME':str(case/'cache'),'XDG_STATE_HOME':str(case/'state'),'RUST_LOG':'warn','DOME_BROWSER_TRACE':'1'}
        for k in list(env):
            if k in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','MOZ_GCTIMER','DOME_BROWSER_GC_PROFILE'] or k.startswith(('DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE','DOME_BROWSER_MEMORY_CAPTURE')):env.pop(k,None)
        if wayland:
            env.pop('DISPLAY',None);env.update(WAYLAND_DISPLAY=wayland.socket,FLTK_BACKEND='wayland')
        with (folder/'browser.log').open('w') as log:
            child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),f'nex://127.0.0.1:{nex.server_address[1]}/warmup.txt' if name in stream_cases else target],env=env,stdout=log,stderr=log)
            try:
                browser=wait(window,seconds=30);check(browser is not None,name+': browser opens')
                if name in stream_cases:
                    check(wait(lambda:painted(browser)),'A small native page paints before transfer measurement');time.sleep(1);navigate(browser,target)
                if name in ['auth','uri-user','account']:
                    form=wait(lambda:window('FTP account'));check(form is not None,name+': native credential prompt opens')
                    if name=='uri-user':check(not any(v=='PASS' for _,v,_ in records[before:]),'A named URI without a password prompts before sending PASS')
                    click(form,180,130);key(XK.XK_a,XK.XK_Control_L);text('fixture');click(form,180,206);text('local secret')
                    if name=='account':click(form,180,282);text('local-account')
                    frame(form).save(folder/'native-account.png');click(form,680,613)
                if name in ['binary','save-cancel'] or name in stream_cases and name not in ['stream-save-link','stream-redirect']:
                    form=wait(lambda:window('Save FTP file'));check(form is not None,name+': file saving requires a native destination choice')
                    destination=case/'chosen.bin';destination.write_bytes(b'Previous local file')
                    if name=='save-cancel':close(form);time.sleep(.2);check(destination.read_bytes()==b'Previous local file','Canceling a save leaves the destination unchanged')
                    else:
                        click(form,642,130);chooser=wait(lambda:next((w for w in windows() if w.id not in [browser.id,form.id]),None));check(chooser is not None,'Native save chooser opens')
                        chooser.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(str(destination));key(XK.XK_Return)
                        # The chooser confirms replacing an existing fixture file.
                        time.sleep(.3)
                        confirmation=next((w for w in windows() if w.id not in [browser.id,form.id,chooser.id]),None)
                        if confirmation:confirmation.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_Return)
                        check(wait(lambda:chooser.id not in [w.id for w in windows()]),'Explicit save destination selected')
                        samples=[];stages=[];stop=threading.Event()
                        def sample():
                            while not stop.is_set():
                                try:
                                    status=Path(f'/proc/{child.pid}/status').read_text();samples.append({'time':time.monotonic(),'rss':int(re.search(r'VmRSS:\s+(\d+)',status).group(1))*1024,'anonymous':int(re.search(r'RssAnon:\s+(\d+)',status).group(1))*1024,'file':int(re.search(r'RssFile:\s+(\d+)',status).group(1))*1024})
                                except (OSError,AttributeError):pass
                                stop.wait(.01)
                        sampler=threading.Thread(target=sample,daemon=True);sampler.start();started=time.monotonic();click(form,680,613)
                        try:
                            if name=='stream-cancel':
                                check(ftp.started.wait(10),'Streaming FTP has begun receiving data')
                                check(wait(lambda:bool(list(case.glob('.dome-smallnet-*')))),'Streaming save uses a pending local file')
                                close(browser);check(child.wait(timeout=10)==0,'Browser closes normally during a streaming save')
                                check(ftp.data_closed.wait(4) and ftp.control_closed.wait(4),'Closing a streaming save releases both connections')
                                check(destination.read_bytes()==b'Previous local file' and not list(case.glob('.dome-smallnet-*')),'Canceled streaming save preserves the destination and removes its partial file')
                            elif name in ['stream-short','stream-final-error']:
                                check(ftp.finished.wait(35),'FTP fixture completes its terminal response')
                                check(wait(lambda:not list(case.glob('.dome-smallnet-*'))),'Failed streaming save removes its partial file')
                                check(destination.read_bytes()==b'Previous local file','Failed streaming save preserves the previous destination')
                            elif name in stream_cases:
                                check(wait(lambda:destination.exists() and destination.stat().st_size==large_size,seconds=60),'Large native save reaches its complete size')
                                stages.append({'stage':'native-save-complete','memory':samples[-1] if samples else None});check(sha(destination)==large_sha,'Large native save preserves every byte')
                                check(destination.stat().st_mode&0o777==0o600,'Large saved file has private permissions')
                                check(ftp.finished.wait(5),'Large save consumes the FTP completion reply')
                                if name=='stream-save':
                                    check(wait(lambda:public in str(browser.get_wm_name())),'Completed binary save retains the resource URI')
                                    key(XK.XK_s,XK.XK_Control_L);download=case/'Downloads'/'fixture.bin'
                                    check(wait(lambda:download.exists() and download.stat().st_size==large_size,seconds=60),'Ctrl+S streams the original large native resource')
                                    stages.append({'stage':'source-save-complete','memory':samples[-1] if samples else None});check(sha(download)==large_sha,'Ctrl+S preserves the complete large resource bytes')
                                if name=='stream-slow':check(time.monotonic()-started>20,'Progressing transfer continues beyond the former whole-load deadline')
                            else:
                                check(wait(lambda:destination.read_bytes()==binary_body),'Binary retrieval preserves bytes at the selected destination')
                                check(destination.stat().st_mode&0o777==0o600,'Saved FTP file has private permissions')
                        finally:
                            stop.set();sampler.join(timeout=2)
                            if name in stream_cases and samples:
                                delta=max(item['rss'] for item in samples)-samples[0]['rss'];stream_observations.append({'case':name,'size':large_size,'initial_rss':samples[0]['rss'],'peak_rss':max(item['rss'] for item in samples),'rss_growth':delta,'initial_anonymous':samples[0]['anonymous'],'peak_anonymous':max(item['anonymous'] for item in samples),'anonymous_growth':max(item['anonymous'] for item in samples)-samples[0]['anonymous'],'stages':stages,'elapsed_seconds':time.monotonic()-started})
                                if name not in ['stream-short','stream-cancel']:check(delta<64*1024*1024,'Large transfer avoids retaining a complete 128 MiB body')

                if name in ['stream-save-link','stream-redirect']:
                    check(wait(lambda:public in str(browser.get_wm_name()) and painted(browser)),'Large file directory paints');time.sleep(.4);click(browser,8,180);frame(browser).save(folder/'index-before-context.png')
                    position=wait(lambda:link_position(browser))
                    check(position is not None,'Directory exposes the large native file link');browser.set_input_focus(X.RevertToParent,X.CurrentTime);g=browser.get_geometry();xtest.fake_input(connection,X.MotionNotify,x=g.x+position[0]-1,y=g.y+position[1]);connection.sync();time.sleep(.03);xtest.fake_input(connection,X.MotionNotify,x=g.x+position[0],y=g.y+position[1]);xtest.fake_input(connection,X.ButtonPress,3);connection.sync();time.sleep(.04);xtest.fake_input(connection,X.ButtonRelease,3);connection.sync();time.sleep(.4)
                    xtest.fake_input(connection,X.MotionNotify,x=2,y=2);connection.sync();time.sleep(.1);key(XK.XK_Tab,XK.XK_Shift_L);key(XK.XK_Tab,XK.XK_Shift_L);key(XK.XK_Return)
                    download=case/'Downloads'/'fixture.bin';check(wait(lambda:download.exists() and download.stat().st_size==large_size,seconds=60),'Save Link streams a large native resource')
                    check(sha(download)==large_sha,'Save Link preserves every large-file byte')
                    check(json.loads((profile/'session.json').read_text())['tabs'][0]['url']==public,'Large Save Link keeps the directory open')
                if name=='cancel':
                    check(ftp.started.wait(10),'FTP fixture begins a pending transfer');replacement=f'nex://127.0.0.1:{nex.server_address[1]}/replacement.txt';navigate(browser,replacement)
                    check(wait(lambda:replacement in str(browser.get_wm_name())),'New navigation replaces the pending FTP load')
                    check(ftp.data_closed.wait(4) and ftp.control_closed.wait(4) and child.poll() is None,'Navigation releases both FTP connections before browser exit')
                elif name not in ['size-limit','short-transfer','save-cancel','stream-short','stream-final-error','stream-cancel']:
                    check(wait(lambda:public in str(browser.get_wm_name())),'FTP result uses a public URL without credentials')
                    check(wait(lambda:painted(browser)),name+': document visibly paints')
                    frame(browser).save(folder/'frame.png')
                    if name=='directory-link':
                        check(wait(lambda:first_link(browser)),'Directory exposes a native protocol link');check(wait(lambda:any(v=='RETR' and arg==b'readme.txt' for _,v,arg in records[before:])),'Selecting a directory entry retrieves that file')
                elif name!='stream-cancel':time.sleep(.4);check(child.poll() is None,name+': bounded or canceled retrieval leaves browser running')
                local=records[before:];commands=[(v,arg) for _,v,arg in local]
                check(all(v in ['USER','PASS','ACCT','FEAT','OPTS','TYPE','CWD','SIZE','EPSV','PASV','MLSD','NLST','RETR','QUIT'] for v,_ in commands),'FTP operations remain within the read-only command set')
                if name=='type-a':check(('TYPE',b'A') in commands,'ASCII URI transfer type is honored')
                if name=='type-i':check(('TYPE',b'I') in commands,'Binary URI transfer type is honored')
                if name=='type-d':check(('CWD',b'parent') in commands and ('NLST',b'pattern') in commands and ('CWD',b'pattern') not in commands,'Directory URI passes its final name to NLST')
                if name=='encoded-path':check(('CWD',b'/pub') in commands and ('RETR',b'a b.txt') in commands,'Encoded path components map to separate FTP arguments')
                if name=='empty-component':check(('CWD',b'') in commands and ('CWD',b'pub') in commands,'Empty intermediate components retain the RFC CWD argument')
                if name=='names-fallback':check(any(v=='NLST' for v,_ in commands),'Older servers use the names-list fallback')
                if name=='passive-fallback':check(any(v=='PASV' for v,_ in commands),'IPv4 passive fallback stays on the control peer')
                if name=='size-limit':check(not any(v=='RETR' for v,_ in commands),'An announced oversized file is rejected before retrieval')
                if child.poll() is None:close(browser)
                check(child.wait(timeout=10)==0,name+': browser closes cleanly');child=None
                for file in profile.rglob('*'):
                    if file.is_file() and file.suffix in ['.json','.txt']:check(b'local secret' not in file.read_bytes() and b'local%20secret' not in file.read_bytes(),name+': credentials absent from '+file.name)
                runs.append({'name':name,'commands':[v for v,_ in commands],'passed':True});print('PASS',name,flush=True)
            except BaseException:
                if browser is not None and child is not None and child.poll() is None:frame(browser).save(folder/'failure-frame.png')
                raise
            finally:
                if wayland:
                    (folder/'scene-at-close.json').write_text(json.dumps(wayland.scene(),indent=2)+'\n')
                if child is not None and child.poll() is None:child.kill();child.wait()
                child=None
finally:
 if wayland:wayland.close()
 if connection:connection.close()
 if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
 for server in servers:server.shutdown();server.server_close()
 (out/'report.json').write_text(json.dumps({'binary_sha256':sha(binary),'driver_sha256':driver_sha,'display_backend':'native-wayland' if a.wayland else 'private-x11','wayland_helper_sha256':sha(Path(__file__).with_name('private_wayland.py')) if a.wayland else None,'cases':a.cases,'streaming_observations':stream_observations,'runs':runs,'checks':checks,'passed':len(runs)==len(a.cases) and all(c['passed'] for c in checks),'scope':'Read-only FTP private fixtures: directory and file retrieval, URL types, fallback modes, credentials, explicit binary saves, 128 MiB streaming, original-source/Save Link, failed or canceled transfers, preview bounds and clean exit.'},indent=2)+'\n')
