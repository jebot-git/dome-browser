#!/usr/bin/env python3
"""Exercise native Gopher+ forms against isolated local fixtures."""
import argparse,hashlib,json,os,signal,socketserver,ssl,subprocess,sys,tempfile,threading,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_SMALLNET_FORMS_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_SMALLNET_FORMS_TEST_BUS':'1'})
from PIL import Image
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--gophers',action='store_true',help='Run Gopher+ forms over TLS with RFC layout')
p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--wayland',action='store_true')
all_cases=['controls','files','cancel','close','uri','uri-standard','uri-normalized','uri-slow','uri-cancel','reference','cross-reference','cross-reference-chain','reference-cancel','reference-precedence','reference-loop']
p.add_argument('--cases',nargs='+',choices=all_cases,default=all_cases)
a=p.parse_args();binary=a.binary.resolve();out=a.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
checks=[];runs=[];records=[];contacts=[];servers=[]
def sha(f):
    with f.open('rb') as s:return hashlib.file_digest(s,'sha256').hexdigest()
def check(value,name):
    checks.append({'name':name,'passed':bool(value)});assert value,name
class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True
class TLSServer(Server):
    def get_request(self):
        stream,peer=self.socket.accept();stream.settimeout(10)
        try:return self.context.wrap_socket(stream,server_side=True),peer
        except BaseException:stream.close();raise
class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(8)
        request=self.rfile.readline(4097);body=None
        if request.endswith(b'\t1\r\n'):
            header=self.rfile.readline(32);assert header.startswith(b'+');length=int(header[1:]);assert 0<=length<=8*1024*1024
            body=self.rfile.read(length);assert len(body)==length
        records.append((request,body))
        contacts.append((self.server.server_address[1],request.split(b'\t')[0].rstrip(b'\r\n'),body is not None))
        if request.startswith(b'/uri-slow\t') and body is not None:
            self.server.slow_started.set()
            self.connection.settimeout(15)
            try:
                if self.connection.recv(1)==b'':self.server.slow_closed.set()
            except OSError:pass
            return
        if self.server.kind=='nex':data=b'Replacement after form cancellation\n'
        elif b'!+ASK' in request:
            selector=request.split(b'\t')[0]
            port=self.server.server_address[1]
            if selector==b'/files':form=b' ChooseF: Upload a local file\t/server/default\r\n AskF: Save response\t../../response.bin\r\n'
            elif selector in [b'/reference',b'/cross-reference',b'/cross-reference-chain',b'/reference-cancel',b'/reference-precedence',b'/reference-loop']:
                target_port=self.server.reference_port if selector in [b'/cross-reference',b'/cross-reference-chain',b'/reference-cancel',b'/reference-precedence'] else port
                target='/loop' if selector==b'/reference-loop' else '/chained-definition' if selector==b'/cross-reference-chain' else '/definition'
                form=f' 0Referenced definition\t{target}\t127.0.0.1\t{target_port}\t+\r\n'.encode()
                if selector==b'/reference-precedence':form+=b' Ask: Literal definition\r\n'
                data=b'+-1\r\n+ASK:'+form+b'.\r\n';self.wfile.write(data);return
            else:form=b' Note: Local native form\r\n Ask: Name\r\n AskP: Secret\r\n Choose: Color\tRed\tBlue\r\n Select: Enabled:1\r\n AskL: Lines\r\n'
            data=b'+-1\r\n+ASK:\r\n'+form+b'.\r\n'
        else:
            if request.startswith(b'/chained-definition'):
                content=f'+ASK: 0Final definition\t/definition\t127.0.0.1\t{self.server.server_address[1]}\t+\r\n'.encode()
            elif request.startswith(b'/definition'):
                content=b'Ask: Referenced name\r\n'
            elif request.startswith(b'/loop'):
                content=f'+ASK: 0Loop\t/loop\t127.0.0.1\t{self.server.server_address[1]}\t+\r\n'.encode()
            else:content=b'Saved response\0\r\n' if request.startswith(b'/files') else b'Native form submitted successfully\r\n'
            data=b'+'+str(len(content)).encode()+b'\r\n'+content
        self.wfile.write(data)
def server(kind,context=None):
    s=(TLSServer if context else Server)(('127.0.0.1',0),Handler);s.context=context;s.kind=kind;s.slow_started=threading.Event();s.slow_closed=threading.Event();servers.append(s);threading.Thread(target=s.serve_forever,daemon=True).start();return s.server_address[1]
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
    time.sleep(.03)
