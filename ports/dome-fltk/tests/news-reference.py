#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Browser interoperability with a private independently packaged INN server."""
import argparse,hashlib,json,os,re,signal,socketserver,ssl,subprocess,sys,tempfile,threading,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
from PIL import Image,ImageChops
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,required=True);p.add_argument('--root',type=Path,required=True);p.add_argument('--state',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--wayland',action='store_true')
a=p.parse_args();binary=a.binary.resolve();root=a.root.resolve();state=a.state.resolve();out=a.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
checks=[];runs=[];processes=[];servers=[];connection=xvfb=wayland=child=None
args=a
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

class Server(socketserver.ThreadingTCPServer):allow_reuse_address=True;daemon_threads=True
class Backend(socketserver.BaseRequestHandler):
    def handle(self):
        with (out/'inn-server.log').open('a') as log:
            env={**os.environ,'INNCONF':str(self.server.config),'INN_HOSTNAME':'localhost.invalid','LD_LIBRARY_PATH':str(root/'usr/lib64')}
            command=[str(root/'usr/libexec/news/nnrpd'),'-n','-c',str(state/'etc/readers.conf')]
            if self.server.implicit:command.append('-S')
            process=subprocess.Popen(command,env=env,stdin=self.request,stdout=self.request,stderr=log);processes.append(process)
            try:process.wait(timeout=45)
            except subprocess.TimeoutExpired:process.terminate();process.wait(timeout=5)

try:
    with tempfile.TemporaryDirectory(prefix='dome-news-ref-') as directory:
        runtime=Path(directory);runtime.chmod(0o700)
        ca=runtime/'ca.pem';ca_key=runtime/'ca.key';private=runtime/'server.key';leaf=runtime/'server.pem';csr=runtime/'server.csr';extensions=runtime/'server.ext'
        extensions.write_text('subjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n')
        for command in [
            ['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','30','-sha256','-subj','/CN=Private INN fixture CA','-addext','basicConstraints=critical,CA:TRUE','-addext','keyUsage=critical,keyCertSign,cRLSign','-out',str(ca),'-keyout',str(ca_key)],
            ['openssl','req','-new','-newkey','rsa:2048','-nodes','-subj','/CN=localhost','-out',str(csr),'-keyout',str(private)],
            ['openssl','x509','-req','-in',str(csr),'-CA',str(ca),'-CAkey',str(ca_key),'-CAcreateserial','-days','30','-sha256','-extfile',str(extensions),'-out',str(leaf)]]:
            subprocess.run(command,check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        for path in [ca,ca_key,private,leaf]:path.chmod(0o600)
        config=(state/'etc/inn.conf').read_text()
        for parameter,value in {'tlscertfile':str(leaf),'tlskeyfile':str(private),'tlscafile':str(ca),'tlscapath':str(runtime)}.items():
            pattern=r'(?m)^#?'+parameter+r':[^\n]*$'
            config=re.sub(pattern,parameter+': '+value,config) if re.search(pattern,config) else config+'\n'+parameter+': '+value+'\n'
        cfg=runtime/'inn.conf';cfg.write_text(config)
        plain=Server(('127.0.0.1',0),Backend);plain.implicit=False;plain.config=cfg
        tls=Server(('127.0.0.1',0),Backend);tls.implicit=True;tls.config=cfg
        servers=[plain,tls]
        for server in servers:threading.Thread(target=server.serve_forever,daemon=True).start()
        rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1100x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
        with os.fdopen(rd) as stream:screen=':'+stream.readline().strip()
        connection=display.Display(screen)
        if a.wayland:
            from private_wayland import PrivateWayland
            wayland=PrivateWayland(connection,screen,runtime,out)
        for name in ['index','overview','nntp','number','message','tls','starttls']:
            folder=out/name;folder.mkdir();home=runtime/name;home.mkdir();profile=home/'profile';profile.mkdir(mode=0o700)
            server=tls if name=='tls' else plain;port=server.server_address[1];endpoint=f'news://localhost:{port}/'
            transport='tls' if name=='tls' else 'starttls' if name=='starttls' else 'plain'
            settings={'version':1,'default':endpoint,'servers':{endpoint:{'host':'localhost','port':port,'transport':transport,'ca_file':str(ca) if transport!='plain' else None}}}
            configfile=profile/'news-servers.json';configfile.write_text(json.dumps(settings));configfile.chmod(0o600)
            target=endpoint if name=='index' else endpoint+'comp.test' if name in ['overview','nntp'] else endpoint+'Article-1@Example.invalid'
            if name in ['number','nntp']:target=endpoint.replace('news://','nntp://')+'comp.test'+('/1' if name=='number' else '')
            env={**os.environ,'HOME':str(home),'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','XDG_RUNTIME_DIR':directory,'XDG_DATA_HOME':str(home/'data'),'XDG_CONFIG_HOME':str(home/'config'),'XDG_CACHE_HOME':str(home/'cache'),'XDG_STATE_HOME':str(home/'state'),'RUST_LOG':'warn'}
            for k in list(env):
                if k in ['WAYLAND_DISPLAY','WAYLAND_SOCKET'] or k.startswith(('DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE','DOME_BROWSER_MEMORY_CAPTURE')):env.pop(k,None)
            if wayland:env.pop('DISPLAY',None);env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket)
            with (folder/'browser.log').open('w') as log:
                child=subprocess.Popen(['dbus-run-session','--',str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log,start_new_session=True)
                w=wait(browser,30);check(w is not None,name+': native browser opens')
                if wayland:wayland.api('/api/action',{'op':'maximize','id':w.id});time.sleep(.3)
                title='News groups' if name=='index' else 'News: comp.test' if name in ['overview','nntp'] else 'News fixture'
                check(ready(title),name+': independent INN resource visibly paints');frame(w).save(folder/'frame.png')
                w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L)
                destination=home/('Downloads/news.txt' if name in ['index','overview','nntp'] else 'Downloads/article.eml')
                check(wait(destination.exists),name+': native source save completes against INN')
                content=destination.read_bytes();(folder/destination.name).write_bytes(content)
                if name=='index':check(wait(lambda:destination.exists() and b'comp.test' in destination.read_bytes()),'Independent active group list reaches native source')
                elif name in ['overview','nntp']:check(wait(lambda:destination.exists() and 'News fixture 世界'.encode() in destination.read_bytes()),'Independent overview subject is decoded through the browser')
                else:
                    expected=(state/'spool/articles/comp/test/1').read_bytes().replace(b'\n',b'\r\n')
                    check(wait(lambda:destination.exists() and destination.read_bytes()==expected),name+': original MIME article bytes survive NNTP framing exactly')
                (folder/destination.name).write_bytes(destination.read_bytes())
                check(destination.stat().st_mode&0o777==0o600,name+': native source save is private')
                close(w);check(child.wait(timeout=10)==0,name+': browser closes normally');child=None
                check(destination.exists(),name+': completed source survives browser shutdown')
                runs.append({'name':name,'passed':True})
        check(wait(lambda:all(p.poll() is not None for p in processes)) and all(p.returncode==0 for p in processes),'Every unmodified private INN process closes normally')
finally:
    if child and child.poll() is None:os.killpg(child.pid,signal.SIGTERM);child.wait(timeout=5)
    for server in servers:server.shutdown();server.server_close()
    for process in processes:
        if process.poll() is None:process.terminate();process.wait(timeout=5)
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    report={'passed':len(runs)==7 and all(c['passed'] for c in checks),'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'inn_sha256':sha(root/'usr/libexec/news/nnrpd'),'backend':'native-wayland' if a.wayland else 'private-x11','checks':checks,'runs':runs,'scope':'Seven browser resource/source loads against unmodified privately extracted INN 2.7.3, including implicit TLS and STARTTLS with a private CA. Synthetic two-article spool only; not full protocol conformance.'}
    (out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
