#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Default URI dispatch with private handlers, profiles, bus and display."""
import argparse,hashlib,http.server,json,os,signal,subprocess,sys,tempfile,threading,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_EXTERNAL_LINKS_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_EXTERNAL_LINKS_TEST_BUS':'1'})
from PIL import Image
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--wayland',action='store_true')
a=p.parse_args();binary=a.binary.resolve();out=a.output.resolve();assert not out.exists();out.mkdir(parents=True,mode=0o700)
checks=[];child=xvfb=connection=server=wayland=None
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def check(value,name):checks.append({'name':name,'passed':bool(value)});assert value,name
def wait(fn,seconds=12):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        value=fn()
        if value:return value
        time.sleep(.05)
    return None
def windows():
    if wayland:return wayland.windows()
    result=[]
    for w in connection.screen().root.query_tree().children:
        try:
            if w.get_attributes().map_state==X.IsViewable:result.append(w)
        except Exception:pass
    return result
def browser_window():return next((w for w in windows() if (str(w.get_wm_name() or '').endswith(' — Dome Browser') if wayland else w.get_geometry().width>=1000)),None)
def frame(w):
    g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
    return Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
def painted(w):return any(hi-lo>80 for lo,hi in frame(w).crop((20,145,800,450)).getextrema())
def key(symbol,mod=None):
    if mod:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(mod))
    code=connection.keysym_to_keycode(symbol);xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
    if mod:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(mod))
    connection.sync();time.sleep(.03)
def text(value):
    for c in value:
        symbol=XK.string_to_keysym(c) or ord(c);code=connection.keysym_to_keycode(symbol)
        shift=connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
        key(symbol,XK.XK_Shift_L if shift else None)
def click(w,x,y):
    g=w.get_geometry();w.set_input_focus(X.RevertToParent,X.CurrentTime)
    xtest.fake_input(connection,X.MotionNotify,x=g.x+x,y=g.y+y);xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync();time.sleep(.15)
def navigate(w,uri):
    w.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_l,XK.XK_Control_L);key(XK.XK_a,XK.XK_Control_L);text(uri);key(XK.XK_Return)
