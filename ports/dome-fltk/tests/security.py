#!/usr/bin/env python3
"""Real Servo + isolated HTTP, X11 selections and an in-memory KWallet fixture."""
import asyncio, base64, contextlib, http.server, json, os, select, signal, socket, subprocess, sys, tempfile, threading, time
from pathlib import Path
from urllib.parse import parse_qs
signal.pthread_sigmask(signal.SIG_SETMASK, [])

if '--wallet' in sys.argv:
    from dbus_next.aio import MessageBus
    from dbus_next.service import ServiceInterface, method
    log=Path(sys.argv[-1])
    class Wallet(ServiceInterface):
        def __init__(self): super().__init__('org.kde.KWallet');self.folders={}
        def record(self,operation,key=''): 
            with log.open('a') as f:f.write(json.dumps({'operation':operation,'key':key})+'\n')
        @method()
        def networkWallet(self)->'s':return 'test-wallet'
        @method()
        def open(self,wallet:'s',window:'x',app:'s')->'i':self.record('open');return 7
        @method()
        def isOpen(self,handle:'i')->'b':return handle==7
        @method()
        def hasFolder(self,handle:'i',folder:'s',app:'s')->'b':return folder in self.folders
        @method()
        def createFolder(self,handle:'i',folder:'s',app:'s')->'b':self.folders[folder]={};return True
        @method()
        def entryList(self,handle:'i',folder:'s',app:'s')->'as':self.record('list');return list(self.folders.get(folder,{}))
        @method()
        def readPassword(self,handle:'i',folder:'s',key:'s',app:'s')->'s':self.record('read',key);return self.folders.get(folder,{}).get(key,'')
        @method()
        def writePassword(self,handle:'i',folder:'s',key:'s',value:'s',app:'s')->'i':self.record('write',key);self.folders[folder][key]=value;return 0
        @method()
        def removeEntry(self,handle:'i',folder:'s',key:'s',app:'s')->'i':self.record('remove',key);self.folders[folder].pop(key,None);return 0
    async def serve():
        bus=await MessageBus().connect();bus.export('/modules/kwalletd6',Wallet());await bus.request_name('org.kde.kwalletd6')
        print('ready',flush=True);await asyncio.Future()
    asyncio.run(serve());raise SystemExit()
if os.environ.get('DOME_SECURITY_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_SECURITY_BUS':'1'})

from Xlib import X, XK, display, protocol
from Xlib.ext import xtest
source=Path(__file__).resolve().parents[3]
artifacts=Path(os.environ.get('DOME_BROWSER_TEST_OUTPUT',source.parent/'artifacts/browser-security-tests'));artifacts.mkdir(parents=True,exist_ok=True)
checks=[];events=[];hits=[]
def check(ok,name):
    assert ok,name
    checks.append(name);print('PASS',name,flush=True)
def wait(fn,seconds=40):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        result=fn()
        if result:return result
        time.sleep(.05)
    raise AssertionError('Timed out: '+repr(fn))
def latest(name):return next((e for e in reversed(events) if e['name']==name),None)

