#!/usr/bin/env python3
"""Explicit Titan actions on private X11/Wayland desktops and loopback TLS."""
import argparse, hashlib, json, os, re, signal, ssl, subprocess, sys, tempfile, time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_TITAN_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session','--',sys.executable,*sys.argv], {**os.environ,'DOME_TITAN_TEST_BUS':'1'})
from PIL import Image, ImageChops
from Xlib import X, XK, display, protocol
from Xlib.ext import xtest
from titan_fixture import TitanFixture

cases=['landing','parameters','text','file','delete','delete-file','empty-text','empty-file','cancel-method','cancel-review','reload','restart','receipt-restart','redirect','redirect-titan','early-failure','failure','input','input-cancel','identity','identity-cancel','identity-review-cancel','certificate-decline','file-changed','large-file','cancel-transfer','bad-header','sensitive','gemini-redirect','view','source-download']
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True)
p.add_argument('--wayland',action='store_true');p.add_argument('--cases',nargs='+',choices=cases)
p.add_argument('--reference-root',type=Path);p.add_argument('--perl-prefix',type=Path)
a=p.parse_args();reference_cases=['landing','parameters','text','file','delete','delete-file','reload','receipt-restart'];a.cases=a.cases or (reference_cases if a.reference_root else cases)
if a.reference_root and (not a.perl_prefix or any(name not in reference_cases for name in a.cases)):p.error('Reference mode needs --perl-prefix and only landing, parameters, text, file, delete, reload or receipt-restart cases')
binary=a.binary.resolve();out=a.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
checks=[];runs=[];child=xvfb=connection=wayland=fixture=None
def sha(path):
    with Path(path).open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()
def check(value,label):
    checks.append({'name':label,'passed':bool(value)});print(('PASS ' if value else 'FAIL ')+label,flush=True);assert value,label
def wait(fn,seconds=30):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        value=fn()
        if value:return value
        time.sleep(.05)
    return None
def windows():
    if wayland:return wayland.windows()
    return [w for w in connection.screen().root.query_tree().children if w.get_attributes().map_state==X.IsViewable]
def browser():
    for w in windows():
        try:
            if str(w.get_wm_name() or '').endswith(' — Dome Browser') if wayland else w.get_geometry().width>=1000:return w
        except (StopIteration,OSError):pass
    return None
def form(title):
    for w in windows():
        try:
            if w.get_wm_name()==title:return w
        except (StopIteration,OSError):pass
    return None
def frame(w):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    return Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
def painted(w):return any(hi-lo>70 for lo,hi in frame(w).crop((20,145,min(900,w.get_geometry().width-20),min(700,w.get_geometry().height-30))).getextrema())
def ready(title):return wait(lambda:browser() and re.search(re.escape(title)+r'\s+(?:—|â|\?)',str(browser().get_wm_name())) and painted(browser()))
def canonical(profile,target):
    try:
        session=json.loads((profile/'session.json').read_text());return session['tabs'][session['active']]['url']==target
    except (OSError,ValueError,IndexError,KeyError):return False
def body_frame(w):
    image=frame(w);return image.crop((20,145,min(900,image.width-20),min(500,image.height-30)))
def recovered(w,profile,target,title,count):
    check(ready(title),'Stopped operation has a readable public resource page')
    def replaced():
        current=body_frame(w)
        return current.size!=initial_body.size or ImageChops.difference(current,initial_body).getbbox()
    check(wait(replaced),'Stopped operation visibly replaces the preparation content')
    check(wait(lambda:canonical(profile,target)),'Stopped operation saves the real resource address')
    frame(w).save(folder/'stopped-resource.png');w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_r,XK.XK_Control_L)
    check(ready('Titan submission'),'Reload returns to preparation at the real resource')
    time.sleep(.3);check(form('Titan action') is None and len(fixture.records)==count,'Reload after stopping neither submits nor replays an internal action')
def key(symbol,mod=None):
    if mod:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(mod))
    code=connection.keysym_to_keycode(symbol);xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
    if mod:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(mod))
    connection.sync();time.sleep(.025 if wayland else .005)
def text(value):
    for character in value:
        if character=='\n':key(XK.XK_Return);continue
        symbol=XK.string_to_keysym(character) or ord(character);code=connection.keysym_to_keycode(symbol)
        shift=connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
        key(symbol,XK.XK_Shift_L if shift else None)
