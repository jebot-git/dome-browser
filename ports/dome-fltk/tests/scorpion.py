#!/usr/bin/env python3
"""Native Scorpion retrieval on private desktops and synthetic local servers."""
import argparse,hashlib,json,os,re,signal,ssl,subprocess,sys,tempfile,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_SCORPION_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_SCORPION_TEST_BUS':'1'})
from PIL import Image,ImageChops
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
from scorpion_fixture import ScorpionFixture,document
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--wayland',action='store_true')
p.add_argument('--reference-root',type=Path,help='Pinned upstream scorpiond and scorpionlib sources for independent retrieval tests')
p.add_argument('--client-key-type',choices=['rsa','ed25519','ec-p256'],default='rsa')
cases=['index','child','input','input-link','plain','unknown-length','keep-open','redirect','redirect-loop','failure','bad-header','truncated-body','truncated-block','cancel','source','large-source','restart','tls','login','uri-auth','login-cancel','identity','identity-cancel','certificate-decline','redirect-accept','redirect-cancel','downgrade-accept','downgrade-cancel','upgrade-accept','upgrade-cancel','error-light']
p.add_argument('--cases',nargs='+',choices=cases);a=p.parse_args()
reference_cases=['index','child','plain','source','restart']
a.cases=a.cases or (reference_cases if a.reference_root else cases)
if a.reference_root and any(case not in reference_cases for case in a.cases):p.error('Reference daemon supports index, child, plain, source and restart fixture cases')
binary=a.binary.resolve();out=a.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
checks=[];runs=[];services=[];child=xvfb=wayland=connection=None

def sha(path):
    with Path(path).open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()
def check(value,label):
    checks.append({'name':label,'passed':bool(value)});assert value,label;print('PASS',label,flush=True)
def wait(fn,seconds=15):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        value=fn()
        if value:return value
        time.sleep(.05)
    return None
def windows():
    if wayland:return wayland.windows()
    return [w for w in connection.screen().root.query_tree().children if w.get_attributes().map_state==X.IsViewable]
def browser():return next((w for w in windows() if (str(w.get_wm_name() or '').endswith(' — Dome Browser') if wayland else w.get_geometry().width>=1000)),None)
def form(title):return next((w for w in windows() if w.get_wm_name()==title),None)
def frame(w):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    return Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
def painted(w):
    image=frame(w)
    if wayland and w.value()['maximized'] and image.getpixel((20,150))!=image.getpixel((image.width-20,150)):return False
    return any(hi-lo>70 for lo,hi in image.crop((20,145,min(900,image.width-20),min(700,image.height-30))).getextrema())
def ready(title):return wait(lambda:browser() and title in str(browser().get_wm_name() or '') and painted(browser()))
def key(symbol,mod=None):
    if mod:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(mod))
    code=connection.keysym_to_keycode(symbol);xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
    if mod:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(mod))
    connection.sync();time.sleep(.025 if wayland else .005)
def text(value):
    for c in value:
        symbol=XK.string_to_keysym(c) or ord(c);code=connection.keysym_to_keycode(symbol);shift=connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
        key(symbol,XK.XK_Shift_L if shift else None)
def click(w,x,y):
    w.set_input_focus(X.RevertToParent,X.CurrentTime)
    if not wayland:w.configure(stack_mode=X.Above)
    g=w.get_geometry();xtest.fake_input(connection,X.MotionNotify,x=g.x+x-1,y=g.y+y);connection.sync();time.sleep(.03)
    xtest.fake_input(connection,X.MotionNotify,x=g.x+x,y=g.y+y);xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync();time.sleep(.1)
def navigate(w,url):
    w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(url);key(XK.XK_Return)
