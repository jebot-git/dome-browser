#!/usr/bin/env python3
"""Keep shared workers alive, then require normal native browser closure."""
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
from urllib.parse import urlsplit
from Xlib import X, display, protocol

signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_WORKER_SHUTDOWN_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],
              {**os.environ,'DOME_WORKER_SHUTDOWN_TEST_BUS':'1'})
source=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,default=source/'target/release/dome-browser')
p.add_argument('--output',type=Path,required=True)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True)
worker='''let connections=0;onconnect=e=>{const port=e.ports[0];port.start();port.postMessage(++connections)};
if(BUSY)setTimeout(()=>{while(true){}},1000);'''
results=[]
for case in ['http-idle','blob-idle','blob-busy']:
    folder=out/case;folder.mkdir(exist_ok=True);messages=[]
    page=('''<!doctype html><title>Shared worker shutdown</title><h1>Worker lifecycle</h1><script>
const source=WORKER_SOURCE;const ids=[];const workers=[];let sent=false;
function report(value){fetch('/result',{method:'POST',body:JSON.stringify(value)})}
const url=USE_BLOB?URL.createObjectURL(new Blob([source],{type:'text/javascript'})):'/worker.js';
for(let i=0;i<2;i++){const w=new SharedWorker(url,'shutdown-regression');workers.push(w);
w.onerror=e=>report({error:String(e.message)});w.port.onmessage=e=>{
ids.push(e.data);if(ids.length===2&&!sent){sent=true;
report({connections:ids.sort(),busy:BUSY})}};w.port.start()}
</script>'''.replace('WORKER_SOURCE',json.dumps(worker)).replace('USE_BLOB',str(case.startswith('blob')).lower()).replace('BUSY',str(case.endswith('busy')).lower())).encode()
    (folder/'fixture.html').write_bytes(page)
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*_):pass
        def do_GET(self):
            script=urlsplit(self.path).path=='/worker.js'
            data=worker.replace('BUSY',str(case.endswith('busy')).lower()).encode()if script else page
            self.send_response(200);self.send_header('Content-Type','text/javascript'if script else'text/html; charset=utf-8')
            self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        def do_POST(self):
            messages.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204);self.end_headers()
    app=xvfb=connection=None;record={'case':case}
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dome-worker-shutdown-')as directory:
            Path(directory).chmod(0o700);rd,wr=os.pipe()
            xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1280x900x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
            with os.fdopen(rd)as stream:screen=':'+stream.readline().strip()
            connection=display.Display(screen)
            env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1',
                 'XDG_RUNTIME_DIR':directory,'XDG_CONFIG_HOME':directory+'/config',
                 'XDG_DATA_HOME':directory+'/data','XDG_CACHE_HOME':directory+'/cache'}
            for key in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(key,None)
            with(folder/'browser.log').open('w')as log:
                app=subprocess.Popen([str(a.binary.resolve()),'--software','--profile',directory+'/profile',f'http://127.0.0.1:{server.server_port}/'],env=env,stdout=log,stderr=log,start_new_session=True)
                deadline=time.monotonic()+30
                while not messages and app.poll()is None and time.monotonic()<deadline:time.sleep(.05)
                assert messages and messages[-1]=={'connections':[1,2],'busy':case.endswith('busy')},messages
                entered=False
                if case.endswith('busy'):
                    # Observe actual execution in the worker before closing.
                    # A postMessage queued immediately before an infinite task
                    # need not be delivered until that task returns.
                    def ticks():
                        total=0
                        for task in (Path('/proc')/str(app.pid)/'task').iterdir():
                            try:
                                if (task/'comm').read_text().startswith('SWW:'):
                                    fields=(task/'stat').read_text().rsplit(')',1)[1].split()
                                    total+=int(fields[11])+int(fields[12])
                            except OSError:pass
                        return total
                    while app.poll()is None and time.monotonic()<deadline:
                        previous=ticks();time.sleep(.1)
                        if ticks()-previous>=5:entered=True;break
                    assert entered,'SharedWorker did not enter its executing task'
                record['worker_report']=messages[-1];closed=0
                record['worker_entered_busy_task']=entered
                for window in connection.screen().root.query_tree().children:
                    try:
                        if window.get_wm_name()and window.get_attributes().map_state==X.IsViewable:
                            window.send_event(protocol.event.ClientMessage(window=window,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);closed+=1
                    except Exception:pass
                assert closed,'No mapped browser window to close'
                connection.sync();started=time.monotonic()
                try:app.wait(timeout=10)
                except subprocess.TimeoutExpired:record['shutdown_timeout']=True
                record.update(exit_code=app.poll(),shutdown_seconds=round(time.monotonic()-started,3))
    except Exception as error:record['harness_error']=str(error)
    finally:
        if app is not None and app.poll()is None:
            os.killpg(app.pid,signal.SIGTERM)
            try:app.wait(timeout=5)
            except subprocess.TimeoutExpired:os.killpg(app.pid,signal.SIGKILL);app.wait()
        if connection is not None:connection.close()
        if xvfb is not None:xvfb.terminate();xvfb.wait(timeout=5)
        server.shutdown();server.server_close()
        record['fixture_sha256']=hashlib.sha256(page).hexdigest();results.append(record)
        (out/'report.json').write_text(json.dumps({'runs':results,'binary_sha256':hashlib.sha256(a.binary.read_bytes()).hexdigest()},indent=2)+'\n')
        print(json.dumps(record),flush=True)
assert all(r.get('exit_code')==0 and not r.get('shutdown_timeout')and not r.get('harness_error')for r in results),results
