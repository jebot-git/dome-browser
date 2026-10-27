#!/usr/bin/env python3
"""Check native WASM stream consumption and closure with a pending stream."""
import argparse
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
from Xlib import X, display, protocol

signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_WASM_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
               {**os.environ, 'DOME_WASM_TEST_BUS':'1'})
source=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,default=source/'target/release/dome-browser')
p.add_argument('--output',type=Path,required=True)
p.add_argument('--reference-browser',type=Path)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True)
page=Path(__file__).with_suffix('.html').read_bytes();(out/'fixture.html').write_bytes(page);reports=[];pending=threading.Event()
wasm=bytes([0,97,115,109,1,0,0,0])
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_GET(self):
        if self.path=='/stream.wasm':
            self.send_response(200);self.send_header('Content-Type','application/wasm');self.send_header('Content-Length',str(len(wasm)));self.end_headers()
            try:
                for start in range(0,len(wasm),4):
                    time.sleep(.1);self.wfile.write(wasm[start:start+4]);self.wfile.flush()
            except (BrokenPipeError,ConnectionResetError):pass
        else:
            self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8');self.end_headers();self.wfile.write(page)
    def do_POST(self):
        data=self.rfile.read(int(self.headers['Content-Length']))
        if self.path=='/report':reports.append(json.loads(data))
        elif self.path=='/pending':pending.set()
        self.send_response(204);self.end_headers()
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
app=xvfb=connection=reference=None
try:
    with tempfile.TemporaryDirectory(prefix='dome-wasm-streams-')as directory:
        rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1280x900x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
        with os.fdopen(rd)as stream:screen=':'+stream.readline().strip()
        env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','XDG_RUNTIME_DIR':directory,
             'XDG_CONFIG_HOME':directory+'/config','XDG_CACHE_HOME':directory+'/cache','XDG_DATA_HOME':directory+'/data'}
        for key in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(key,None)
        with(out/'browser.log').open('w')as log:
            url=f'http://127.0.0.1:{server.server_port}/'
            if a.reference_browser:
                from reference_browser import ReferenceBrowser
                reference=ReferenceBrowser(a.reference_browser.resolve(),directory,url,env,log)
                app=reference.process
            else:
                app=subprocess.Popen([str(a.binary.resolve()),'--software','--profile',directory+'/profile',url],env=env,stdout=log,stderr=log,start_new_session=True)
        deadline=time.monotonic()+40
        while not pending.is_set()and app.poll()is None and time.monotonic()<deadline:time.sleep(.05)
        assert reports and pending.is_set(),('No completed report/pending stream',reports,app.poll())
        worker_threads=[]
        if not reference:
            deadline=time.monotonic()+2
            while time.monotonic()<deadline:
                worker_threads=[]
                for task in (Path('/proc')/str(app.pid)/'task').iterdir():
                    try:
                        name=(task/'comm').read_text().strip()
                        if name.startswith('WW:'):worker_threads.append({'tid':int(task.name),'name':name})
                    except OSError:pass
                if not worker_threads:break
                time.sleep(.05)
        started=time.monotonic()
        if reference:
            closed=reference.close()
        else:
            connection=display.Display(screen)
            for window in connection.screen().root.query_tree().children:
                try:
                    if window.get_wm_name()and window.get_attributes().map_state==X.IsViewable:
                        window.send_event(protocol.event.ClientMessage(window=window,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0)
                except Exception:pass
            connection.sync();closed=False;started=time.monotonic()
            try:app.wait(timeout=10);closed=app.returncode==0
            except subprocess.TimeoutExpired:
                with(out/'shutdown-stack.log').open('w')as stack:
                    try:subprocess.run(['eu-stack','-p',str(app.pid),'-n','30'],env={**os.environ,'DEBUGINFOD_URLS':''},stdout=stack,stderr=stack,timeout=20)
                    except (OSError,subprocess.TimeoutExpired)as error:stack.write(str(error))
        record={'checks':reports[-1]['checks'],'pending_stream_started':True,'remaining_terminated_workers':worker_threads,'clean_close':closed,'exit_code':app.poll(),'shutdown_seconds':round(time.monotonic()-started,3),
                'fixture_sha256':hashlib.sha256(page).hexdigest(),'binary_sha256':hashlib.sha256((a.reference_browser or a.binary).read_bytes()).hexdigest(),'engine':'Chromium'if reference else'Servo'}
        (out/'report.json').write_text(json.dumps(record,indent=2)+'\n')
        for check in record['checks']:print(('PASS 'if check['passed']else'FAIL ')+check['name']+(' '+check.get('error','')),flush=True)
        print('Clean close:',closed,flush=True)
        assert closed and not worker_threads and all(check['passed']for check in record['checks']),record
finally:
    if app is not None and app.poll()is None:os.killpg(app.pid,signal.SIGKILL);app.wait(timeout=5)
    if connection is not None:connection.close()
    if xvfb is not None:xvfb.terminate();xvfb.wait(timeout=5)
    server.shutdown();server.server_close()