class Server(http.server.BaseHTTPRequestHandler):
    def response(self,data,kind='text/html',status=200):
        if isinstance(data,str):data=data.encode()
        self.send_response(status);self.send_header('Content-Type',kind);self.send_header('Cache-Control','no-store');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def do_GET(self):
        hits.append(self.path)
        if self.path.startswith('/basic-'):
            realm=self.path.removeprefix('/basic-');user='alice' if realm=='alpha' else 'bob'
            expected='Basic '+base64.b64encode((user+':http-fixture-secret').encode()).decode()
            if self.headers.get('Authorization')!=expected:
                self.send_response(401);self.send_header('WWW-Authenticate','Basic realm="'+realm+'"');self.send_header('Content-Length','0');self.end_headers();return
            self.response('<title>HTTP '+realm+' accepted</title>');return
        if self.path=='/redirect-start.js':
            self.send_response(302);self.send_header('Location','/ad-test-redirect.js');self.send_header('Content-Length','0');self.end_headers();return
        if self.path.endswith('.js'):
            variable='allowed' if 'allowed' in self.path else 'cross' if 'cross' in self.path else 'redirected' if 'redirect' in self.path else 'blocked'
            self.response('window.'+variable+'=true;','text/javascript');return
        if self.path=='/ad-test-image.png':self.response(b'not-an-image','image/png');return
        if self.path=='/ad-test-xhr':self.response('ok','text/plain');return
        if self.path=='/frame-login':
            self.response('''<form action=/login method=post><input name=user><input style="position:absolute;top:40px;left:10px" name=pass type=password id=p></form><script>p.onfocus=()=>parent.postMessage('frame-focused','*')</script>''');return
        page='''<!doctype html><meta charset=utf-8><title>Browser security fixture</title>
<style>body{height:3200px;margin:0;font:16px sans-serif}textarea,input,button{position:absolute;left:24px;width:300px;height:42px}#paste{top:60px;height:65px}#user{top:160px}#pass{top:220px}#submit{top:280px}iframe{position:absolute;top:400px;left:24px;width:350px;height:100px}</style>
<h1>Security fixture</h1><textarea id=paste></textarea><form method=post action=/login autocomplete=on><input id=user name=user autocomplete=username><input id=pass name=pass type=password autocomplete=current-password><button id=submit>Sign in</button></form>
<script src=/ad-test-blocked.js></script><script src=/ad-test-allowed.js></script><script src=/redirect-start.js></script>
<script src="http://localhost:PORT/cross.js" referrerpolicy=no-referrer></script><img src=/ad-test-image.png>
<iframe src="http://localhost:PORT/frame-login"></iframe>
<script>
function report(name,data={}){fetch('/event',{method:'POST',body:JSON.stringify({name,data})}).catch(()=>{})}
paste.oninput=()=>report('INPUT',{value:paste.value});
addEventListener('scroll',()=>report('SCROLL',{y:scrollY}));
addEventListener('message',e=>{if(e.data==='frame-focused')report('FRAME_FOCUSED')});
let xhr=false;fetch('/ad-test-xhr').then(()=>xhr=true).catch(()=>{});
addEventListener('load',()=>setTimeout(()=>report('READY',{blocked:!!window.blocked,allowed:!!window.allowed,cross:!!window.cross,redirected:!!window.redirected,xhr}),400));
</script>'''.replace('PORT',str(self.server.server_port))
        self.response(page)
    def do_POST(self):
        data=self.rfile.read(int(self.headers.get('Content-Length','0')))
        if self.path=='/login':
            values=parse_qs(data.decode());events.append({'name':'LOGIN','data':values});self.response('<title>Login accepted</title>');return
        events.append(json.loads(data));self.response('',status=204)
    def log_message(self,*args):pass

class Selections:
    def __init__(self,screen):
        self.d=display.Display(screen);root=self.d.screen().root
        self.window=root.create_window(0,0,1,1,0,X.CopyFromParent,X.InputOutput,X.CopyFromParent)
        self.utf8=self.d.intern_atom('UTF8_STRING');self.targets=self.d.intern_atom('TARGETS')
        self.values={self.d.intern_atom('PRIMARY'):'PRIMARY žluťoučký 日本語',self.d.intern_atom('CLIPBOARD'):'CLIPBOARD explicit copy'}
        for atom in self.values:self.window.set_selection_owner(atom,X.CurrentTime)
        self.d.sync();self.stop=False;self.thread=threading.Thread(target=self.serve,daemon=True);self.thread.start()
    def serve(self):
        while not self.stop:
            if not self.d.pending_events():select.select([self.d.fileno()],[],[],.1);continue
            e=self.d.next_event()
            if e.type!=X.SelectionRequest:continue
            prop=e.property or e.target;ok=True
            if e.target==self.targets:e.requestor.change_property(prop,Xatom_ATOM,32,[self.utf8,Xatom_STRING,self.targets])
            elif e.target in (self.utf8,Xatom_STRING):e.requestor.change_property(prop,e.target,8,self.values[e.selection].encode('utf-8' if e.target==self.utf8 else 'latin-1',errors='replace'))
            else:ok=False
            e.requestor.send_event(protocol.event.SelectionNotify(time=e.time,requestor=e.requestor,selection=e.selection,target=e.target,property=prop if ok else X.NONE),propagate=False);self.d.flush()
    def close(self):self.stop=True;self.thread.join(timeout=2);self.d.close()
