#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Mixed smallnet sessions, native bookmarks and history on private desktops."""
import argparse,hashlib,json,os,re,signal,socket,socketserver,ssl,subprocess,sys,tempfile,threading,time,urllib.parse
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_SMALLNET_PROFILE_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_SMALLNET_PROFILE_TEST_BUS':'1'})
from PIL import Image
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--include-additional',action='store_true',help='Include Mercury, Guppy, DICT, CSO/Ph, news/NNTP and Gopher TLS in mixed sessions')
p.add_argument('--include-scorpion',action='store_true',help='Also exercise plain and TLS Scorpion in mixed sessions')
p.add_argument('--include-whois',action='store_true',help='Include native WHOIS and RWhois lookups in mixed sessions')
p.add_argument('--include-fsp',action='store_true',help='Include native read-only FSP in mixed sessions')
p.add_argument('--fsp-server',type=Path,help='Unmodified reference fspd binary, required with --include-fsp')
p.add_argument('--include-titan',action='store_true',help='Restore Titan preparation without making a submission')
p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--wayland',action='store_true');p.add_argument('--installed-dispatch',action='store_true')
a=p.parse_args();binary=a.binary.resolve();out=a.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
driver_sha=sha(Path(__file__));checks=[];records=[];stages=[];servers=[];child=xvfb=connection=wayland=None;cold_pid=cold_start=None
installed_digest=sha(Path('/usr/local/libexec/dome-browser/dome-browser')) if a.installed_dispatch else None
def check(ok,label):
    checks.append({'name':label,'passed':bool(ok)});assert ok,label
    print('PASS',label,flush=True)
def wait(fn,seconds=30):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        value=fn()
        if value:return value
        time.sleep(.05)
    return None
class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True
class TLSServer(Server):
    def get_request(self):
        stream,peer=self.socket.accept();stream.settimeout(10)
        try:return self.context.wrap_socket(stream,server_side=True),peer
        except BaseException:stream.close();raise
class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(15);line=self.rfile.readline(16385)
        if not line:return
        kind=self.server.kind
        if kind=='mercury':
            uri=line[:-2].decode();records.append({'kind':kind,'request':uri});self.wfile.write(b'20 text/gemini\r\n# Profile Mercury\nSession content\n')
        elif kind=='tls':
            uri=line[:-2].decode().split(' ',1)[0];url=urllib.parse.urlsplit(uri);kind=url.scheme
            records.append({'kind':kind,'request':uri})
            if kind=='scroll':data=b'24 text/scroll\r\nProfile author\r\n\r\n\r\n# Profile Scroll\nSession content\n'
            else:data=b'20 text/gemini\r\n# Profile Gemini\nSession content\n'
            self.wfile.write(data);self.wfile.flush()
            try:self.connection.unwrap().close()
            except (OSError,ssl.SSLError):pass
        elif kind=='spartan':
            fields=line[:-2].split(b' ');size=int(fields[2]);body=self.rfile.read(size)
            records.append({'kind':kind,'request':line.decode().strip(),'body_bytes':len(body)})
            self.wfile.write(b'2 text/gemini\r\n# Profile Spartan\nSession content\n')
        else:
            actual='gopher+' if kind=='gopher' and b'\t+' in line else kind
            records.append({'kind':actual,'request':line.decode().strip()})
            body=('Profile '+actual+' content\r\n').encode()
            self.wfile.write(b'+'+str(len(body)).encode()+b'\r\n'+body if actual=='gopher+' else body+b'.\r\n' if actual in ['gopher','gophers'] else body)
            if kind=='gophers':
                self.wfile.flush()
                try:self.connection.unwrap().close()
                except (OSError,ssl.SSLError):pass