def text(value):
    for c in value:
        symbol=XK.XK_Return if c=='\n' else XK.XK_Tab if c=='\t' else (XK.string_to_keysym(c) or ord(c))
        code=connection.keysym_to_keycode(symbol)
        shifted=symbol not in [XK.XK_Return,XK.XK_Tab] and connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
        key(symbol,XK.XK_Shift_L if shifted else None)
def click(w,x,y):
    w.set_input_focus(X.RevertToParent,X.CurrentTime);g=w.get_geometry()
    if wayland:
        xtest.fake_input(connection,X.MotionNotify,x=g.x+x-1,y=g.y+y);connection.sync();time.sleep(.03)
    xtest.fake_input(connection,X.MotionNotify,x=g.x+x,y=g.y+y)
    xtest.fake_input(connection,X.ButtonPress,1);connection.sync()
    if wayland:time.sleep(.05)
    xtest.fake_input(connection,X.ButtonRelease,1);connection.sync();time.sleep(.1)
def close(w):
    if wayland:wayland.api('/api/action',{'op':'close','id':w.id});return
    w.send_event(protocol.event.ClientMessage(window=w,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);connection.sync()
def capture(w,path):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX').save(path)
def navigate(w,url):
    if wayland:
        w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(url);key(XK.XK_Return);return
    # Xvfb has no window manager: explicitly bring the browser above the
    # nonmodal form before clicking its address bar.
    w.configure(stack_mode=X.Above);connection.sync();time.sleep(.2)
    click(w,w.get_geometry().width//2,55);key(XK.XK_a,XK.XK_Control_L);text(url);time.sleep(.15);key(XK.XK_Return)
def fill_controls(w,folder):
    click(w,180,184);text('local name')
    click(w,180,260);text('local secret')
    click(w,300,336);key(XK.XK_Down);key(XK.XK_Return)
    click(w,36,380)
    click(w,180,470);text('first\n.second')
    capture(w,folder/'native-form.png')
    click(w,680,613)
def pick_file(form,y,path,folder,label):
    click(form,642,y)
    chooser=wait(lambda:next((w for w in windows() if w.id!=form.id and 'file' in str(w.get_wm_name() or '').lower()),None),4)
    if chooser is None:
        chooser=next((w for w in windows() if w.id not in [form.id,browser_window().id]),None)
    check(chooser is not None,label+': native file chooser opened')
    capture(chooser,folder/(label+'-chooser.png'))
    chooser.set_input_focus(X.RevertToParent,X.CurrentTime);connection.sync();key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(str(path));key(XK.XK_Return)
    if not wait(lambda:form_window() and chooser.id not in [w.id for w in windows()],4):
        capture(chooser,folder/(label+'-chooser-pending.png'))
        raise AssertionError(label+': native chooser selection needs review')

xvfb=connection=child=wayland=None
try:
    with tempfile.TemporaryDirectory(prefix='dome-smallnet-forms-') as directory:
        runtime=Path(directory);context=None
        if a.gophers:
            cert=runtime/'server.pem';private=runtime/'server.key'
            subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','30','-sha256','-subj','/CN=localhost','-out',str(cert),'-keyout',str(private)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(cert,private)
        plus=server('gopher',context);nex=server('nex');reference_port=server('gopher',context)
        for s in servers:s.reference_port=reference_port
        rd,wr=os.pipe()
        xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1440x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
        with os.fdopen(rd) as f:screen=':'+f.readline().strip()
        connection=display.Display(screen)
        if a.wayland:
            from private_wayland import PrivateWayland
            wayland=PrivateWayland(connection,screen,runtime,out)
        env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','HOME':directory,'XDG_RUNTIME_DIR':directory,'XDG_CONFIG_HOME':str(runtime/'config'),'XDG_DATA_HOME':str(runtime/'data'),'XDG_CACHE_HOME':str(runtime/'cache'),'XDG_STATE_HOME':str(runtime/'state')}
        for k in list(env):
            if k in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','MOZ_GCTIMER','DOME_BROWSER_GC_PROFILE'] or k.startswith(('DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE','DOME_BROWSER_MEMORY_CAPTURE')):env.pop(k,None)
        if wayland:env.pop('DISPLAY',None);env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket)
        for name in a.cases:
            folder=out/name;folder.mkdir(mode=0o700);profile=runtime/name;before=len(records);contact_before=len(contacts)
            endpoint='uri-slow' if name=='uri-slow' else 'uri' if name.startswith('uri') else name if 'reference' in name or name=='files' else 'form'
            target=f'{"gophers" if a.gophers else "gopher+"}://127.0.0.1:{plus}/0/{endpoint}%09%09%3F'
            if name.startswith('uri'):
                scheme='gophers' if a.gophers else 'gopher' if name=='uri-standard' else 'gopher+'
                target=f'{scheme}://127.0.0.1:{plus}/0/{endpoint}%09%09%2B%091%0D%0A%2B%2D1%0D%0Asupplied%20value%0D%0A.%0D%0A'
                if name=='uri-normalized':target=target.replace('127.0.0.1','%31%32%37.0.0.1')
            with (folder/'browser.log').open('w') as log:
                child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log)
                try:
                    title='Review Gopher+ data' if name.startswith('uri') else 'Review Gopher+ form source' if name in ['cross-reference','cross-reference-chain','reference-cancel'] else 'Gopher+ form'
                    form=wait(lambda:form_window(title),30) if name!='reference-loop' else None
                    if name!='reference-loop':check(form is not None,name+': native form opened')
                    browser=wait(browser_window,30);check(browser is not None,name+': browser remains available')
                    if wayland and form:check(wait(lambda:any(popup['id']==form.id and popup['root']==browser.id for popup in wayland.scene()['popups'])),name+': native prompt stays attached to its browser')
                    check(not any(body is not None for _,body in records[before:]),name+': opening a form does not submit answers')
                    if name=='controls':
                        fill_controls(form,folder)
                        expected=b'local name\r\nlocal secret\r\nBlue\r\n0\r\n2\r\nfirst\r\n.second\r\n'
                        check(wait(lambda:any(request==(b'/form\t\t+\t1\r\n' if a.gophers else b'/form\t+\t1\r\n') and body==expected for request,body in records[before:])), 'Native controls submit ordered, correctly encoded answers')
                        check(wait(lambda:target in str(browser.get_wm_name() or '')),'Submitted response reaches the original native address')
                        time.sleep(.3);capture(browser,folder/'submitted.png')
                        browser.set_input_focus(X.RevertToParent,X.CurrentTime);connection.sync();key(XK.XK_r,XK.XK_Control_L)
                        reloaded=wait(form_window);check(reloaded is not None,'Reload requests a fresh native form')
                        time.sleep(.2);check(sum(body is not None for _,body in records[before:])==1,'Reload does not resubmit prior answers')
                        close(reloaded)
                    elif name=='files':
                        upload=runtime/'upload.bin';upload.write_bytes(b'local binary\0\r\n.\r\n');destination=runtime/'response.bin'
                        pick_file(form,130,upload,folder,'upload')
                        pick_file(form,206,destination,folder,'save')
                        capture(form,folder/'native-form.png');click(form,680,613)
                        expected=b'response.bin\r\n'+upload.read_bytes()
                        check(wait(lambda:any(body==expected for _,body in records[before:])),'Explicitly selected file is sent after answer lines')
                        check(wait(destination.exists),'Selected response destination was saved')
                        check(destination.read_bytes()==b'Saved response\0\r\n','Saved response preserves binary bytes')
                        check(destination.stat().st_mode&0o777==0o600,'Saved response is private')
                        capture(browser,folder/'saved.png')
                    elif name=='cancel':
                        capture(form,folder/'native-form.png')
                        navigate(browser,f'nex://127.0.0.1:{nex}/replacement.txt')
                        check(wait(lambda:form_window() is None),'Navigation closes the cancelled native form')
                        check(wait(lambda:'replacement.txt' in str(browser.get_wm_name() or '')),'Navigation completes while a form was open')
                        check(not any(body is not None for _,body in records[before:]),'Navigation cancellation sends no form answers')
                    elif name=='close':
                        close(browser);check(child.wait(timeout=10)==0,'Closing the browser with an open form exits cleanly')
                        check(not any(body is not None for _,body in records[before:]),'Browser close sends no form answers')
                    elif name.startswith('uri'):
                        check(not contacts[contact_before:],'URI data remains local until native review is accepted')
                        capture(form,folder/'native-review.png')
                        if name=='uri-cancel':
                            close(form);navigate(browser,f'nex://127.0.0.1:{nex}/replacement.txt')
                            check(wait(lambda:'replacement.txt' in str(browser.get_wm_name() or '')),'Declined URI data permits ordinary replacement navigation')
                            check(not any(body is not None for _,body in records[before:]),'Declining URI data sends no answers')
                        else:
                            click(form,680,613)
                            check(wait(lambda:any(body==b'supplied value\r\n' for _,body in records[before:])),'Reviewed URI data is submitted with preserved bytes')
                            if name=='uri-slow':
                                slow=next(s for s in servers if s.server_address[1]==plus)
                                check(slow.slow_started.wait(4),'Submitted URI fixture begins an incomplete response')
                                navigate(browser,f'nex://127.0.0.1:{nex}/replacement.txt')
                                check(wait(lambda:'replacement.txt' in str(browser.get_wm_name() or '')),'Pending submitted response can be replaced by ordinary navigation')
                                check(slow.slow_closed.wait(4) and child.poll() is None,'Replacement navigation releases the pending submitted connection')
                                check(sum(body is not None for _,body in records[before:])==1,'Cancellation does not repeat submitted URI data')
                            else:
                                scheme='gophers' if a.gophers else 'gopher' if name=='uri-standard' else 'gopher+'
                                clean=f'{scheme}://127.0.0.1:{plus}/0%2Furi%09%09%3F'
                                check(wait(lambda:clean in str(browser.get_wm_name() or '')),'Reviewed URI result uses a clean address without supplied data')
                                capture(browser,folder/'submitted.png')
                                if not wayland:browser.configure(stack_mode=X.Above)
                                browser.set_input_focus(X.RevertToParent,X.CurrentTime);connection.sync();key(XK.XK_r,XK.XK_Control_L)
                                reloaded=wait(form_window);check(reloaded is not None,'Reload of a supplied URI requests a fresh form')
                                check(sum(body is not None for _,body in records[before:])==1,'Reload does not repeat URI-supplied data');close(reloaded)
                    elif name=='reference-loop':
                        check(wait(lambda:any(selector==b'/loop' for _,selector,_ in contacts[contact_before:])),'Referenced definition fixture begins')
                        time.sleep(.4)
                        check(sum(selector==b'/loop' for _,selector,_ in contacts[contact_before:])==1,'Repeated definition references terminate after the first repeated target')
                        check(not any(body is not None for _,body in records[before:]),'Reference loops submit no answers')
                        check(child.poll() is None,'Browser survives a form-definition loop')
                    elif 'reference' in name:
                        if name in ['cross-reference','cross-reference-chain','reference-cancel']:
                            check(not any(port==reference_port for port,_,_ in contacts[contact_before:]),'A different definition server is not contacted before native approval')
                            capture(form,folder/'native-review.png')
                            if name=='reference-cancel':
                                close(form);navigate(browser,f'nex://127.0.0.1:{nex}/replacement.txt')
                                check(wait(lambda:'replacement.txt' in str(browser.get_wm_name() or '')),'Declined definition permits ordinary replacement navigation')
                                check(not any(port==reference_port for port,_,_ in contacts[contact_before:]),'Declined definition source is never contacted')
                            else:
                                click(form,680,613);form=wait(form_window);check(form is not None,'Approved referenced definition opens its native controls')
                        if name!='reference-cancel':
                            if name=='reference-precedence':check(not any(port==reference_port for port,_,_ in contacts[contact_before:]),'Literal ASK fields take precedence without contacting the reference')
                            click(form,180,130);text('reference answer');capture(form,folder/'native-form.png');click(form,680,613)
                            check(wait(lambda:any(request.startswith(('/'+endpoint).encode()+b'\t') and body==b'reference answer\r\n' for request,body in records[before:])),name+': answers go to the original form target')
                            check(not any(port==reference_port and submitted for port,_,submitted in contacts[contact_before:]),'Definition-only server receives no submitted answers')
                            check(wait(lambda:target in str(browser.get_wm_name() or '')),name+': response retains the original address')
                    if child.poll() is None:
                        close(browser);check(child.wait(timeout=10)==0,name+': clean browser close')
                    for file in profile.rglob('*'):
                        if file.is_file() and file.suffix in ['.txt','.json']:
                            check(b'local secret' not in file.read_bytes(),name+': submitted secret absent from '+file.name)
                            if name.startswith('uri'):
                                check(b'supplied%20value' not in file.read_bytes() and b'supplied value' not in file.read_bytes(),name+': URI-supplied data absent from '+file.name)
                    runs.append({'name':name,'passed':True});print('PASS',name,flush=True)
                finally:
                    if sys.exc_info()[0] is not None and wayland:
                        (folder/'failure-scene.json').write_text(json.dumps(wayland.scene(),indent=2)+'\n')
                        for window in windows():capture(window,folder/f'failure-window-{window.id}.png')
                    if child.poll() is None:child.kill();child.wait()
                    child=None
finally:
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    for s in servers:s.shutdown();s.server_close()
    report={'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'display_backend':'native-wayland' if wayland else 'private-x11','wayland_helper_sha256':sha(Path(__file__).with_name('private_wayland.py')) if wayland else None,'checks':checks,'runs':runs,'cases':a.cases,'passed':len(runs)==len(a.cases) and all(c['passed'] for c in checks),'tls':a.gophers,'scope':'Local native controls, explicit file handoff, reload, cancellation and orderly close; no full protocol conformance claim.'}
    (out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