def click(w,x,y):
    w.set_input_focus(X.RevertToParent,X.CurrentTime)
    if not wayland:w.configure(stack_mode=X.Above)
    g=w.get_geometry();xtest.fake_input(connection,X.MotionNotify,x=g.x+x-1,y=g.y+y);connection.sync();time.sleep(.03)
    xtest.fake_input(connection,X.MotionNotify,x=g.x+x,y=g.y+y);xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync();time.sleep(.1)
def navigate(w,url):
    w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(url);key(XK.XK_Return)
def link(w,row):
    image=frame(w);red,green,blue=image.split();mask=ImageChops.multiply(ImageChops.subtract(blue,red).point(lambda v:255 if v>=20 else 0),ImageChops.subtract(blue,green).point(lambda v:255 if v>=20 else 0));pixels=mask.tobytes();groups=[]
    for y in range(150,min(820,image.height-30)):
        spans=list(re.finditer(b'\xff{12,}',pixels[y*image.width:(y+1)*image.width]))
        if spans:
            if groups and y-groups[-1][0]<=2:groups[-1]=(y,spans)
            else:groups.append((y,spans))
    if len(groups)<=row:return False
    y,spans=groups[row];span=spans[0];click(w,(span.start()+span.end())//2,y-4);return True
def close(w):
    if wayland:wayland.api('/api/action',{'op':'close','id':w.id})
    else:w.send_event(protocol.event.ClientMessage(window=w,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);connection.sync()
def choose_file(f,path,folder):
    click(f,640,130);chooser=wait(lambda:next((w for w in windows() if w.id not in [f.id,browser().id] and 'file' in str(w.get_wm_name()).lower()),None),5)
    check(chooser is not None,'Native file chooser opens');frame(chooser).save(folder/'file-chooser.png')
    chooser.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(str(path));key(XK.XK_Return)
    check(wait(lambda:chooser.id not in [w.id for w in windows()],5),'Native chooser returns the selected file')
def begin(w,mode):
    check(wait(lambda:link(w,1)),'Landing offers an explicit preparation action')
    f=wait(lambda:form('Titan action'));check(f is not None,'Native action selector opens')
    if mode:
        click(f,700,130)
        for _ in range(mode):key(XK.XK_Down)
        key(XK.XK_Return)
    click(f,680,613)
def certificate(root,stem,client=False):
    cert=root/(stem+'.pem');key_path=root/(stem+'.key')
    args=['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','30','-subj','/CN=Titan fixture client' if client else '/CN=localhost','-addext','basicConstraints=critical,CA:FALSE','-out',str(cert),'-keyout',str(key_path)]
    if client:args+=['-addext','extendedKeyUsage=clientAuth','-addext','keyUsage=critical,digitalSignature']
    else:args+=['-addext','subjectAltName=DNS:localhost']
    subprocess.run(args,check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);cert.chmod(0o600);key_path.chmod(0o600)
    return cert,key_path,hashlib.sha256(ssl.PEM_cert_to_DER_cert(cert.read_text())).hexdigest()
try:
    with tempfile.TemporaryDirectory(prefix='dome-titan-') as directory:
        root=Path(directory);root.chmod(0o700);cert,server_key,server_id=certificate(root,'server');client_cert,client_key,client_id=certificate(root,'client',True)
        context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.minimum_version=ssl.TLSVersion.TLSv1_2;context.load_cert_chain(cert,server_key);context.verify_mode=ssl.CERT_OPTIONAL;context.load_verify_locations(cafile=str(client_cert))
        if a.reference_root:
            from titan_reference_fixture import PhoebeFixture
            fixture=PhoebeFixture(a.reference_root,a.perl_prefix,root,out,cert,server_key)
        else:fixture=TitanFixture(context)
        rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1100x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
        with os.fdopen(rd) as f:screen=':'+f.readline().strip()
        connection=display.Display(screen)
        if a.wayland:
            from private_wayland import PrivateWayland
            wayland=PrivateWayland(connection,screen,root,out)
        for name in a.cases:
            folder=out/name;folder.mkdir(mode=0o700);home=root/name;home.mkdir(mode=0o700);profile=home/'profile'
            env={**os.environ,'HOME':str(home),'GNUPGHOME':str(home/'gpg'),'XDG_RUNTIME_DIR':directory,'XDG_CONFIG_HOME':str(home/'config'),'XDG_DATA_HOME':str(home/'data'),'XDG_CACHE_HOME':str(home/'cache'),'XDG_STATE_HOME':str(home/'state'),'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','RUST_LOG':'warn'}
            for k in list(env):
                if k in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','MOZ_GCTIMER','DOME_BROWSER_GC_PROFILE'] or k.startswith(('DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE','DOME_BROWSER_MEMORY_CAPTURE')):env.pop(k,None)
            if wayland:env.pop('DISPLAY',None);env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket)
            fixture.mode='identity' if name in ['identity','identity-cancel','identity-review-cancel'] else 'input' if name=='input-cancel' else 'stalled' if name=='cancel-transfer' else name if name in ['redirect','redirect-titan','early-failure','failure','input','sensitive','gemini-redirect','bad-header'] else 'normal';fixture.started.clear();fixture.closed.clear();fixture.cancel_requested=False
            base=f'titan://localhost:{fixture.port}';target=base+'/resource?username=Alex#local';before=len(fixture.records)
            if name=='parameters':target=base+'/resource;size=0;mime=text/plain;token=ignored-token?username=Alex#local'
            if a.reference_root:fixture.prepare_case(name);target=target.replace('/resource','/resource-'+name,1)
            if name in ['input','sensitive','input-cancel']:target=base+'/resource#local'
            if name=='gemini-redirect':target=target.replace('titan://','gemini://',1)
            if name in ['identity','identity-cancel','identity-review-cancel']:
                catalog=home/'data/de-xr/identities/index.json';catalog.parent.mkdir(parents=True,mode=0o700);catalog.write_text(json.dumps({'version':1,'identities':[{'id':client_id,'name':'Titan fixture identity','certificate':str(client_cert),'key':str(client_key),'address':'fixture@localhost'}]}));catalog.chmod(0o600);catalog_hash=sha(catalog)
            if name=='certificate-decline':
                profile.mkdir(mode=0o700);trust=profile/'gemini-trust.json';decoded=ssl._ssl._test_decode_cert(str(cert));pin={'fingerprint':'00'*32,'not_before':int(ssl.cert_time_to_seconds(decoded['notBefore'])),'expires':int(ssl.cert_time_to_seconds(decoded['notAfter']))};trust.write_text(json.dumps({'version':1,'pins':{base:pin},'identities':[]}));trust.chmod(0o600);trust_hash=sha(trust)
            with (folder/'browser.log').open('w') as log:
                try:
                    start=time.monotonic();child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(profile),target],env=env,stdout=log,stderr=log)
                    w=wait(browser);check(w is not None,name+': browser opens');launch_seconds=round(time.monotonic()-start,3)
                    check(ready('Titan submission'),'Navigation paints a preparation page');time.sleep(.15)
                    initial_body=body_frame(w)
                    if name=='gemini-redirect':check(len(fixture.records)==before+1 and fixture.records[-1]['scheme']=='gemini','Gemini redirect makes no Titan request')
                    else:check(len(fixture.records)==before,'Navigation sends no Titan or Gemini request')
                    if name in ['landing','parameters','restart','gemini-redirect']:
                        if name=='parameters':
                            check(wait(lambda:(profile/'session.json').exists()),'Clean resource address reaches the session file');check('ignored-token' not in (profile/'session.json').read_text(),'Supplied command parameters are absent from the session')
                    elif name=='view':
                        check(wait(lambda:link(w,0)),'Landing offers Gemini viewing');check(ready('Existing resource'),'Selected Gemini resource renders');check(len(fixture.records)==before+1 and fixture.records[-1]['scheme']=='gemini','Viewing uses only Gemini retrieval')
                    elif name=='source-download':
                        w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_s,XK.XK_Control_L);time.sleep(.5);check(len(fixture.records)==before,'Save Source does not submit or contact the server');check(child.poll() is None,'Unsupported Titan source save leaves the browser responsive')
                    elif name=='cancel-method':
                        check(wait(lambda:link(w,1)),'Preparation action is available');f=wait(lambda:form('Titan action'));check(f is not None,'Native selector opens');click(f,565,613);check(wait(lambda:form('Titan action') is None),'Selector cancels cleanly');check(len(fixture.records)==before,'Cancelling preparation sends no request');recovered(w,profile,target,'Titan submission',before)
                    else:
                        file_case=name in ['file','empty-file','file-changed','large-file','cancel-transfer'];mode=2 if name in ['delete','delete-file'] else 1 if file_case else 0;begin(w,mode)
                        f=wait(lambda:form('Titan deletion' if mode==2 else 'Titan file' if file_case else 'Titan text'));check(f is not None,'Native payload controls open')
                        payload=b'# Fixture text\nHello Titan';source=None
                        if file_case:
                            source=home/'payload.bin';payload='File bytes: Žluťoučký 世界\n'.encode()
                            if name in ['large-file','cancel-transfer']:
                                chunk=b'0123456789abcdef'*(65536//16)
                                with source.open('wb') as output:
                                    for _ in range(128*1024*1024//len(chunk)):output.write(chunk)
                                expected_sha=sha(source)
                            else:source.write_bytes(b'' if name=='empty-file' else payload)
                            choose_file(f,source,folder)
                        elif mode==0 and name!='empty-text':click(f,170,145);text(payload.decode())
                        if mode!=2:
                            click(f,180,206 if file_case else 282);key(XK.XK_a,XK.XK_Control_L);text('application/octet-stream' if file_case else 'text/gemini');click(f,180,282 if file_case else 358);text('fixture-token')
                        else:
                            if name=='delete-file':click(f,180,130);key(XK.XK_a,XK.XK_Control_L);text('application/octet-stream')
                            click(f,180,206);text('fixture-token')
                        click(f,680,613)
                        if name in ['empty-file','empty-text']:
                            check(wait(lambda:form('Titan file' if file_case else 'Titan text')),'Empty content returns to preparation');check(len(fixture.records)==before,'Empty content does not become a delete request');click(form('Titan file' if file_case else 'Titan text'),565,613)
                        else:
                            title='Review Titan deletion' if mode==2 else 'Review Titan upload';f=wait(lambda:form(title));check(f is not None,'Submission gets a separate native review');check(len(fixture.records)==before,'Review precedes any application request');frame(f).save(folder/'review.png')
                            if name=='cancel-review':click(f,565,613);check(wait(lambda:form(title) is None),'Final review cancels cleanly');check(len(fixture.records)==before,'Cancelling review sends no request');recovered(w,profile,target,'Titan submission',before)
                            else:
                                if name=='file-changed':source.write_bytes(b'changed after selection')
                                initial=0
                                if name=='large-file':initial=int(next(v.split()[1] for v in Path(f'/proc/{child.pid}/status').read_text().splitlines() if v.startswith('VmRSS:')))*1024;peak=initial
                                click(f,680,613)
                                if name=='certificate-decline':
                                    f=wait(lambda:form('Titan certificate changed'));check(f is not None,'Changed certificate requires native review');check(len(fixture.records)==before,'No intent precedes certificate approval');click(f,565,613);check(wait(lambda:form('Titan certificate changed') is None),'Certificate change cancels cleanly');check(sha(trust)==trust_hash,'Declining a change preserves the saved pin');recovered(w,profile,target,'Titan submission',before)
                                elif name=='file-changed':check(ready('Titan action not sent'),'Changed local file is reported before submission');check(len(fixture.records)==before,'Changed file is not submitted');recovered(w,profile,target,'Titan action not sent',before)
                                elif name=='cancel-transfer':
                                    check(wait(fixture.started.is_set),'Large upload reaches the actual socket');fixture.cancel_requested=True;navigate(w,'about:blank');check(wait(fixture.closed.is_set),'Replacement navigation releases the stalled transfer');check(child.poll() is None,'Cancelled transfer leaves the browser responsive')
                                elif name in ['identity','identity-cancel','identity-review-cancel']:
                                    f=wait(lambda:form('Titan client identity'));check(f is not None,'Server identity status opens explicit shared identity selection');check(not any(r['client_certificate'] for r in fixture.records[before:]),'Initial upload intent is anonymous')
                                    if name=='identity-cancel':click(f,565,613);check(wait(lambda:form('Titan client identity') is None),'Identity selector cancels cleanly');check(not any(r['client_certificate'] for r in fixture.records[before:]),'Cancellation sends no client identity');recovered(w,profile,target,'Titan submission stopped',before+1)
                                    else:
                                        click(f,680,613);f=wait(lambda:form('Review Titan upload'));check(f is not None,'Identity selection requires approval before retrying the upload');start=len(fixture.records);time.sleep(.15);check(len(fixture.records)==start,'Identity selection alone does not resubmit')
                                        if name=='identity-review-cancel':click(f,565,613);recovered(w,profile,target,'Titan submission stopped',before+1);check(not any(r['client_certificate'] for r in fixture.records[before:]),'Cancelled retry sends no selected identity')
                                        else:click(f,680,613);check(ready('Titan uploaded'),'Approved shared identity completes the upload');check(any(r['client_certificate'] for r in fixture.records[before:]),'Server receives the selected client identity')
                                    check(sha(catalog)==catalog_hash,'Source identity catalog is unchanged')
                                elif name in ['input','sensitive','input-cancel']:
                                    f=wait(lambda:form('Titan input'));check(f is not None,'Server input response opens native query controls')
                                    if name=='input-cancel':click(f,565,613);recovered(w,profile,target,'Titan submission stopped',before+1)
                                    else:
                                        click(f,180,392);text('query-secret-fixture' if name=='sensitive' else 'next query');click(f,680,613);f=wait(lambda:form('Review Titan upload'));check(f is not None,'Query response requires renewed submission approval');click(f,680,613);check(ready('Titan uploaded'),'Approved input continuation completes')
                                        expected=base+'/resource?next%20query#local' if name=='input' else target;check(wait(lambda:canonical(profile,expected)),'Input continuation preserves its correct public resource address')
                                elif name=='large-file':
                                    deadline=time.monotonic()+90
                                    while time.monotonic()<deadline and not any(r.get('sha256')==expected_sha for r in fixture.records[before:]):
                                        peak=max(peak,int(next(v.split()[1] for v in Path(f'/proc/{child.pid}/status').read_text().splitlines() if v.startswith('VmRSS:')))*1024);time.sleep(.05)
                                    check(any(r.get('bytes')==128*1024*1024 and r.get('sha256')==expected_sha for r in fixture.records[before:]),'128 MiB upload preserves the exact chosen bytes');check(ready('Titan uploaded'),'Large upload produces its receipt');check(peak-initial<96*1024*1024,'Large upload has bounded browser RSS growth');(folder/'stream.json').write_text(json.dumps({'bytes':128*1024*1024,'sha256':expected_sha,'rss_before':initial,'rss_peak':peak},indent=2)+'\n')
                                elif name in ['redirect','redirect-titan']:
                                    check(ready('Titan response'),'Redirect produces a destination page');time.sleep(.15);check(len(fixture.records)==before+1,'Response redirect does not automatically repeat or navigate')
                                    check(wait(lambda:link(w,0)),'Response destination is an explicit link');check(ready('Titan submission' if name=='redirect-titan' else 'Existing resource'),'Explicit destination opens without resubmission');check(len(fixture.records)==before+(1 if name=='redirect-titan' else 2),'Destination retrieval follows the selected protocol')
                                elif name in ['early-failure','failure']:check(ready('Titan response'),'Protocol failure produces a readable receipt');check(len(fixture.records)==before+1,'Failure does not retry automatically')
                                elif a.reference_root and name=='delete-file':
                                    check(ready('Titan outcome unconfirmed' if fixture.ragged_delete_receipt else 'resource-delete-file'),'Binary deletion receipt matches independently measured server completion')
                                    check(wait(lambda:fixture.records[-1].get('deleted') is True),'Author server actually removes the binary resource')
                                    record=fixture.records[-1];check(record['size']==0 and record['mime']=='application/octet-stream' and record['token_matches'],'Binary deletion uses the reviewed MIME and native token')
                                    time.sleep(.2);check(len(fixture.records)==before+1,'Incomplete deletion receipt is never retried automatically');check(child.poll() is None,'Deletion receipt handling leaves the browser responsive')
                                    if fixture.ragged_delete_receipt:recovered(w,profile,target,'Titan outcome unconfirmed',before+1)
                                elif name=='bad-header':check(ready('Titan outcome unconfirmed'),'Malformed response produces an unconfirmed outcome');recovered(w,profile,target,'Titan outcome unconfirmed',before+1)
                                else:
                                    check(ready('Titan response' if a.reference_root else 'Titan deleted' if mode==2 else 'Titan uploaded'),'Explicit submission completes')
                                    check(wait(lambda:len(fixture.records)>before and fixture.records[-1].get('bytes')==(0 if mode==2 else len(payload))),'Server stores the declared payload or completes deletion');record=fixture.records[-1];check(record['size']==record['bytes']==(0 if mode==2 else len(payload)),'Declared byte count matches the exact payload');check(record['token_matches'],'Only the supplied native token is used');check(record['query']=='username=Alex','Query remains after the intent parameters')
                                    if mode!=2:check(record['sha256']==hashlib.sha256(payload).hexdigest(),'Server receives the exact text or file bytes')
                                    else:
                                        check(record['mime']==('application/octet-stream' if name=='delete-file' else 'text/gemini'),'Deletion uses the reviewed resource MIME type')
                                        if a.reference_root:check(record.get('deleted') is True,'Author server actually removes the selected resource')
                                    if name=='reload':
                                        start=len(fixture.records);w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_r,XK.XK_Control_L);check(ready('Titan submission'),'Reload returns to preparation');check(len(fixture.records)==start,'Reload does not repeat the upload')
                    frame(w).save(folder/'frame.png');close(w);check(child.wait(timeout=15)==0,name+': browser closes cleanly');child=None
                    if name in ['restart','receipt-restart']:
                        start=len(fixture.records);child=subprocess.Popen([str(binary),'--software','--profile',str(profile)],env=env,stdout=log,stderr=log);w=wait(browser);check(w is not None and ready('Titan submission'),'Restart restores preparation');check(len(fixture.records)==start,'Restart makes no submission request');close(w);check(child.wait(timeout=15)==0,'Restored browser closes cleanly');child=None
                    files=[file for file in profile.rglob('*') if file.is_file()];check(all(b'fixture-token' not in file.read_bytes() and b'ignored-token' not in file.read_bytes() for file in files),'Intent tokens are absent from persisted profile files');check(b'fixture-token' not in (folder/'browser.log').read_bytes(),'Intent token is absent from diagnostics')
                    if name=='sensitive':check(all(b'query-secret-fixture' not in file.read_bytes() for file in files) and b'query-secret-fixture' not in (folder/'browser.log').read_bytes(),'Sensitive query is absent from saved browser state and diagnostics')
                    if name not in ['landing','parameters','restart','gemini-redirect','view','source-download','cancel-method','empty-text','empty-file','large-file','cancel-transfer'] and mode!=2:
                        check(all(payload not in file.read_bytes() for file in files),'Prepared payload is absent from saved browser state')
                    (folder/'exchanges.json').write_text(json.dumps(fixture.records[before:],indent=2)+'\n');runs.append({'name':name,'passed':True,'launch_seconds':launch_seconds})
                finally:
                    (folder/'exchanges.json').write_text(json.dumps(fixture.records[before:],indent=2)+'\n')
                    if sys.exc_info()[0] is not None:
                        for v in windows():
                            try:frame(v).save(folder/f'failure-{v.id}.png')
                            except Exception:pass
                    if child and child.poll() is None:child.terminate();child.wait(timeout=10)
                    child=None
        check(not fixture.errors,'Fixture requests remain within the declared Titan/Gemini formats')
finally:
    if child and child.poll() is None:child.kill();child.wait()
    if fixture:fixture.close()
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    (out/'report.json').write_text(json.dumps({'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'fixture_sha256':sha(Path(__file__).with_name('titan_reference_fixture.py' if a.reference_root else 'titan_fixture.py')),'cases':runs,'checks':checks,'passed':len(runs)==len(a.cases) and bool(checks) and all(c['passed'] for c in checks),'native_wayland':bool(a.wayland),'reference_server':bool(a.reference_root),'scope':'Private profiles, private bus and desktops, loopback TLS only; no public uploads'},indent=2)+'\n')