class FTP(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(15);listener=None;body=b'Profile FTP content\r\n'
        def send(code,text):self.wfile.write(f'{code} {text}\r\n'.encode());self.wfile.flush()
        send(220,'Profile fixture')
        try:
            while True:
                line=self.rfile.readline(16385)
                if not line:return
                verb,_,arg=line[:-2].partition(b' ');verb=verb.decode()
                if verb=='USER':records.append({'kind':'ftp-user','user':arg.decode()});send(331,'Password')
                elif verb=='PASS':send(230,'Anonymous fixture login')
                elif verb=='FEAT':self.wfile.write(b'211-Features\r\n UTF8\r\n211 End\r\n');self.wfile.flush()
                elif verb in ['TYPE','OPTS','CWD']:send(200,'Selected')
                elif verb=='SIZE':send(213,str(len(body)))
                elif verb=='EPSV':
                    listener=socket.socket();listener.bind(('127.0.0.1',0));listener.listen(1);listener.settimeout(10)
                    send(229,f'Extended passive (|||{listener.getsockname()[1]}|)')
                elif verb=='RETR':
                    records.append({'kind':'ftp','request':arg.decode()});send(150,'Data');data,_=listener.accept()
                    with data:data.sendall(body)
                    listener.close();listener=None;send(226,'Complete')
                elif verb=='QUIT':send(221,'Goodbye');return
                else:raise AssertionError('Unexpected fixture FTP command')
        finally:
            if listener:listener.close()
def server(kind,handler=Handler,context=None):
    service=(TLSServer if context else Server)(('127.0.0.1',0),handler);service.kind=kind
    if context:service.context=context
    servers.append(service);threading.Thread(target=service.serve_forever,daemon=True).start()
    return service.server_address[1]
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
    connection.sync();time.sleep(.03 if wayland else .01)
def text(value):
    for character in value:
        symbol=XK.string_to_keysym(character) or ord(character);code=connection.keysym_to_keycode(symbol)
        shifted=connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
        key(symbol,*([XK.XK_Shift_L] if shifted else []))
def focus(w):w.set_input_focus(X.RevertToParent,X.CurrentTime);connection.sync()
def navigate(w,url):
    focus(w);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(url);key(XK.XK_Return)
def capture(w,name):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX').save(out/name)
def painted(w):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    image=Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
    return any(hi-lo>80 for lo,hi in image.crop((20,145,min(700,g.width-20),min(420,g.height-30))).getextrema())
def close(w):
    if wayland:wayland.api('/api/action',{'op':'close','id':w.id})
    else:w.send_event(protocol.event.ClientMessage(window=w,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);connection.sync()
def json_file(path):
    try:return json.loads(path.read_text())
    except (OSError,ValueError):return None

def bus_owner(env):
    result=subprocess.run(['gdbus','call','--session','--dest','org.freedesktop.DBus','--object-path','/org/freedesktop/DBus','--method','org.freedesktop.DBus.GetConnectionUnixProcessID','org.domed.Browser'],env=env,capture_output=True,text=True,timeout=5)
    found=re.search(r'uint32 (\d+)',result.stdout) if result.returncode==0 else None
    return int(found[1]) if found else None

def process_start(pid):
    try:return Path(f'/proc/{pid}/stat').read_text().rsplit(') ',1)[1].split()[19]
    except (OSError,IndexError):return None

try:
 with tempfile.TemporaryDirectory(prefix='dome-smallnet-profile-') as directory:
    runtime=Path(directory);runtime.chmod(0o700);profile=runtime/'profile';profile.mkdir(mode=0o700)
    cert=runtime/'server.pem';private=runtime/'server.key'
    subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','30','-sha256','-subj','/CN=localhost','-addext','subjectAltName=DNS:localhost','-out',str(cert),'-keyout',str(private)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    cert.chmod(0o600);private.chmod(0o600);context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.minimum_version=ssl.TLSVersion.TLSv1_2;context.load_cert_chain(cert,private)
    ports={kind:server(kind) for kind in ['gopher','nex','spartan','finger']};ports['tls']=server('tls',context=context);ports['ftp']=server('ftp',FTP)
    urls=[f'gopher://127.0.0.1:{ports["gopher"]}/0/gopher#part',f'gopher+://127.0.0.1:{ports["gopher"]}/0/plus',
          f'gemini://localhost:{ports["tls"]}/page?two%20words#part',f'scroll://localhost:{ports["tls"]}/page?two%20words#1',
          f'nex://127.0.0.1:{ports["nex"]}/native.txt',f'spartan://127.0.0.1:{ports["spartan"]}/page',
          f'finger://alice@127.0.0.1:{ports["finger"]}/',f'ftp://127.0.0.1:{ports["ftp"]}/readme.txt;type=a#part']
    if a.include_additional:
        ports['mercury']=server('mercury');ports['gophers']=server('gophers',context=context)
        from guppy_fixture import GuppyFixture
        guppy=GuppyFixture(lambda url:(6,'text/gemini',b'# Profile Guppy\nSession content\n'),records);servers.append(guppy);ports['guppy']=guppy.server_address[1]
        from dict_fixture import DictFixture
        dictionary=DictFixture(records);servers.append(dictionary);ports['dict']=dictionary.server_address[1]
        from cso_fixture import CsoFixture
        for scheme in ['cso','ph']:
            directory_server=CsoFixture(records,kind=scheme);servers.append(directory_server);ports[scheme]=directory_server.server_address[1]
        urls += [f'mercury://localhost:{ports["mercury"]}/native?two%20words#part',f'gophers://localhost:{ports["gophers"]}/0/gophers#part',f'guppy://localhost:{ports["guppy"]}/native?two%20words#part',f'dict://localhost:{ports["dict"]}/d:fixture:wn:1#part']
        urls += [f'{scheme}://localhost:{ports[scheme]}/?q=fixture&field=name#part' for scheme in ['cso','ph']]
        from news_fixture import NewsFixture
        for scheme in ['news','nntp']:
            news_server=NewsFixture(records,kind=scheme);servers.append(news_server);ports[scheme]=news_server.server_address[1]
        urls += [f'news://localhost:{ports["news"]}/comp.test#part',f'nntp://localhost:{ports["nntp"]}/comp.test/101#part']
    if a.include_scorpion:
        from scorpion_fixture import ScorpionFixture
        class ScorpionRecords(list):
            def __init__(self,kind):super().__init__();self.kind=kind
            def append(self,item):super().append(item);records.append({'kind':self.kind,**item})
        for scheme in ['scorpion','scorpions']:
            service=ScorpionFixture(context=context if scheme=='scorpions' else None)
            service.records=ScorpionRecords(scheme);servers.append(service);ports[scheme]=service.port
            urls.append(f'{scheme}://localhost:{service.port}/index.scorp#start')
    if a.include_whois:
        from whois_fixture import WhoisFixture
        for scheme in ['whois','rwhois']:
            service=WhoisFixture(records,scheme);servers.append(service);ports[scheme]=service.server_address[1]
        urls += [f'whois://127.0.0.1:{ports["whois"]}/?q=Smith',f'rwhois://127.0.0.1:{ports["rwhois"]}/auth-area=example.invalid?q=domain+example']
    if a.include_fsp:
        if not a.fsp_server:p.error('--include-fsp requires --fsp-server')
        from fsp_reference_fixture import FspReference
        class FspRecords(list):
            def append(self,item):super().append(item);records.append({'kind':'fsp',**item})
        fsp_start=len(records);fsp_reference=FspReference(a.fsp_server,out/'fsp-reference',FspRecords());servers.append(fsp_reference);del records[fsp_start:]
        urls.append(f'fsp://127.0.0.1:{fsp_reference.port}/read%20me.txt')
    if a.include_titan:
        from titan_fixture import TitanFixture
        titan_server=TitanFixture(context);servers.append(titan_server)
        urls.append(f'titan://localhost:{titan_server.port}/resource#part')
    expected={'version':1,'active':3,'tabs':[{'url':url,'zoom':1.+i/10} for i,url in enumerate(urls)]}
    seeded=json.loads(json.dumps(expected));seeded['tabs'][5]['url']+='?one%20load';seeded['tabs'][7]['url']=urls[7].replace('ftp://','ftp://fixture:one-load@')
    (profile/'session.json').write_text(json.dumps(seeded));(profile/'session.json').chmod(0o600)
    rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1440x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
    with os.fdopen(rd) as stream:screen=':'+stream.readline().strip()
    connection=display.Display(screen)
    if a.wayland:
        from private_wayland import PrivateWayland
        wayland=PrivateWayland(connection,screen,runtime,out)
    env={**os.environ,'HOME':directory,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','XDG_RUNTIME_DIR':directory,'XDG_DATA_HOME':str(runtime/'data'),'XDG_CONFIG_HOME':str(runtime/'config'),'XDG_CACHE_HOME':str(runtime/'cache'),'XDG_STATE_HOME':str(runtime/'state'),'RUST_LOG':'warn'}
    for name in list(env):
        if name in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','MOZ_GCTIMER','DOME_BROWSER_GC_PROFILE'] or name.startswith(('DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE','DOME_BROWSER_MEMORY_CAPTURE')):env.pop(name,None)
    if wayland:env.pop('DISPLAY',None);env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket)
    def start(label):
        global child
        log=(out/(label+'.log')).open('w')
        try:child=subprocess.Popen([str(binary),'--software','--profile',str(profile)],env=env,stdout=log,stderr=log)
        finally:log.close()
        w=wait(browser)
        if w is None:(out/(label+'-launch-diagnostics.json')).write_text(json.dumps({'exit_code':child.poll(),'windows':[{'title':str(w.get_wm_name()),'width':w.get_geometry().width,'height':w.get_geometry().height} for w in windows()]},indent=2)+'\n')
        check(w is not None,label+': native window opens');return w
    def all_schemes_loaded(since=0):return {r['kind'] for r in records[since:]}>={urllib.parse.urlsplit(url).scheme for url in urls}-{'titan'}
    def complete(label,since=0):
        check(wait(lambda:all_schemes_loaded(since)),label+': all restored protocols reach their local fixtures')
        if a.include_titan:check(not titan_server.records,label+': restoring Titan makes no submission or application request')
        check(wait(lambda:json_file(profile/'session.json')==expected),label+': tab order, active tab and per-tab zoom are preserved')
        check(wait(lambda:(profile/'hs.txt').exists() and set((profile/'hs.txt').read_text().splitlines())>=set(urls)),label+': history contains every native resource')
        check(all(r['body_bytes']==0 for r in records if r['kind']=='spartan'),label+': restoring a Spartan resource sends no previous submission body')
        check(all(r['user']=='anonymous' for r in records if r['kind']=='ftp-user'),label+': restored FTP resources use fresh per-load login state')
    w=start('initial');complete('initial');check(wait(lambda:'Profile Scroll' in str(w.get_wm_name())),'Restored active Scroll tab is displayed')
    check(wait(lambda:painted(w)),'Initial restored native document visibly paints')
    capture(w,'initial-tabs.png');stages.append('mixed-session')
    if a.installed_dispatch:
        check(binary==Path('/usr/local/bin/dome-browser'),'Installed dispatch uses the real system launcher')
        desktop=Path('/usr/local/share/applications/org.domed.Browser.desktop');check(desktop.is_file(),'System browser desktop entry exists')
        Path(env['XDG_CONFIG_HOME']).mkdir(parents=True,exist_ok=True,mode=0o700)
        for url in urls:
            scheme=urllib.parse.urlsplit(url).scheme;kind='x-scheme-handler/'+scheme
            configured=subprocess.run(['gio','mime',kind,'org.domed.Browser.desktop'],env=env,capture_output=True,text=True,timeout=10)
            check(configured.returncode==0,scheme+': private MIME default selects the installed desktop entry')
        for url in [*urls,urls[3]]:
            scheme=urllib.parse.urlsplit(url).scheme;since=len(records)
            with (out/'dispatch.log').open('a') as output:
                result=subprocess.run(['gio','open',url],env=env,stdout=output,stderr=subprocess.STDOUT,timeout=10)
            check(result.returncode==0,scheme+': GIO opens the installed URI handler')
            if scheme=='titan':check(wait(lambda:'Titan submission' in str(w.get_wm_name())) and not titan_server.records,scheme+': installed dispatch paints preparation without an application request')
            else:check(wait(lambda:any(r['kind']==scheme for r in records[since:])),scheme+': dispatched URI reaches its local protocol fixture')
            check(wait(lambda:len(json_file(profile/'session.json')['tabs'])==len(urls)+1 and json_file(profile/'session.json')['tabs'][json_file(profile/'session.json')['active']]['url']==url if json_file(profile/'session.json') else False),scheme+': desktop dispatch opens the unchanged public URI in its new tab')
            owner=subprocess.run(['gdbus','call','--session','--dest','org.freedesktop.DBus','--object-path','/org/freedesktop/DBus','--method','org.freedesktop.DBus.GetConnectionUnixProcessID','org.domed.Browser'],env=env,capture_output=True,text=True,check=True,timeout=5)
            check(str(child.pid) in owner.stdout and child.poll() is None,scheme+': forwarding keeps the original installed process')
            snapshot=json_file(profile/'session.json');check(len(snapshot['tabs'])==len(urls)+1 and snapshot['tabs'][:len(urls)]==expected['tabs'],scheme+': desktop dispatch preserves all existing native tabs')
            focus(w);key(XK.XK_w,XK.XK_Control_L);check(wait(lambda:len(json_file(profile/'session.json')['tabs'])==len(urls)),scheme+': dispatched tab closes normally')
            current=json_file(profile/'session.json')['active']
            for _ in range((expected['active']-current)%len(urls)):key(XK.XK_Tab,XK.XK_Control_L)
            check(wait(lambda:json_file(profile/'session.json')==expected),scheme+': original active tab and zoom are restored')
        check(wait(lambda:json_file(profile/'session.json')==expected),'Installed URI dispatch preserves the other native tabs and original session layout')
        check(len(windows())==1,'Installed URI dispatch creates no duplicate browser window')
        stages.append('installed-dispatch')
    focus(w);key(XK.XK_s,XK.XK_Control_L,XK.XK_Shift_L)
    check(wait(lambda:json_file(profile/'saved-session.json')==expected),'Save Session keeps all native tabs and zoom levels')
    focus(w);key(XK.XK_d,XK.XK_Control_L)
    def dialog():return next((item for item in windows() if str(item.get_wm_name() or '')=='Dome Browser'),None)
    for value in ['Native profile bookmark','Smallnet']:
        form=wait(dialog);check(form is not None,'Native bookmark prompt opens');focus(form);key(XK.XK_a,XK.XK_Control_L);text(value);key(XK.XK_Return)
        check(wait(lambda:form.id not in [item.id for item in windows()]),'Bookmark prompt accepts the entered value')
    bookmark=profile/'bm.txt'
    check(wait(lambda:bookmark.exists() and urls[3]+' Native profile bookmark' in bookmark.read_text()),'Bookmark file stores the active native resource')
    check(':s1: Smallnet' in bookmark.read_text(),'Bookmark retains its selected section')
    navigate(w,urls[4]);check(wait(lambda:urls[4] in str(w.get_wm_name())),'Native resource can replace the active tab')
    focus(w);key(XK.XK_o,XK.XK_Control_L,XK.XK_Shift_L);time.sleep(.3);key(XK.XK_Down);key(XK.XK_Return)
    check(wait(lambda:'Profile Scroll' in str(w.get_wm_name())),'Native bookmark picker reopens its native resource');stages.append('bookmark')
    navigate(w,urls[4]);check(wait(lambda:urls[4] in str(w.get_wm_name())),'Second native replacement completes')
    navigate(w,urls[3].split('/page')[0]+'/pa')
    check(wait(lambda:json_file(profile/'session.json')==expected),'Address completion reopens the full native URL from history');stages.append('history-completion')
    navigate(w,urls[4]);check(wait(lambda:urls[4] in str(w.get_wm_name())),'Active tab changes before explicit session restore')
    since=len(records);focus(w);key(XK.XK_r,XK.XK_Control_L,XK.XK_Shift_L);complete('explicit-restore',since)
    check(wait(lambda:'Profile Scroll' in str(w.get_wm_name())),'Restore Saved Session activates its saved native tab');stages.append('explicit-restore')
    close(w);check(child.wait(timeout=20)==0,'Mixed native session closes cleanly');child=None
    pins=(profile/'gemini-trust.json').read_bytes();since=len(records);w=start('restart');complete('restart',since)
    check(wait(lambda:'Profile Scroll' in str(w.get_wm_name())) and wait(lambda:painted(w)),'Restart displays and paints its saved active native document')
    check((profile/'gemini-trust.json').read_bytes()==pins,'Restart reuses the same Gemini and Scroll server pins')
    check(bookmark.read_text().count(urls[3]+' Native profile bookmark')==1,'Bookmark survives restart without duplication')
    capture(w,'restored-tabs.png');close(w);check(child.wait(timeout=20)==0,'Restored mixed session closes cleanly');child=None;stages.append('restart')
    if a.installed_dispatch:
        check(wait(lambda:bus_owner(env) is None),'Installed browser releases its private activation name before cold dispatch')
        for url in urls:
            scheme=urllib.parse.urlsplit(url).scheme;since=len(records)
            with (out/'dispatch-cold.log').open('a') as output:result=subprocess.run(['gio','open',url],env=env,stdout=output,stderr=subprocess.STDOUT,timeout=10)
            check(result.returncode==0,scheme+': cold GIO dispatch launches the installed handler')
            cold_pid=wait(lambda:bus_owner(env));check(cold_pid is not None,scheme+': cold handler owns its isolated activation service');cold_start=process_start(cold_pid)
            check(sha(Path(f'/proc/{cold_pid}/exe'))==installed_digest,scheme+': cold handler runs the verified installed binary')
            w=wait(browser);check(w is not None and wait(lambda:painted(w)),scheme+': cold native window paints')
            if scheme=='titan':check(wait(lambda:'Titan submission' in str(w.get_wm_name())) and not titan_server.records,scheme+': cold dispatch paints preparation without an application request')
            else:check(wait(lambda:any(r['kind']==scheme for r in records[since:])),scheme+': cold URI reaches its protocol fixture')
            cold_profile=runtime/'data/dome-browser/session.json'
            check(wait(lambda:json_file(cold_profile) and len(json_file(cold_profile)['tabs'])==1 and json_file(cold_profile)['tabs'][0]['url']==url),scheme+': cold launch retains its requested public URI')
            close(w);check(wait(lambda:bus_owner(env) is None and not Path(f'/proc/{cold_pid}/exe').exists()),scheme+': cold handler closes and releases activation');cold_pid=cold_start=None
        stages.append('installed-cold-dispatch')
    for name in ['session.json','saved-session.json','bm.txt','hs.txt']:
        file=profile/name;check(file.stat().st_mode&0o777==0o600,name+': stored profile file has private permissions')
        (out/name).write_bytes(file.read_bytes())
finally:
    if child is not None and child.poll() is None:child.kill();child.wait()
    if cold_pid is not None and process_start(cold_pid)==cold_start and Path(f'/proc/{cold_pid}/exe').exists() and sha(Path(f'/proc/{cold_pid}/exe'))==installed_digest:
        os.kill(cold_pid,signal.SIGTERM);wait(lambda:not Path(f'/proc/{cold_pid}/exe').exists(),5)
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    for service in servers:service.shutdown();service.server_close()
    (out/'report.json').write_text(json.dumps({'binary_sha256':installed_digest if a.installed_dispatch else sha(binary),'launcher_sha256':sha(binary) if a.installed_dispatch else None,'desktop_entry_sha256':sha(Path('/usr/local/share/applications/org.domed.Browser.desktop')) if a.installed_dispatch else None,'installed_dispatch':a.installed_dispatch,'include_additional':a.include_additional,'include_scorpion':a.include_scorpion,'include_titan':a.include_titan,'include_fsp':a.include_fsp,'include_whois':a.include_whois,'native_schemes':sorted({urllib.parse.urlsplit(url).scheme for url in urls}) if 'urls' in globals() else [],'driver_sha256':driver_sha,'display_backend':'native-wayland' if a.wayland else 'private-x11','wayland_helper_sha256':sha(Path(__file__).with_name('private_wayland.py')) if a.wayland else None,'checks':checks,'stages':stages,'passed':len(stages)==(7 if a.installed_dispatch else 5) and bool(checks) and all(item['passed'] for item in checks),'scope':'Isolated local protocol fixtures and profile state; mixed native sessions, explicit restore, bookmark picker, address completion and clean restart. Optional installed dispatch exercises GIO and the real system desktop entry with private MIME defaults, single-instance forwarding and cold startup for all eight mandatory protocols. No live desktop or server changes.'},indent=2)+'\n')
