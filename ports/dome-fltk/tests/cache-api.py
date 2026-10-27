#!/usr/bin/env python3
"""Exercise native Cache APIs, origin isolation and persistence across restart."""
import argparse
import base64
import hashlib
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
from urllib.parse import urlsplit, parse_qs
from Xlib import X, display, protocol

signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_CACHE_API_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_CACHE_API_TEST_BUS':'1'})
source=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,default=source/'target/release/dome-browser')
p.add_argument('--output',type=Path,required=True)
p.add_argument('--reference-browser',type=Path)
p.add_argument('--fixture',type=Path,default=Path(__file__).with_suffix('.html'))
p.add_argument('--local-file-controls',action='store_true',help='Create only synthetic local resources for file authority tests')
p.add_argument('--local-top-level',action='store_true',help='Navigate directly to the synthetic local fixture page; requires --local-file-controls')
p.add_argument('--svg-image-controls',action='store_true',help='Enable fixture cookie and CSP image policy headers')
p.add_argument('--fixture-filters',type=Path,help='Seed only the temporary fixture profile with these content filters')
p.add_argument('--disable-adblock',action='store_true',help='Disable content filtering only in this temporary fixture profile')
p.add_argument('--capture-exceptions',action='store_true',help='Collect reference browser exception events for error-reporting tests')
p.add_argument('--capture-frame',action='store_true',help='Save the rendered content frame before closing')
p.add_argument('--timeout',type=float,default=150,help='Seconds to wait for the fixture result')
p.add_argument('--host',default='127.0.0.1')
p.add_argument('--path',default='/')
p.add_argument('--stages',nargs='+',choices=['write','read'],default=['write','read'])
a=p.parse_args()
if a.local_top_level and not a.local_file_controls:p.error('--local-top-level requires --local-file-controls')
out=a.output.resolve();out.mkdir(parents=True,exist_ok=True)
page=a.fixture.read_bytes();(out/'fixture.html').write_bytes(page);phase='write';reports=[];requests=[]
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_GET(self):
        path=urlsplit(self.path).path;requests.append({'path':self.path,'phase':phase,'port':self.server.server_port,'cookie':self.headers.get('Cookie',''),'origin':self.headers.get('Origin'),'referer':self.headers.get('Referer'),'time':time.monotonic()})
        status=200;kind='text/plain';headers={}
        if a.local_file_controls and path=='/local-fixture-config.json':
            data=json.dumps({'localImage':local_fixture_uri,'serverOrigin':'http://127.0.0.1:'+str(primary.server_port)}).encode();kind='application/json';headers['Access-Control-Allow-Origin']='*'
        elif a.local_file_controls and path=='/file-boundary-worker.js':
            probe=(Path(__file__).parent/'file-boundary-worker.js').read_text()
            data=('onmessage=async({data:cfg})=>{('+probe+')(cfg).then(result=>postMessage(result))}').encode();kind='application/javascript'
        elif path=='/svg-image.svg':
            query=parse_qs(urlsplit(self.path).query)
            time.sleep(max(0,min(float(query.get('delay',['0'])[0]),2000))/1000)
            color=query.get('color',['green'])[0]
            assert color in ['red','green','blue'], 'unsupported fixture color'
            data=('<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="'+color+'"/></svg>').encode();kind='image/svg+xml'
            cors=query.get('cors',['none'])[0]
            if cors=='anonymous':headers['Access-Control-Allow-Origin']='*'
            if cors in ['credentials','missing-credentials']:headers['Access-Control-Allow-Origin']=self.headers.get('Origin','null')
            if cors=='credentials':headers['Access-Control-Allow-Credentials']='true'
        elif path=='/timing-control.svg':
            query=parse_qs(urlsplit(self.path).query)
            # Bounded delays exercise image completion/removal races without
            # requiring external networking or touching a desktop session.
            time.sleep(max(0,min(float(query.get('delay',['0'])[0]),2000))/1000)
            data=b'<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="green"/></svg>'
            if query.get('fail')==['1']:data=b'not a valid SVG image'
            kind='image/svg+xml'
        elif path=='/config.json':data=json.dumps({'phase':phase,'crossOrigin':f'http://127.0.0.1:{foreign.server_port}','credentialCacheTag':str(primary.server_port)+'-credential-cache'}).encode();kind='application/json'
        elif path=='/foreign-document':
            primary_origin=parse_qs(urlsplit(self.path).query)['primary'][0]
            data=('''<!doctype html><script>(async()=>{try{const names=await caches.keys();const cache=await caches.open('foreign');await cache.put(PRIMARY+'/secret',new Response('foreign'));parent.postMessage({names},'*')}catch(error){parent.postMessage({error:String(error)},'*')}})();</script>'''.replace('PRIMARY',json.dumps(primary_origin))).encode();kind='text/html'
        elif path=='/policy-worker.js':
            query=parse_qs(urlsplit(self.path).query);data=b"postMessage('executed')";kind='text/javascript'
            if 'policy'in query:headers['Cross-Origin-Embedder-Policy']=query['policy'][0]
        elif path=='/policy-child':
            query=parse_qs(urlsplit(self.path).query);tag=query['tag'][0];kind='text/html'
            if 'policy'in query:headers['Cross-Origin-Embedder-Policy']=query['policy'][0]
            data=('<script>parent.postMessage({policyChild:'+json.dumps(tag)+'},"*")</script>').encode()
            if 'self_nav'in query:data=('<script>location.replace('+json.dumps('/policy-child?tag='+tag+'&final=1')+')</script>').encode()
        elif path=='/policy-log':
            tag=parse_qs(urlsplit(self.path).query)['tag'][0];data=json.dumps([record for record in requests if urlsplit(record['path']).path=='/policy-child'and parse_qs(urlsplit(record['path']).query).get('tag')==[tag]]).encode();kind='application/json'
        elif path=='/cookie-set':data=b'cookie set';headers['Set-Cookie']='__dome_coep=present; Path=/; SameSite=Lax; HttpOnly'
        elif path=='/request-headers':
            tag=parse_qs(urlsplit(self.path).query)['tag'][0]
            data=json.dumps([record for record in requests if urlsplit(record['path']).path in ['/credential-test','/credential-cache']and parse_qs(urlsplit(record['path']).query).get('tag')==[tag]]).encode();kind='application/json'
        elif path in ['/credential-test','/credential-cache']:
            data=self.headers.get('Cookie','').encode()
            if path=='/credential-cache':headers['Cache-Control']='public, max-age=3600'
        elif path=='/font-api.ttf':
            query=parse_qs(urlsplit(self.path).query)
            time.sleep(min(float(query.get('delay',['0'])[0]),2))
            data=(source/'components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans-Bold.ttf').read_bytes();kind='font/ttf'
            if query.get('cors')==['1']:headers['Access-Control-Allow-Origin']='*'
        elif path=='/not-found':data=b'not found';status=404
        elif path=='/batch-stream':
            # Keep a successful sibling's body reader pending while another
            # addAll response fails. This exercises synchronous abort reentry.
            self.send_response(200);self.send_header('Content-Length','104');self.end_headers()
            try:
                self.wfile.write(b'body');self.wfile.flush()
                for _ in range(100):
                    time.sleep(.02);self.wfile.write(b'x');self.wfile.flush()
            except (BrokenPipeError,ConnectionResetError):pass
            return
        elif path=='/batch-failure':
            time.sleep(.25);mode=parse_qs(urlsplit(self.path).query).get('mode',['404'])[0]
            data=b'failed batch response'
            if mode=='vary':headers['Vary']='*'
            else:status=int(mode)
        elif path=='/asset':data=('asset:'+urlsplit(self.path).query).encode()
        elif path in ['/opaque','/opaque-vary','/opaque-corp']:
            data=b'private opaque body\x00\xff'
            if path=='/opaque-vary':headers['Vary']='*'
            if path=='/opaque-corp':headers['Cross-Origin-Resource-Policy']='cross-origin'
        else:
            data=page;kind='text/html; charset=utf-8'
            if path=='/coep-document':
                query=parse_qs(urlsplit(self.path).query)
                name='Cross-Origin-Embedder-Policy-Report-Only'if 'report_only'in query else'Cross-Origin-Embedder-Policy'
                headers[name]=query.get('policy',['require-corp'])[0]
        if a.svg_image_controls and path=='/':
            headers['Set-Cookie']='svg-cookie=fixture; SameSite=Lax; Path=/'
            policy=parse_qs(urlsplit(self.path).query).get('policy',['allow'])[0]
            if policy=='none':headers['Content-Security-Policy']="img-src 'none'"
            if policy=='self':headers['Content-Security-Policy']="img-src 'self'"
        self.send_response(status);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(data)))
        for name,value in headers.items():self.send_header(name,value)
        self.end_headers()
        try:self.wfile.write(data)
        except (BrokenPipeError,ConnectionResetError):pass
    def do_POST(self):
        data=self.rfile.read(int(self.headers['Content-Length']))
        if urlsplit(self.path).path=='/report':reports.append(json.loads(data))
        self.send_response(204)
        if a.local_file_controls:self.send_header('Access-Control-Allow-Origin','*')
        self.end_headers()
