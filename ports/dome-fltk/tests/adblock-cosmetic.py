#!/usr/bin/env python3
"""Verify native cosmetic filters hide ads without hiding legitimate content."""
import argparse
import hashlib
import asyncio
from urllib.parse import urlsplit
import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time

signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_COSMETIC_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_COSMETIC_TEST_BUS': '1'})
from Xlib import X, XK, display
from Xlib.ext import xtest
from PIL import Image
from dbus_next.aio import MessageBus
from dbus_next import Variant

repo = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, default=repo / 'target/release/dome-browser')
p.add_argument('--output', type=Path, required=True)
p.add_argument("--baseline",action="store_true",help="Record the absence of cosmetic filtering")
a = p.parse_args()
out = a.output.resolve(); out.mkdir(parents=True, exist_ok=False)
checks = []; children = []; logs = []; reports = []
def check(ok, name):
    checks.append({'check':name, 'passed':bool(ok)})
    print(('PASS ' if ok else 'FAIL ') + name, flush=True)
    assert ok, name
def wait(callback, seconds=30):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        try:
            value = callback()
            if value: return value
        except (OSError, ValueError): pass
        time.sleep(.05)
    raise AssertionError('Timed out; inspect ' + str(out))
def start(command, name, env, **kwargs):
    log = (out / (name + '.log')).open('w'); logs.append(log)
    proc = subprocess.Popen(command, env=env, stdout=log, stderr=log, **kwargs)
    children.append(proc); return proc