def link(w,row):
    image=frame(w);r,g,b=image.split();mask=ImageChops.multiply(ImageChops.subtract(b,r).point(lambda v:255 if v>=20 else 0),ImageChops.subtract(b,g).point(lambda v:255 if v>=20 else 0));pixels=mask.tobytes();groups=[]
    for y in range(160,min(820,image.height-30)):
        spans=list(re.finditer(b'\xff{12,}',pixels[y*image.width:(y+1)*image.width]))
        if spans:
            if groups and y-groups[-1][0]<=2:groups[-1]=(y,spans)
            else:groups.append((y,spans))
    if len(groups)<=row:return False
    y,spans=groups[row];span=spans[0];click(w,(span.start()+span.end())//2,y-4);return True
def close(w):
    if wayland:wayland.api('/api/action',{'op':'close','id':w.id})
    else:w.send_event(protocol.event.ClientMessage(window=w,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);connection.sync()
try:
    with tempfile.TemporaryDirectory(prefix='dome-scorpion-') as directory:
        root=Path(directory);root.chmod(0o700)
        cert=root/'server.pem';private=root/'server.key'
        subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','30','-subj','/CN=Scorpion fixture','-addext','subjectAltName=DNS:localhost','-out',str(cert),'-keyout',str(private)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        client_cert=root/'client.pem';client_key=root/'client.key'
        key_args=['rsa:2048'] if a.client_key_type=='rsa' else ['ed25519'] if a.client_key_type=='ed25519' else ['ec','-pkeyopt','ec_paramgen_curve:P-256']
        subprocess.run(['openssl','req','-x509','-newkey',*key_args,'-nodes','-days','30','-subj','/CN=Scorpion fixture client','-addext','basicConstraints=critical,CA:FALSE','-addext','extendedKeyUsage=clientAuth','-addext','keyUsage=critical,digitalSignature','-out',str(client_cert),'-keyout',str(client_key)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        client_cert.chmod(0o600);client_key.chmod(0o600);client_id=hashlib.sha256(ssl.PEM_cert_to_DER_cert(client_cert.read_text())).hexdigest()
        context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.minimum_version=ssl.TLSVersion.TLSv1_2;context.load_cert_chain(cert,private);context.verify_mode=ssl.CERT_OPTIONAL;context.load_verify_locations(cafile=str(client_cert))
        if a.reference_root:
            from scorpion_reference_fixture import ReferenceFixture
            plain=ReferenceFixture(a.reference_root,root,out)
        else:plain=ScorpionFixture()
        secure=ScorpionFixture(context=context);other=ScorpionFixture();dual=ScorpionFixture(context=context,dual=True);services=[plain,secure,other,dual]
        source_document=plain.document if a.reference_root else document()
        rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1100x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
        with os.fdopen(rd) as f:screen=':'+f.readline().strip()
        connection=display.Display(screen)
        if a.wayland:
            from private_wayland import PrivateWayland
            wayland=PrivateWayland(connection,screen,root,out)
        for name in a.cases:
            folder=out/name;folder.mkdir();home=root/name;home.mkdir();profile=home/'profile'
            env={**os.environ,'HOME':str(home),'GNUPGHOME':str(home/'gpg'),'XDG_RUNTIME_DIR':directory,'XDG_CONFIG_HOME':str(home/'config'),'XDG_DATA_HOME':str(home/'data'),'XDG_CACHE_HOME':str(home/'cache'),'XDG_STATE_HOME':str(home/'state'),'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','RUST_LOG':'warn'}
            for k in list(env):
                if k in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','MOZ_GCTIMER','DOME_BROWSER_GC_PROFILE'] or k.startswith(('DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE','DOME_BROWSER_MEMORY_CAPTURE')):env.pop(k,None)
            if wayland:env.pop('DISPLAY',None);env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket)
            upgrade=name in ['upgrade-accept','upgrade-cancel']
            tls_case=upgrade or name in ['tls','identity','identity-cancel','certificate-decline','downgrade-accept','downgrade-cancel']
            server=dual if upgrade else secure if tls_case else plain;server.mode='bad-header' if name=='error-light' else name if name in ['unknown-length','keep-open','redirect-loop','failure','bad-header','truncated-body','truncated-block'] else 'identity' if upgrade or name in ['identity','identity-cancel'] else 'partial' if name=='cancel' else 'normal';server.started.clear();server.closed.clear()
            reviewed_redirect=name in ['redirect-accept','redirect-cancel','downgrade-accept','downgrade-cancel']
            if reviewed_redirect:
                destination=plain if tls_case else other;destination.mode='normal';destination_start=len(destination.records)
                server.mode='external-redirect';server.redirect_target=f'scorpion://localhost:{destination.port}/index.scorp'
            scheme='scorpions' if tls_case and not upgrade else 'scorpion';base=f'{scheme}://localhost:{server.port}';endpoint='/auth' if name in ['login','uri-auth','login-cancel'] else '/input' if name=='input' else '/plain.txt' if name=='plain' else '/redirect' if name=='redirect' else '/index.scorp';target=base+endpoint+'#start';before=len(server.records)
            if name=='uri-auth':target=target.replace('://','://fixture:fixture-pass@',1)
            if upgrade or name in ['identity','identity-cancel']:
                catalog=home/'data/de-xr/identities/index.json';catalog.parent.mkdir(parents=True,mode=0o700)
                catalog.write_text(json.dumps({'version':1,'identities':[{'id':client_id,'name':'Scorpion fixture identity','certificate':str(client_cert),'key':str(client_key),'address':'fixture@localhost'}]}));catalog.chmod(0o600);catalog_hash=sha(catalog)
            if name=='certificate-decline':
                profile.mkdir(mode=0o700);trust=profile/'gemini-trust.json'
                decoded=ssl._ssl._test_decode_cert(str(cert));pin={'fingerprint':'00'*32,'not_before':int(ssl.cert_time_to_seconds(decoded['notBefore'])),'expires':int(ssl.cert_time_to_seconds(decoded['notAfter']))}
                trust.write_text(json.dumps({'version':1,'pins':{base:pin},'identities':[]}));trust.chmod(0o600);trust_hash=sha(trust)
            if name=='error-light':
                appearance=home/'config/dome/appearance.json';appearance.parent.mkdir(parents=True)
                appearance.write_text(json.dumps({'light':True,'font':'Unifont / Sans-Serif','font_size':16,'accent':'#6babff'}));env['XDG_CURRENT_DESKTOP']='Dome'
            with (folder/'browser.log').open('w') as log:
                try:
                    launch_start=time.monotonic();child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log)
                    w=wait(browser)
                    launch_seconds=round(time.monotonic()-launch_start,3)
                    if w is None:
                        diagnostic={'pid':child.pid,'exit_code':child.poll(),'windows':[{'id':v.id,'title':v.get_wm_name()} for v in windows()]}
                        if child.poll() is None:
                            diagnostic['process_status']=Path(f'/proc/{child.pid}/status').read_text()
                            diagnostic['wait_channel']=Path(f'/proc/{child.pid}/wchan').read_text()
                        if wayland:diagnostic['scene']=wayland.scene()
                        (folder/'launch-diagnostic.json').write_text(json.dumps(diagnostic,indent=2)+'\n')
                    check(w is not None,name+': browser opens its private window')
                    if upgrade:
                        f=wait(lambda:form('Use TLS for Scorpion identity'));check(f is not None,'Plain identity status asks before changing transport')
                        check(server.records[before:] and not any(v['transport_tls'] or v['client_certificate'] for v in server.records[before:]),'No TLS connection or identity precedes transport approval')
                        if name=='upgrade-cancel':
                            click(f,565,613);check(wait(lambda:form('Use TLS for Scorpion identity') is None),'Transport upgrade review cancels cleanly');time.sleep(.2)
                            check(not any(v['transport_tls'] for v in server.records[before:]),'Cancelling the upgrade makes no TLS request')
                            check(not (profile/'gemini-trust.json').exists(),'Cancelling the upgrade creates no TLS pin');check(child.poll() is None,'Cancelled upgrade leaves the browser responsive')
                        else:
                            click(f,680,613);f=wait(lambda:form('Scorpion client identity'));check(f is not None,'Approved TLS upgrade opens a separate identity decision')
                            check(any(v['transport_tls'] for v in server.records[before:]) and not any(v['client_certificate'] for v in server.records[before:]),'Upgraded request is anonymous before identity approval')
                            click(f,680,613);check(ready('Scorpion fixture'),'Explicitly approved identity retrieves the upgraded resource')
                            check(any(v['transport_tls'] and v['client_fingerprint']==client_id for v in server.records[before:]),'Selected identity is used only over the approved TLS transport')
                            pins=json.loads((profile/'gemini-trust.json').read_text())['pins'];check(f'scorpions://localhost:{server.port}' in pins,'Upgrade records its pin under the TLS scheme')
                        check(sha(catalog)==catalog_hash,'Transport upgrade leaves the identity catalog unchanged')
                    elif reviewed_redirect:
                        title='Continue without TLS' if tls_case else 'Follow Scorpion redirect';f=wait(lambda:form(title))
                        check(f is not None,'Cross-authority or transport redirect opens native review');check(len(destination.records)==destination_start,'Redirect destination is not contacted before approval')
                        frame(f).save(folder/'redirect-review.png')
                        if name.endswith('-cancel'):
                            click(f,565,613);check(wait(lambda:form(title) is None),'Redirect review cancels cleanly');time.sleep(.2)
                            check(len(destination.records)==destination_start,'Cancelled redirect never contacts the destination');check(child.poll() is None,'Cancelled redirect leaves the browser responsive')
                        else:
                            click(f,680,613);check(ready('Scorpion fixture'),'Approved redirect renders the destination document')
                            check(len(destination.records)>destination_start,'Destination is contacted after explicit approval')
                            check(not any(v['client_certificate'] or v['authenticated'] for v in destination.records[destination_start:]),'Cross-authority redirect does not carry an identity or account')
                    elif name=='certificate-decline':
                        f=wait(lambda:form('Scorpion certificate changed'));check(f is not None,'Changed server certificate opens its native trust prompt')
                        check(len(server.records)==before,'No R request precedes the certificate decision');click(f,565,613)
                        check(wait(lambda:form('Scorpion certificate changed') is None),'Changed-certificate prompt cancels cleanly')
                        check(sha(trust)==trust_hash,'Declining a changed certificate preserves the prior pin');check(len(server.records)==before,'Declining a changed certificate sends no R request')
                        replacement=f'scorpion://localhost:{plain.port}/plain.txt';navigate(w,replacement);check(ready(replacement),'Other navigation remains available after declining the change')
                    elif name in ['identity','identity-cancel']:
                        f=wait(lambda:form('Scorpion client identity'));check(f is not None,'Identity status opens explicit native selection')
                        check(not any(v['client_certificate'] for v in server.records[before:]),'Anonymous request precedes explicit identity selection')
                        if name=='identity-cancel':
                            click(f,565,613);check(wait(lambda:form('Scorpion client identity') is None),'Identity selection cancels cleanly')
                            check(not any(v['client_certificate'] for v in server.records[before:]),'Cancelling selection sends no client certificate')
                            replacement=f'scorpion://localhost:{plain.port}/plain.txt';navigate(w,replacement);check(ready(replacement),'Other navigation remains available after identity cancellation')
                        else:
                            click(f,36,436);frame(f).save(folder/'identity-source.png');click(f,680,613)
                            check(ready('Scorpion fixture'),'Shared identity retrieves the native document')
                            check(any(v['client_fingerprint']==client_id for v in server.records[before:]),'Server receives the explicitly selected shared identity')
                            bindings=json.loads((profile/'gemini-trust.json').read_text())['identities'];check(len(bindings)==1 and bindings[0]['authority']==base,'Remembered identity is scoped to the secure Scorpion authority')
                            start=len(server.records);w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_r,XK.XK_Control_L)
                            check(wait(lambda:any(v['client_fingerprint']==client_id for v in server.records[start:])),'Reload reuses the approved identity binding')
                            check(form('Scorpion client identity') is None,'Remembered scope does not reopen identity selection')
                        check(sha(catalog)==catalog_hash,'Browser leaves the source identity catalog unchanged')
                    elif name in ['login','uri-auth','login-cancel']:
                        if name!='uri-auth':
                            f=wait(lambda:form('Scorpion login'));check(f is not None,'Login status opens native account controls')
                            check(not any(record['authenticated'] for record in server.records[before:]),'No account is sent before the user submits it')
                            if name=='login-cancel':click(f,565,613)
                            else:click(f,170,130);text('fixture');click(f,170,206);text('fixture-pass');click(f,680,613)
                        if name=='login-cancel':
                            check(wait(lambda:form('Scorpion login') is None),'Account prompt cancels cleanly');time.sleep(.2)
                            check(not any(record['authenticated'] for record in server.records[before:]),'Cancel does not send an account');check(child.poll() is None,'Cancelling an account leaves the browser responsive')
                        else:
                            check(ready('Authenticated fixture'),'Explicit account retrieves the native document')
                            check(any(record['authenticated'] for record in server.records[before:]),'Fixture confirms the explicitly supplied account')
                            check('fixture-pass' not in str(w.get_wm_name()),'Credentials are absent from the window title')
                            start=len(server.records);w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_r,XK.XK_Control_L)
                            f=wait(lambda:form('Scorpion login'));check(f is not None,'Reload asks for account approval again')
                            check(not any(record['authenticated'] for record in server.records[start:]),'Reload does not reuse transient credentials');click(f,565,613)
                    elif name=='input':
                        f=wait(lambda:form('Scorpion link input'));check(f is not None,'Server input status opens native controls before a query is sent');click(f,170,390);text('two words');click(f,680,613);check(ready('Input fixture'),'Submitted input opens its native document');check(server.records[-1]['request'].endswith('/input?two%20words'),'Query is percent encoded exactly once')
                    elif name=='cancel':
                        check(wait(server.started.is_set),'Partial document reaches its bounded reader');navigate(w,'about:blank');check(wait(server.closed.is_set),'Replacement navigation releases the stalled socket');check(child.poll() is None,'Cancellation leaves the application responsive')
                    elif name in ['bad-header','truncated-body','truncated-block','redirect-loop','error-light']:
                        check(wait(lambda:browser() and 'Error loading page' in str(browser().get_wm_name() or '') and painted(browser())),'Invalid or incomplete response produces a readable error page')
                        if name=='redirect-loop':check(len(server.records)-before==6,'Automatic redirect chains stop after five redirects')
                        if name=='error-light':
                            image=frame(w);check(min(image.getpixel((40,300)))>200,'Error page follows the configured light appearance')
                            check(sum(max(pixel)<100 for pixel in image.crop((20,100,min(680,image.width-20),260)).getdata())>500,'Light error page has visible dark diagnostic text')
                    elif name in ['plain','failure']:
                        check(ready(base),'Plain text or protocol failure visibly paints')
                        if name=='failure':time.sleep(.3);check(len(server.records)-before==1,'Temporary failure does not schedule an automatic retry')
                    else:
                        check(ready('Child fixture' if name=='redirect' else 'Scorpion fixture'),name+': decoded document visibly paints')
                        if name=='child':
                            check(wait(lambda:link(w,0)),'Native document has an actionable child link');check(ready('Child fixture'),'Child link retrieves its document');w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_Left,XK.XK_Alt_L);check(ready('Scorpion fixture'),'Back restores the native document')
                        if name=='input-link':
                            check(wait(lambda:link(w,1)),'Input link is actionable without an initial server request');f=wait(lambda:form('Scorpion link input'));check(f is not None,'Input link opens the native query form');click(f,170,390);text('two words');click(f,680,613);check(ready('Input fixture'),'Input link submits a native query document')
                        if name=='source':
                            w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L);dest=home/'Downloads/index.scorp';check(wait(lambda:dest.exists() and dest.read_bytes()==source_document),'Save Source preserves the original binary block bytes');check(dest.stat().st_mode&0o777==0o600,'Saved source has private permissions')
                        if name=='large-source':
                            def rss():
                                try:return int(next(v.split()[1] for v in Path(f'/proc/{child.pid}/status').read_text().splitlines() if v.startswith('VmRSS:')))*1024
                                except (OSError,StopIteration):return 0
                            initial=rss();peak=initial;server.mode='large';w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L);dest=home/'Downloads/index.scorp';size=128*1024*1024;end=time.monotonic()+60
                            while time.monotonic()<end and not(dest.exists() and dest.stat().st_size==size and server.closed.is_set()):peak=max(peak,rss());time.sleep(.05)
                            check(dest.exists() and dest.stat().st_size==size and server.closed.is_set(),'128 MiB source completes through the real download consumer')
                            expected=hashlib.sha256();chunk=b'0123456789abcdef'*(65536//16)
                            for _ in range(size//len(chunk)):expected.update(chunk)
                            check(sha(dest)==expected.hexdigest(),'Large source digest matches the exact remote bytes');check(peak-initial<96*1024*1024,'Large source has bounded browser RSS growth');(folder/'stream.json').write_text(json.dumps({'bytes':size,'sha256':sha(dest),'rss_before':initial,'rss_peak':peak},indent=2)+'\n')
                        if name=='keep-open':check(wait(server.closed.is_set),'Known-length completion does not wait for an idle peer')
                        if name=='tls':
                            pins=json.loads((profile/'gemini-trust.json').read_text())['pins'];check(any(k.startswith('scorpions://localhost:') for k in pins),'TLS certificate is pinned under the distinct secure scheme');check((profile/'gemini-trust.json').stat().st_mode&0o777==0o600,'TLS pins are private')
                    frame(w).save(folder/'frame.png');close(w);check(child.wait(timeout=10)==0,name+': browser closes cleanly');child=None
                    if name in ['login','uri-auth','login-cancel']:
                        persisted=[file for file in profile.rglob('*') if file.is_file()]
                        check(all(b'fixture-pass' not in file.read_bytes() for file in persisted),'Account password is absent from persisted profile files')
                        check(b'fixture-pass' not in (folder/'browser.log').read_bytes(),'Account password is absent from browser diagnostics')
                    if name=='restart':
                        previous=len(server.records);child=subprocess.Popen([str(binary),'--software','--profile',str(profile)],env=env,stdout=log,stderr=log);w=wait(browser);check(w is not None and ready('Scorpion fixture') and len(server.records)>previous,'Restart restores and retrieves the saved native resource');close(w);check(child.wait(timeout=10)==0,'Restored resource closes cleanly');child=None
                    if name=='source':check(dest.read_bytes()==source_document,'Original source survives a clean shutdown')
                    if name=='large-source':check(sha(dest)==expected.hexdigest(),'Large source survives a clean shutdown')
                    records=server.records[before:]
                    if a.reference_root:check(records,name+': unmodified reference daemon accepted the browser connection')
                    elif name!='certificate-decline':check(records and all('#' not in v['request'] for v in records),name+': local fragments are never transmitted')
                    if not tls_case:check(not (profile/'gemini-trust.json').exists(),name+': plain transport never consults TLS settings')
                    runs.append({'name':name,'passed':True,'launch_seconds':launch_seconds});print('CASE',name,flush=True)
                finally:
                    if sys.exc_info()[0] is not None:
                        for v in windows():
                            try:frame(v).save(folder/f'failure-{v.id}.png')
                            except Exception:pass
                    if child and child.poll() is None:child.terminate();child.wait(timeout=10)
                    child=None
        check(not any(s.errors for s in services),'All fixture exchanges remain within the read-only request protocol')
finally:
    for service in services:service.close()
    if child and child.poll() is None:child.kill();child.wait()
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    (out/'report.json').write_text(json.dumps({'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'fixture_sha256':sha(Path(__file__).with_name('scorpion_reference_fixture.py' if a.reference_root else 'scorpion_fixture.py')),'cases':runs,'checks':checks,'passed':bool(checks) and all(c['passed'] for c in checks),'native_wayland':bool(a.wayland),'reference_daemon':bool(a.reference_root),'client_key_type':a.client_key_type,'scope':'Private local read-only reference daemon and C-generated documents' if a.reference_root else 'Private local fixtures and synthetic documents; no external services or messages'},indent=2)+'\n')