primary=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
foreign=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
for server in [primary,foreign]:threading.Thread(target=server.serve_forever,daemon=True).start()
app=xvfb=connection=None;runs=[]
try:
    with tempfile.TemporaryDirectory(prefix='dome-cache-api-')as directory:
        rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1280x900x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
        with os.fdopen(rd)as stream:screen=':'+stream.readline().strip()
        connection=display.Display(screen)
        env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','XDG_RUNTIME_DIR':directory,
             'XDG_CONFIG_HOME':directory+'/config','XDG_CACHE_HOME':directory+'/cache','XDG_DATA_HOME':directory+'/data'}
        for key in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(key,None)
        if a.disable_adblock:
            fixture_profile=Path(directory)/'profile';fixture_profile.mkdir(exist_ok=True)
            (fixture_profile/'adblock-disabled').write_text('disabled for isolated engine fixture\n')
        if a.fixture_filters:
            fixture_profile=Path(directory)/'profile';fixture_profile.mkdir(exist_ok=True)
            (fixture_profile/'filterlists.json').write_text('[]')
            (fixture_profile/'adblock.txt').write_text(a.fixture_filters.read_text())
        if a.local_file_controls:
            local_fixture=Path(directory)/'synthetic-local-image.svg'
            local_fixture.write_text('<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="green"/></svg>')
            local_fixture_uri=local_fixture.as_uri()
            local_page=Path(directory)/'synthetic-local-page.html'
            local_page.write_bytes(('<base href="http://127.0.0.1:'+str(primary.server_port)+'/">').encode()+page)
        for stage in a.stages:
            phase=stage;folder=out/stage;folder.mkdir(exist_ok=True);reference=None;before=len(reports)
            if a.capture_frame:env['DOME_BROWSER_FRAME_CAPTURE']=str(folder/'frame.png')
            with(folder/'browser.log').open('w')as log:
                url=f'http://{a.host}:{primary.server_port}{a.path}'
                if a.local_top_level:url=local_page.as_uri()
                if a.reference_browser:
                    from reference_browser import ReferenceBrowser
                    reference=ReferenceBrowser(a.reference_browser.resolve(),directory,url,env,log,capture_exceptions=a.capture_exceptions);app=reference.process
                    if a.capture_frame:reference.command('Emulation.setDeviceMetricsOverride',{'width':1280,'height':712,'deviceScaleFactor':1,'mobile':False})
                else:app=subprocess.Popen([str(a.binary.resolve()),'--software','--profile',directory+'/profile',url],env=env,stdout=log,stderr=log,start_new_session=True)
            deadline=time.monotonic()+a.timeout
            while len(reports)==before and app.poll()is None and time.monotonic()<deadline:time.sleep(.05)
            if len(reports)==before:
                (out/'failure.json').write_text(json.dumps({'error':'No fixture result','stage':stage,'exit_code':app.poll(),'requests':requests,'adblock_disabled':a.disable_adblock},indent=2)+'\n')
            assert len(reports)>before,('No fixture result',stage,app.poll())
            record=reports[-1];started=time.monotonic()
            if a.capture_frame:
                time.sleep(.5)
                if reference:
                    shot=reference.command('Page.captureScreenshot',{'format':'png'})
                    (folder/'frame.png').write_bytes(base64.b64decode(shot['data']))
            if reference:
                if a.capture_exceptions:
                    reference.command('Runtime.evaluate',{'expression':'0'})
                    record['reference_exception_events']=reference.events.copy()
                closed=reference.close()
            else:
                for window in connection.screen().root.query_tree().children:
                    try:
                        if window.get_wm_name()and window.get_attributes().map_state==X.IsViewable:
                            window.send_event(protocol.event.ClientMessage(window=window,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0)
                    except Exception:pass
                connection.sync();closed=False
                try:app.wait(timeout=10);closed=app.returncode==0
                except subprocess.TimeoutExpired:pass
            record.update(clean_close=closed,exit_code=app.poll(),shutdown_seconds=round(time.monotonic()-started,3));runs.append(record)
            (out/'report.json').write_text(json.dumps({'runs':runs,'requests':requests,'fixture_sha256':hashlib.sha256(page).hexdigest(),
                'binary_sha256':hashlib.sha256((a.reference_browser or a.binary).read_bytes()).hexdigest(),'engine':'Chromium'if reference else'Servo','adblock_disabled':a.disable_adblock},indent=2)+'\n')
            for check in record['checks']:print(('PASS 'if check['passed']else'FAIL ')+stage+': '+check['name']+' '+str(check.get('error')or''),flush=True)
            assert closed,'Browser did not close cleanly'
            if not all(check['passed']for check in record['checks']):break
        assert len(runs)==len(a.stages) and all(all(check['passed']for check in run['checks'])for run in runs),runs
finally:
    if app is not None and app.poll()is None:os.killpg(app.pid,signal.SIGKILL);app.wait(timeout=5)
    if connection is not None:connection.close()
    if xvfb is not None:xvfb.terminate();xvfb.wait(timeout=5)
    for server in [primary,foreign]:server.shutdown();server.server_close()