try:
 with tempfile.TemporaryDirectory(prefix='dome-cosmetic-') as directory:
    runtime=Path(directory);runtime.chmod(0o700)
    fixture=Path(__file__).with_suffix('.html').read_bytes()
    requests=[];observations=[]
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*_):pass
        def do_POST(self):
            reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204);self.send_header('Content-Length','0');self.end_headers()
        def do_GET(self):
            path=urlsplit(self.path).path;requests.append(path)
            data=fixture;kind='text/html; charset=utf-8'
            script_policy="'nonce-fixture'";style_policy="'nonce-fixture'"
            if path=='/strict':style_policy="'none'"
            if path=='/frame-host':
                data=fixture+f'<iframe referrerpolicy="no-referrer" src="http://localhost:{self.server.server_port}/frame-child"></iframe>'.encode()
            if path=='/no-script':
                script_policy="'none'"
                data=b"""<!doctype html><title>Cosmetic without scripts</title><style nonce=fixture>
body{background:white}.dome-ad-generic{position:fixed;left:20px;top:20px;width:100px;height:100px;background:red;display:block!important}
.legit{position:fixed;left:20px;top:140px;width:100px;height:100px;background:lime}</style>
<div class=dome-ad-generic></div><div class=legit></div><script>fetch('/script-executed')</script>"""
            if path in ['/svg','/svg-script-control']:
                kind='image/svg+xml';script_policy="'none'"
                data=b'<svg xmlns="http://www.w3.org/2000/svg" width="400" height="400"><title>Cosmetic SVG</title><rect class="dome-ad-generic" x="20" y="20" width="100" height="100" fill="red"/><rect x="20" y="140" width="100" height="100" fill="cyan"/></svg>'
                if path=='/svg-script-control':
                    script_policy="'nonce-fixture'"
                    script="fetch('/report',{method:'POST',body:JSON.stringify({url:location.href,originalURL:location.href,values:{},xml:document.contentType,root:document.documentElement.namespaceURI,title:document.title})});"
                else:script="fetch('/svg-script-executed');"
                data=data.replace(b'</svg>',('<foreignObject><script xmlns="http://www.w3.org/1999/xhtml" nonce="fixture">'+script+'</script></foreignObject></svg>').encode())
            self.send_response(200);self.send_header('Content-Type',kind)
            self.send_header('Content-Security-Policy',f"default-src 'self'; script-src {script_policy}; style-src {style_policy}; frame-src http://localhost:{self.server.server_port}")
            self.send_header('Content-Length',str(len(data)));self.send_header('Cache-Control','no-store');self.end_headers();self.wfile.write(data)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True
    origin='http://127.0.0.1:'+str(server.server_port)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    env = {**os.environ, 'FLTK_BACKEND':'x11', 'XDG_RUNTIME_DIR':directory,
           'XDG_CONFIG_HOME':str(runtime/'config'), 'XDG_DATA_HOME':str(runtime/'data'),
           'XDG_CACHE_HOME':str(runtime/'cache'), 'XDG_CURRENT_DESKTOP':'Dome',
           'LIBGL_ALWAYS_SOFTWARE':'1', 'XMODIFIERS':'@im=none', 'RUST_LOG':'warn', 'DOME_BROWSER_TRACE':'1'}
    for key in ['DISPLAY','WAYLAND_DISPLAY','WAYLAND_SOCKET','DOME_BROWSER_EXTERNAL_PLAYER']:
        env.pop(key,None)
    rd, wr = os.pipe()
    start(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1000x24','-nolisten','tcp'], 'xvfb', env, pass_fds=(wr,))
    os.close(wr)
    with os.fdopen(rd) as stream: env['DISPLAY'] = ':' + stream.readline().strip()
    xd = display.Display(env['DISPLAY'])
    profile=runtime/'profile';profile.mkdir(mode=0o700)
    # EasyList intentionally exempts loopback hosts from generic hiding. Use a
    # controlled list cache to test rule semantics, with blocking still enabled.
    (profile/'filterlists.json').write_text('[]')
    (profile/'adblock.txt').write_text('##.dome-ad-generic\n###dome-ad-id\n##.dome-ad-excepted\n127.0.0.1#@#.dome-ad-excepted\n127.0.0.1##.dome-ad-site\nlocalhost##.dome-ad-other\n@@/generichide$generichide\n')
    app=start([str(a.binary.resolve()),'--no-restore','--profile',str(profile),origin+'/normal'],'browser',env)
    def window(title):return wait(lambda:next((w for w in xd.screen().root.query_tree().children if title in str(w.get_wm_name()or'')),None))
    main=window('Cosmetic filter fixture')
    def key(name, control=False, target=None):
        (target or main).set_input_focus(X.RevertToParent,X.CurrentTime)
        if control: xtest.fake_input(xd,X.KeyPress,xd.keysym_to_keycode(XK.XK_Control_L))
        code=xd.keysym_to_keycode(XK.string_to_keysym(name))
        xtest.fake_input(xd,X.KeyPress,code);xtest.fake_input(xd,X.KeyRelease,code)
        if control: xtest.fake_input(xd,X.KeyRelease,xd.keysym_to_keycode(XK.XK_Control_L))
        xd.sync()
    def type_text(text):
        for char in text:
            if char == "\n":
                code = xd.keysym_to_keycode(XK.XK_Return)
                xtest.fake_input(xd,X.KeyPress,code);xtest.fake_input(xd,X.KeyRelease,code)
                continue
            sym = XK.string_to_keysym(char) or ord(char)
            code = xd.keysym_to_keycode(sym)
            shifted = xd.keycode_to_keysym(code,1) == sym
            if shifted: xtest.fake_input(xd,X.KeyPress,xd.keysym_to_keycode(XK.XK_Shift_L))
            xtest.fake_input(xd,X.KeyPress,code);xtest.fake_input(xd,X.KeyRelease,code)
            if shifted: xtest.fake_input(xd,X.KeyRelease,xd.keysym_to_keycode(XK.XK_Shift_L))
        xd.sync()
    def latest(url, after=0):return next((r for r in reversed(reports[after:])if r['url']==url),None)
    def hidden(values,name):return values[name]['display']=='none'and values[name]['height']==0
    def settled(url, hidden_names, visible_names, after=0):
        def ready():
            r=latest(url,after)
            if r and all(n in r['values']for n in hidden_names+visible_names):
                v=r['values']
                if all(hidden(v,n)for n in hidden_names)and all(v[n]['display']!='none'and v[n]['height']>0 for n in visible_names):return r
        r=wait(ready,seconds=15);observations.append(r);return r
    ads=['generic','id','site','changed','inserted']
    content=['excepted','other','legitimate']
    url=origin+'/normal'
    if a.baseline:
        r=settled(url,[],ads+content)
        check(True,'baseline has no cosmetic hiding')
    else:
        r=settled(url,ads,content)
        for name in ads:check(hidden(r['values'],name),name+' is hidden, including dynamic content and author-important rules')
        for name in content:check(r['values'][name]['height']>0,name+' remains visible')
        check(r['sheets']==r['styles']==1,'browser cosmetic styles are absent from page CSSOM and DOM')
        service=main.get_full_property(xd.intern_atom('_KDE_NET_WM_APPMENU_SERVICE_NAME'),X.AnyPropertyType)
        menu_path=main.get_full_property(xd.intern_atom('_KDE_NET_WM_APPMENU_OBJECT_PATH'),X.AnyPropertyType)
        async def menu_action(label):
            bus=await MessageBus().connect()
            try:
                name=bytes(service.value).decode();path=bytes(menu_path.value).decode()
                intro=await bus.introspect(name,path)
                interface=bus.get_proxy_object(name,path,intro).get_interface('com.canonical.dbusmenu')
                _,layout=await interface.call_get_layout(0,-1,[])
                def find(node):
                    while isinstance(node,Variant):node=node.value
                    identifier,props,children=node
                    if props.get('label',Variant('s','')).value==label:return identifier
                    for child in children:
                        found=find(child)
                        if found is not None:return found
                identifier=find(layout);assert identifier is not None,label
                await interface.call_event(identifier,'clicked',Variant('i',0),0)
            finally:bus.disconnect()
        def menu(label):asyncio.run(menu_action(label))
        def navigate(url):
            after=len(reports);key('l',True);type_text(url);key('Return');return after
        after=len(reports);menu('Enable or disable');settled(url,[],ads+content,after)
        check(True,'native disable removes existing cosmetic styles without reload')
        after=len(reports);menu('Enable or disable');settled(url,ads,content,after)
        check(True,'native enable re-evaluates existing content without reload')
        after=len(reports);menu('Toggle exception for this site');settled(url,[],ads+content,after)
        check(True,'site exception removes existing styles without reload')
        after=len(reports);menu('Toggle exception for this site');settled(url,ads,content,after)
        check(True,'removing site exception restores cosmetic protection')
        rules=profile/'adblock.txt';original=rules.read_text();rules.write_text(original+'127.0.0.1##.dome-legitimate\n')
        after=len(reports);menu('Reload lists and custom rules');settled(url,ads+['legitimate'],['excepted','other'],after)
        check(True,'reloading custom rules applies new hiding to existing content')
        rules.write_text(original);after=len(reports);menu('Reload lists and custom rules');settled(url,ads,content,after)
        check(True,'removing a rule and reloading restores its content')
        url=origin+'/strict';after=navigate(url);r=settled(url,ads,content,after)
        check(r['sheets']==0,'native hiding works under style-src none without adding an author stylesheet')
        url=origin+'/generichide';after=navigate(url)
        settled(url,['site'],['generic','id','changed','inserted']+content,after)
        check(True,'generichide exception suppresses generic rules and retains site rules')
        url=origin+'/normal-again';after=navigate(url);settled(url,ads,content,after)
        check(True,'generic-hide exception does not leak to a different URL')
        url=origin+'/history';after=navigate(url);settled(url,ads,content,after)
        settled(origin+'/generichide-history',['site'],['generic','id','changed','inserted']+content,after)
        settled(origin+'/normal-history',ads,content,after)
        check('/generichide-history'not in requests and'/normal-history'not in requests,'pushState/replaceState refresh URL exceptions without a new document request')
        other_origin='http://localhost:'+str(server.server_port);url=other_origin+'/other';after=navigate(url)
        settled(url,['generic','id','changed','inserted','excepted','other'],['site','legitimate'],after)
        check(True,'hostname-specific hiding and unhide exceptions stay scoped to their host')
        after=len(reports);menu('Toggle exception for this site');settled(url,[],ads+content,after)
        check(True,'other top-level origin can be exempted independently')
        url=origin+'/frame-host';after=navigate(url);settled(url,ads,content,after)
        child=other_origin+'/frame-child';settled(child,['generic','id','changed','inserted','excepted','other'],['site','legitimate'],after)
        check(True,'an exempt cross-site frame remains filtered with no referrer under a non-exempt top-level site')
        after=len(reports);menu('Toggle exception for this site');settled(url,[],ads+content,after)
        settled(child,['generic','id','changed','inserted','excepted','other'],['site','legitimate'],after)
        check(True,'tainted cross-site frame context stays filtered when its parent becomes exempt')
        # The no-script fixture uses the already-exempt parent host: remove that
        # exception first, then prove hiding through pixels with scripts denied.
        menu('Toggle exception for this site')
        navigate(origin+'/no-script');wait(lambda:'/no-script'in requests)
        def rendered(cyan=False):
            g=main.get_geometry();raw=main.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
            im=Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
            colors=im.getcolors(g.width*g.height)or[]
            green=sum(n for n,c in colors if c[1]>240 and c[0]<10 and ((c[2]>240)if cyan else(c[2]<10)))
            red=sum(n for n,c in colors if c[0]>240 and c[1]<10 and c[2]<10)
            if green>8000 and red==0:return im
        screenshot=wait(rendered,seconds=15);screenshot.save(out/'no-script.png')
        check('/script-executed'not in requests,'native cosmetic filtering hides the red ad while the green control paints with script-src none')
        navigate(origin+'/svg');wait(lambda:'/svg'in requests)
        screenshot=wait(lambda:rendered(cyan=True),seconds=15);screenshot.save(out/'svg-no-script.png')
        check('/svg-script-executed'not in requests,'native SVG cosmetic hiding works while CSP blocks embedded XHTML scripts')
        after=navigate(origin+'/svg-script-control')
        r=wait(lambda:latest(origin+'/svg-script-control',after))
        check(r.get('xml')=='image/svg+xml'and r.get('root')=='http://www.w3.org/2000/svg','SVG navigation creates an XML SVG document, with a CSP-allowed XHTML script as positive control')
        check(r.get('title')=='Cosmetic SVG','SVG document.title returns its root title element text')
        (out/'svg-title-observation.json').write_text(json.dumps({'document_title':r.get('title'),'native_window_title':str(main.get_wm_name())},indent=2)+'\n')
        observations.append(r)
    (out/'observations.json').write_text(json.dumps(observations,indent=2)+'\n')
    key('q',True);app.wait(timeout=15);check(app.returncode==0,'browser closes cleanly')
    (out/'report.json').write_text(json.dumps({'passed':True,'baseline':a.baseline,'binary_sha256':hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest(),'checks':checks},indent=2)+'\n')
finally:
 (out/'fixture-reports.json').write_text(json.dumps({'reports':reports,'requests':locals().get('requests',[])},indent=2)+'\n')
 for child in reversed(children):
    if child.poll()is None:
        child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
 for log in logs:log.close()