Xatom_ATOM=4;Xatom_STRING=31

processes=[];selections=None
with tempfile.TemporaryDirectory(prefix='browser-security-') as temporary:
    root=Path(temporary);root.chmod(0o700);profile=root/'profile';profile.mkdir()
    (profile/'adblock.txt').write_text('/ad-test-$script,image,xmlhttprequest\n@@/ad-test-allowed.js$script\n||localhost^$script,third-party\n')
    rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);processes.append(xvfb);os.close(wr)
    with os.fdopen(rd) as f:screen=':'+f.readline().strip()
    x=display.Display(screen)
    env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','XDG_RUNTIME_DIR':temporary,'XDG_CONFIG_HOME':temporary+'/config','XDG_CACHE_HOME':temporary+'/cache','XDG_DATA_HOME':temporary+'/data','XDG_CURRENT_DESKTOP':'GNOME','LIBGL_ALWAYS_SOFTWARE':'1'}
    for name in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(name,None)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server);threading.Thread(target=server.serve_forever,daemon=True).start()
    origin='http://127.0.0.1:'+str(server.server_port);walletlog=root/'wallet.jsonl';log=(artifacts/'browser.log').open('w')
    def windows():return x.screen().root.query_tree().children
    def window(title):return wait(lambda:next((w for w in windows() if title in str(w.get_wm_name() or '') and w.get_attributes().map_state==X.IsViewable),None))
    def key(w,name,control=False):
        w.set_input_focus(X.RevertToParent,X.CurrentTime)
        code=x.keysym_to_keycode(XK.string_to_keysym(name));ctrl=x.keysym_to_keycode(XK.string_to_keysym('Control_L'))
        if control:xtest.fake_input(x,X.KeyPress,ctrl)
        xtest.fake_input(x,X.KeyPress,code);xtest.fake_input(x,X.KeyRelease,code)
        if control:xtest.fake_input(x,X.KeyRelease,ctrl)
        x.sync();time.sleep(.15)
    def type_text(text):
        for char in text:
            code,index=list(x.keysym_to_keycodes(ord(char)))[0];shift=x.keysym_to_keycode(XK.string_to_keysym('Shift_L'))
            if index%2:xtest.fake_input(x,X.KeyPress,shift)
            xtest.fake_input(x,X.KeyPress,code);xtest.fake_input(x,X.KeyRelease,code)
            if index%2:xtest.fake_input(x,X.KeyRelease,shift)
        x.sync();time.sleep(.15)
    def click(w,px,py,button=1):
        g=w.get_geometry();xtest.fake_input(x,X.MotionNotify,x=g.x+px,y=g.y+py);xtest.fake_input(x,X.ButtonPress,button);xtest.fake_input(x,X.ButtonRelease,button);x.sync();time.sleep(.2)
    def navigate(url):key(main,'l',True);type_text(url);key(main,'Return')
    def records():return [json.loads(line) for line in walletlog.read_text().splitlines()] if walletlog.exists() else []
    def prompt_dialog():return wait(lambda:next((w for w in windows() if w.id!=main.id and str(w.get_wm_name() or '')=='Dome Browser' and w.get_attributes().map_state==X.IsViewable),None))
    async def activate_menu(label):
        from dbus_next.aio import MessageBus
        from dbus_next import Variant
        service=bytes(main.get_full_property(x.intern_atom('_KDE_NET_WM_APPMENU_SERVICE_NAME'),X.AnyPropertyType).value).decode()
        path=bytes(main.get_full_property(x.intern_atom('_KDE_NET_WM_APPMENU_OBJECT_PATH'),X.AnyPropertyType).value).decode()
        bus=await MessageBus().connect()
        try:
            introspection=await bus.introspect(service,path);iface=bus.get_proxy_object(service,path,introspection).get_interface('com.canonical.dbusmenu')
            revision,layout=await iface.call_get_layout(0,-1,[])
            def find(node):
                ident,props,children=node
                if props.get('label') and props['label'].value==label:return ident
                for child in children:
                    result=find(child.value)
                    if result is not None:return result
            ident=find(layout);assert ident is not None,label
            await iface.call_event(ident,'clicked',Variant('s',''),0)
        finally:bus.disconnect()
    try:
        service=subprocess.Popen([sys.executable,__file__,'--wallet',str(walletlog)],env=env,stdout=subprocess.PIPE,stderr=log,text=True);processes.append(service);assert service.stdout.readline().strip()=='ready'
        binary=Path(os.environ.get('DOME_BROWSER_BINARY', source/'target/debug/dome-browser'))
        app=subprocess.Popen([str(binary),'--profile',str(profile),origin+'/security.html'],env=env,stdout=log,stderr=log);processes.append(app)
        main=window('Browser security fixture');ready=wait(lambda:latest('READY'));print('FILTER RESULTS',ready['data'],flush=True)
        check(ready['data']=={'blocked':False,'allowed':True,'cross':False,'redirected':False,'xhr':False},'adblock-rust blocks scripts, images, XHR and redirected targets while honoring exceptions')
        check('/cross.js' not in hits,'third-party filtering works when Referrer-Policy removes the referrer')
        check('/ad-test-image.png' not in hits and '/ad-test-xhr' not in hits,'blocked resources never reach the HTTP server')
        selections=Selections(screen)
        click(main,70,120+85,2);wait(lambda:latest('INPUT') and latest('INPUT')['data']['value']==selections.values[x.intern_atom('PRIMARY')])
        check(True,'middle-click pastes UTF-8 PRIMARY selection into web text fields')
        key(main,'a',True);key(main,'BackSpace');key(main,'v',True)
        wait(lambda:latest('INPUT')['data']['value']=='CLIPBOARD explicit copy')
        check(True,'ordinary Ctrl+V uses CLIPBOARD independently of PRIMARY')
        key(main,'l',True);key(main,'BackSpace');click(main,500,54,2);key(main,'a',True);key(main,'c',True)
        click(main,70,120+85);key(main,'a',True);key(main,'BackSpace');key(main,'v',True)
        wait(lambda:latest('INPUT')['data']['value']==selections.values[x.intern_atom('PRIMARY')])
        check(True,'the native location field also accepts middle-click PRIMARY paste')
        selections.window.set_selection_owner(selections.d.intern_atom('PRIMARY'),X.CurrentTime);selections.d.sync()
        g=main.get_geometry();xtest.fake_input(x,X.MotionNotify,x=g.x+28,y=g.y+198);xtest.fake_input(x,X.ButtonPress,1);x.sync();time.sleep(.3)
        xtest.fake_input(x,X.MotionNotify,x=g.x+90,y=g.y+198);x.sync();time.sleep(.3)
        xtest.fake_input(x,X.ButtonRelease,1);x.sync()
        wait(lambda:x.get_selection_owner(x.intern_atom('PRIMARY')).id!=selections.window.id)
        key(main,'a',True);key(main,'BackSpace');key(main,'v',True)
        wait(lambda:latest('INPUT')['data']['value']==selections.values[x.intern_atom('PRIMARY')])
        check(True,'web mouse selections publish PRIMARY without changing explicit-copy CLIPBOARD')

        click(main,800,600,5);wait(lambda:latest('SCROLL') and latest('SCROLL')['data']['y']>0);down=latest('SCROLL')['data']['y']
        click(main,800,600,4);wait(lambda:latest('SCROLL')['data']['y']<down)
        check(True,'standard desktop wheel down scrolls down and wheel up scrolls up')
        wait(lambda:latest('SCROLL')['data']['y']==0)
        click(main,80,120+180);type_text('alice');click(main,80,120+240);type_text('form-fixture-secret');click(main,80,120+300)
        key(window('Save login in KWallet'),'Return');window('Login accepted')
        check(any(r['operation']=='write' for r in records()),'submitting a real login form offers encrypted KWallet storage')
        navigate(origin+'/security.html');main=window('Browser security fixture');time.sleep(.7)
        before_reads=len(records());click(main,80,120+455);wait(lambda:latest('FRAME_FOCUSED'));time.sleep(.3)
        check(len(records())==before_reads,'cross-origin iframe password focus cannot access the wallet')
        click(main,80,120+240);time.sleep(.3);key(main,'Down');key(main,'Return');time.sleep(.3)
        click(main,80,120+300);window('Login accepted')
        check(latest('LOGIN')['data']=={'user':['alice'],'pass':['form-fixture-secret']},'native account selection fills real DOM login controls without page-JavaScript scraping')
        for realm,user in [('alpha','alice'),('beta','bob')]:
            navigate(origin+'/basic-'+realm)
            dialog=prompt_dialog();type_text(user);key(dialog,'Return');dialog=prompt_dialog();type_text('http-fixture-secret');key(dialog,'Return')
            key(window('Save login in KWallet'),'Return');main=window('HTTP '+realm+' accepted')
        keys=[json.loads(r['key']) for r in records() if r['operation']=='write']
        check(any(k[0]=='http' and 'alpha' in k[2] for k in keys) and any(k[0]=='http' and 'beta' in k[2] for k in keys),'HTTP authentication credentials are isolated by challenge realm')
        navigate(origin+'/basic-alpha');time.sleep(.4);key(main,'Down');key(main,'Return');main=window('HTTP alpha accepted')
        check(any(r['operation']=='read' and 'alpha' in r['key'] for r in records()),'HTTP authentication can reuse the selected KWallet account')
        before=sum(1 for e in events if e['name']=='READY');navigate(origin+'/security.html');main=window('Browser security fixture');wait(lambda:sum(1 for e in events if e['name']=='READY')>before)
        before=sum(1 for e in events if e['name']=='READY');asyncio.run(activate_menu('Toggle exception for this site'));key(main,'r',True);wait(lambda:sum(1 for e in events if e['name']=='READY')>before)
        check(latest('READY')['data']['blocked'] and latest('READY')['data']['redirected'] and latest('READY')['data']['cross'],'native site exception allows resources on reload')
        key(main,'q',True);wait(lambda:app.poll() is not None)
        check(app.returncode==0,'browser shuts down cleanly with security integrations enabled')
        check(not (profile/'auth_cache.json').exists(),'HTTP passwords are not serialized into a plaintext authentication cache')
        for path in profile.rglob('*'):
            if path.is_file():
                data=path.read_bytes();assert b'form-fixture-secret' not in data and b'http-fixture-secret' not in data,path
        check(True,'passwords are absent from all browser profile files')
    finally:
        if selections:selections.close()
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:process.kill();process.wait()
        server.shutdown();server.server_close();
        with contextlib.suppress(Exception):x.close()
        log.close()
        (artifacts/'checks.json').write_text(json.dumps(checks,indent=2)+'\n')
        (artifacts/'requests.json').write_text(json.dumps(hits,indent=2)+'\n')
