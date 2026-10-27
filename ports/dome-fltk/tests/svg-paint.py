#!/usr/bin/env python3
"""Check actual Servo pixels for stylesheet-defined SVG paint servers/stops."""
import argparse
import asyncio
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
import urllib.request
from PIL import Image

signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_SVG_PAINT_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],
              {**os.environ,'DOME_SVG_PAINT_TEST_BUS':'1'})
port=Path(__file__).resolve().parents[1]
source=port.parents[1]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,default=source/'target/release/dome-browser')
p.add_argument('--output',type=Path,required=True)
p.add_argument('--reference-browser',type=Path)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True)
page=(port/'tests/svg-paint.html').read_bytes();reports=[]

async def reference(url,folder,binary,env,profile):
    import websockets
    endpoint=profile/'DevToolsActivePort'
    browser=None
    try:
        with(folder/'browser.log').open('w')as log:
            browser=subprocess.Popen([str(binary),'--headless=new','--no-first-run',
                '--no-default-browser-check','--disable-background-networking','--disable-gpu',
                '--remote-debugging-port=0','--remote-allow-origins=*',
                '--user-data-dir='+str(profile),'about:blank'],env=env,
                stdout=log,stderr=log,start_new_session=True)
            deadline=time.monotonic()+25
            while not endpoint.exists():
                assert browser.poll()is None and time.monotonic()<deadline,'Reference browser did not start'
                await asyncio.sleep(.1)
            port=endpoint.read_text().splitlines()[0]
            targets=json.load(urllib.request.urlopen('http://127.0.0.1:'+port+'/json/list'))
            target=next(t for t in targets if t['type']=='page');serial=0
            async with websockets.connect(target['webSocketDebuggerUrl'],proxy=None)as ws:
                async def command(method,params=None):
                    nonlocal serial
                    serial+=1;await ws.send(json.dumps({'id':serial,'method':method,'params':params or{}}))
                    while True:
                        message=json.loads(await asyncio.wait_for(ws.recv(),10))
                        if message.get('id')==serial:
                            assert 'error'not in message,message
                            return message.get('result',{})
                await command('Page.enable');await command('Runtime.enable')
                await command('Emulation.setDeviceMetricsOverride',{'width':1280,'height':800,'deviceScaleFactor':1,'mobile':False})
                await command('Page.navigate',{'url':url})
                while time.monotonic()<deadline:
                    state=await command('Runtime.evaluate',{'expression':'document.readyState==="complete" && !!document.getElementById("fallback")','returnByValue':True})
                    if state.get('result',{}).get('value'):break
                    await asyncio.sleep(.1)
                else:raise TimeoutError('Reference fixture did not load')
                await asyncio.sleep(1)
                shot=await command('Page.captureScreenshot',{'format':'png','captureBeyondViewport':False})
                (folder/'frame.png').write_bytes(base64.b64decode(shot['data']))
    finally:
        if browser is not None and browser.poll()is None:
            os.killpg(browser.pid,signal.SIGTERM);browser.wait(timeout=5)
class Server(http.server.BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_GET(self):
        self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8')
        self.send_header('Content-Length',str(len(page)));self.end_headers();self.wfile.write(page)
    def do_POST(self):
        reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
        self.send_response(204);self.end_headers()
with tempfile.TemporaryDirectory(prefix='dome-svg-paint-')as directory:
    runtime=Path(directory);runtime.chmod(0o700)
    rd,wr=os.pipe()
    xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1280x900x24',
                           '-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL)
    os.close(wr)
    with os.fdopen(rd)as stream:screen=':'+stream.readline().strip()
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1',
         'XDG_RUNTIME_DIR':directory,'XDG_DATA_HOME':directory+'/data',
         'XDG_CONFIG_HOME':directory+'/config','XDG_CACHE_HOME':directory+'/cache',
         'DOME_BROWSER_FRAME_CAPTURE':str(out/'frame.png')}
    for key in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(key,None)
    try:
        url=f'http://127.0.0.1:{server.server_port}/'
        if a.reference_browser:
            env['FONTCONFIG_FILE']=os.environ.get('DE_XR_FONTCONFIG_BASE','/etc/fonts/fonts.conf')
            env['DBUS_SESSION_BUS_ADDRESS']='unix:path='+str(runtime/'no-reference-bus')
            asyncio.run(reference(url,out,a.reference_browser.resolve(),env,runtime/'chrome'))
        else:
            with(out/'browser.log').open('w')as log:
                result=subprocess.run([str(a.binary.resolve()),'--software','--profile',
                                       str(runtime/'profile'),'--smoke-test',str(out/'window.ppm'),url],
                                      env=env,stdout=log,stderr=log,timeout=90)
            assert result.returncode==0,('Browser failed',result.returncode)
        frame=Image.open(out/'frame.png').convert('RGB')
        samples={name:list(frame.getpixel((x,50)))for name,x in
                 [('attribute gradient',50),('stylesheet solid fill',130),
                  ('stylesheet gradient reference',210),('stylesheet stop colors',290),
                  ('stylesheet gradient stroke',350),('missing paint server fallback',450)]}
        checks={name:color==[20,180,90]for name,color in samples.items()}
        record={'checks':checks,'pixels':samples,'dom_reports':reports,
                'fixture_sha256':hashlib.sha256(page).hexdigest(),
                'engine':'Chromium'if a.reference_browser else'Servo',
                'binary_sha256':hashlib.sha256((a.reference_browser or a.binary).read_bytes()).hexdigest()}
        (out/'report.json').write_text(json.dumps(record,indent=2)+'\n')
        for name,passed in checks.items():print(('PASS 'if passed else'FAIL ')+name,flush=True)
        assert reports,'No computed-style report received'
        assert all(checks.values()),samples
    finally:
        server.shutdown();server.server_close();xvfb.terminate();xvfb.wait(timeout=5)