def close(w):
    if wayland:
        wayland.api('/api/action',{'op':'close','id':w.id});return
    w.send_event(protocol.event.ClientMessage(window=w,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);connection.sync()
mail='mailto:fixture@example.invalid?subject=Local%20fixture&body=First%20line%0ASecond%20line'
misfin='misfin://Mailbox@localhost:1959'
magnet='magnet:?xt=urn:btih:0123456789012345678901234567890123456789&dn=Local%20fixture&tr=https%3A%2F%2Fexample.invalid%2Fannounce'
try:
 with tempfile.TemporaryDirectory(prefix='dome-external-links-') as temporary:
    root=Path(temporary);root.chmod(0o700);config=root/'config';data=root/'data'
    for d in [config,data,root/'system-data',root/'system-config']:d.mkdir(mode=0o700)
    record=root/'dispatch.jsonl';helper=root/'fixture handler.py'
    helper.write_text('#!/usr/bin/python3\nimport json,os,sys\nwith open(os.environ["DOME_EXTERNAL_TEST_RECORD"],"a") as f:f.write(json.dumps({"args":sys.argv[1:],"realm":os.environ.get("WAYLAND_DISPLAY"),"desktop":os.environ.get("XDG_CURRENT_DESKTOP")})+"\\n")\n');helper.chmod(0o700)
    applications=data/'applications';applications.mkdir();app=applications/'fixture.desktop'
    app.write_text('[Desktop Entry]\nType=Application\nName=Fixture handler\nExec="'+str(helper)+'" %u\nMimeType=x-scheme-handler/mailto;x-scheme-handler/misfin;x-scheme-handler/magnet;\nNoDisplay=true\n')
    preferences='[Default Applications]\nx-scheme-handler/mailto=fixture.desktop;\nx-scheme-handler/magnet=fixture.desktop;\nx-scheme-handler/misfin=fixture.desktop;\n'
    scoped=config/'dome-mimeapps.list';scoped.write_text(preferences);original=scoped.read_bytes()
    env={**os.environ,'HOME':str(root),'XDG_DATA_HOME':str(data),'XDG_CONFIG_HOME':str(config),'XDG_DATA_DIRS':str(root/'system-data'),'XDG_CONFIG_DIRS':str(root/'system-config'),'XDG_CACHE_HOME':str(root/'cache'),'XDG_STATE_HOME':str(root/'state'),'XDG_RUNTIME_DIR':str(root),'XDG_CURRENT_DESKTOP':'Dome','WAYLAND_DISPLAY':'private-fixture-realm','DOME_EXTERNAL_TEST_RECORD':str(record),'LIBGL_ALWAYS_SOFTWARE':'1','RUST_LOG':'warn'}
    env.pop('WAYLAND_SOCKET',None)
    def deliveries():return [json.loads(line) for line in record.read_text().splitlines()] if record.exists() else []
    for uri in [mail,magnet,misfin]:
        start=len(deliveries());result=subprocess.run([str(binary),'--profile',str(root/'cli-profile'),uri],env=env,capture_output=True,timeout=15)
        check(result.returncode==0,'Explicit '+uri.split(':')[0]+' URI exits successfully after dispatch')
        check(wait(lambda:len(deliveries())==start+1),'Default fixture handler receives the URI')
        check(deliveries()[-1]['args']==[uri],'Handler receives one intact URI, including all parameters')
        check(deliveries()[-1]['realm']=='private-fixture-realm','Handler inherits the Wayland realm')
    scoped.unlink();standard=config/'mimeapps.list';standard.write_text(preferences)
    result=subprocess.run([str(binary),mail],env=env,capture_output=True,timeout=15)
    check(result.returncode==0 and wait(lambda:len(deliveries())==4),'Standard desktop default is used without a DomeD override')
    standard.unlink();scoped.write_text('[Default Applications]\nx-scheme-handler/mailto=missing.desktop;\n');start=len(deliveries())
    result=subprocess.run([str(binary),mail],env=env,capture_output=True,timeout=15)
    check(result.returncode!=0 and len(deliveries())==start,'Missing configured handler reports failure without choosing another app')
    scoped.write_text('not valid preferences')
    result=subprocess.run([str(binary),mail],env=env,capture_output=True,timeout=15)
    check(result.returncode!=0 and len(deliveries())==start,'Malformed default preferences are preserved without dispatch')
    scoped.write_text('[Default Applications]\nx-scheme-handler/mailto=fixture.desktop;\nx-scheme-handler/misfin=missing.desktop;\n')
    result=subprocess.run([str(binary),misfin],env=env,capture_output=True,timeout=15)
    check(result.returncode!=0 and len(deliveries())==start,'Misfin without its configured handler does not fall back to ordinary email')
    recursive=applications/'org.domed.Browser.desktop'
    recursive.write_text('[Desktop Entry]\nType=Application\nName=Recursive fixture\nExec="'+str(binary)+'" %u\nMimeType=x-scheme-handler/misfin;\n')
    scoped.write_text('[Default Applications]\nx-scheme-handler/misfin=org.domed.Browser.desktop;\n')
    result=subprocess.run([str(binary),misfin],env=env,capture_output=True,timeout=15)
    check(result.returncode!=0 and len(deliveries())==start,'Browser cannot select itself as its external Misfin handler')
    scoped.write_text(preferences)
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_GET(self):
            body=('<!doctype html><title>External link fixture</title><style>body{background:#fafafa;color:#111}a{position:absolute;left:24px;font:24px monospace}a:nth-child(1){top:30px}a:nth-child(2){top:100px}a:nth-child(3){top:170px}</style><a href="'+mail.replace('&','&amp;')+'">Mail fixture</a><a href="'+magnet.replace('&','&amp;')+'">Torrent fixture</a><a href="'+misfin+'">Misfin fixture</a>').encode()
            self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8');self.end_headers();self.wfile.write(body)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start();base='http://127.0.0.1:'+str(server.server_port)+'/'
    read,write=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(write),'-screen','0','1440x1000x24','-nolisten','tcp'],pass_fds=(write,),stderr=subprocess.DEVNULL);os.close(write)
    with os.fdopen(read) as f:screen=':'+f.readline().strip()
    connection=display.Display(screen);env.update(DISPLAY=screen,FLTK_BACKEND='x11');env.pop('WAYLAND_DISPLAY',None)
    if a.wayland:
        from private_wayland import PrivateWayland
        wayland=PrivateWayland(connection,screen,root,out)
        env.pop('DISPLAY',None);env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket)
    with (out/'browser.log').open('w') as log:
        child=subprocess.Popen([str(binary),'--software','--no-restore','--profile',str(root/'profile'),base],env=env,stdout=log,stderr=log)
        browser=wait(browser_window);check(browser is not None and wait(lambda:painted(browser)),'Browser paints the private link fixture')
        frame(browser).save(out/'links.png')
        for uri in [mail,magnet,misfin]:
            start=len(deliveries());navigate(browser,uri)
            check(wait(lambda:len(deliveries())==start+1),'Address bar dispatches '+uri.split(':')[0]+' through the selected handler')
            check(deliveries()[-1]['args']==[uri],'Address bar preserves the URI parameters')
            check('External link fixture' in str(browser.get_wm_name()),'Address-bar handoff preserves the current page')
        for uri in [mail,magnet,misfin]:
            start=len(deliveries())
            result=subprocess.run(['gdbus','call','--session','--dest','org.domed.Browser','--object-path','/org/domed/Browser','--method','org.freedesktop.Application.Open',json.dumps([uri]),'{}'],capture_output=True,timeout=10)
            check(result.returncode==0 and wait(lambda:len(deliveries())==start+1),'Desktop Open dispatches '+uri.split(':')[0]+' to its handler')
            check(deliveries()[-1]['args']==[uri],'Desktop Open preserves the intact URI')
            check('External link fixture' in str(browser.get_wm_name()),'Desktop Open handoff retains the current page')
        start=len(deliveries());oversized=mail+'&extra='+'x'*9000
        result=subprocess.run(['gdbus','call','--session','--dest','org.domed.Browser','--object-path','/org/domed/Browser','--method','org.freedesktop.Application.Open',json.dumps([oversized]),'{}'],capture_output=True,timeout=10)
        time.sleep(.3)
        check(result.returncode==0 and len(deliveries())==start and child.poll() is None,'Oversized native Open input is rejected without truncation or an application launch')
        # Click each link, decline once, then explicitly allow the handoff.
        page_top=next(y for y in range(80,145) if frame(browser).getpixel((4,y))==(250,250,250))
        for y,uri in [(page_top+42,mail),(page_top+112,magnet),(page_top+182,misfin)]:
            start=len(deliveries());click(browser,160,y)
            dialog=wait(lambda:next((w for w in windows() if w.get_wm_name()=='Open default application'),None))
            check(dialog is not None,'Page link opens a native external-application confirmation')
            check(len(deliveries())==start,'Page link is not dispatched before native approval')
            frame(dialog).save(out/(uri.split(':')[0]+'-confirmation.png'));dialog.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_Escape)
            check(wait(lambda:not any(w.get_wm_name()=='Open default application' for w in windows())),'Declining the page request closes its prompt')
            check(len(deliveries())==start,'Declining a page request does not launch an application')
            click(browser,160,y);dialog=wait(lambda:next((w for w in windows() if w.get_wm_name()=='Open default application'),None));check(dialog is not None,'Page link can be selected again')
            dialog.set_input_focus(X.RevertToParent,X.CurrentTime);key(XK.XK_Return)
            check(wait(lambda:len(deliveries())==start+1),'Approved page request reaches the selected default handler')
            check(deliveries()[-1]['args']==[uri],'Page handoff preserves all mail or torrent parameters')
            check('External link fixture' in str(browser.get_wm_name()),'Page handoff leaves the document open')
        check(scoped.read_bytes()==original,'Default application preferences remain unchanged')
        close(browser);check(child.wait(timeout=10)==0,'Browser closes cleanly after URI handoffs');child=None
    print('PASS private default handlers, CLI, address bar, native page confirmation, URI preservation and clean shutdown')
finally:
    if child is not None and child.poll() is None:child.kill();child.wait()
    if wayland:wayland.close()
    if connection:connection.close()
    if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
    if server:server.shutdown();server.server_close()
    (out/'report.json').write_text(json.dumps({'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'checks':checks,'passed':bool(checks) and all(c['passed'] for c in checks),'native_wayland':bool(a.wayland),'scope':'Private local handler applications only; no real mail client, torrent client, mailbox or torrent network activity.'},indent=2)+'\n')
