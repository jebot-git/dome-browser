#!/usr/bin/env python3
"""Stress native media removal and document teardown in a private DomeD session.

Uses loopback WAV/MP4 fixtures, an isolated browser profile/session bus, Xvfb,
and a nested Wayland compositor. This tests lifetimes, not media feature scope.
"""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import sys
import tempfile
import threading
import time
import wave

from Xlib import X, display
from Xlib.ext import xtest

from private_wayland import PrivateWayland

signal.pthread_sigmask(signal.SIG_SETMASK, [])
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
if os.environ.get('DOME_MEDIA_TEARDOWN_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
              {**os.environ, 'DOME_MEDIA_TEARDOWN_BUS': '1'})
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--debugger', type=Path, help='Capture a local fixture crash stack with gdb')
args = parser.parse_args()
binary = args.binary.resolve()
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=False, mode=0o700)
reports, requests = [], []
app = compositor = connection = xvfb = server = None
result = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
          'private_profile': True, 'private_session_bus': True, 'native_wayland': True}


def wait(callback, timeout=60):
    deadline = time.monotonic()+timeout
    while time.monotonic() < deadline:
        value = callback()
        if value:
            return value
        if app is not None and app.poll() is not None:
            raise AssertionError(f'Browser exited early: {app.returncode}')
        time.sleep(.05)
    raise AssertionError('Media teardown fixture timed out')


try:
    with tempfile.TemporaryDirectory(prefix='dome-media-teardown-') as directory:
        root = Path(directory)
        root.chmod(0o700)
        wav = root/'tone.wav'
        with wave.open(str(wav), 'wb') as stream:
            stream.setnchannels(1)
            stream.setsampwidth(2)
            stream.setframerate(8000)
            stream.writeframes(bytes(16000))
        video = root/'clip.mp4'
        subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi',
                        '-i', 'color=c=blue:s=160x90:r=10', '-t', '1', '-an',
                        '-c:v', 'libx264', '-profile:v', 'baseline', '-pix_fmt', 'yuv420p',
                        '-movflags', '+faststart', str(video)], check=True)

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *unused):
                pass

            def do_POST(self):
                reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                self.send_response(200)
                self.end_headers()

            def do_GET(self):
                requests.append(self.path)
                if self.path in ['/tone.wav', '/clip.mp4']:
                    data = (wav if self.path == '/tone.wav' else video).read_bytes()
                    kind = 'audio/wav' if self.path == '/tone.wav' else 'video/mp4'
                elif self.path == '/missing':
                    self.send_error(404)
                    return
                else:
                    kind = 'text/html'
                    data = b'''<!doctype html><meta charset=utf-8>
<title>Media teardown fixture</title><body style="margin:0;background:#123;color:white">
<button style="width:400px;height:100px" id=start>Start media lifetime fixture</button>
<script>
const page=+(new URL(location).searchParams.get('page')||0);
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
let metadata=0,errors=0;
async function run(){
  start.remove();
  for(let cycle=0;cycle<10;cycle++){
    const nodes=[];
    const ready=[];
    for(const path of ['/clip.mp4','/clip.mp4','/tone.wav','/missing']){
      const m=document.createElement(path.endsWith('.wav')?'audio':'video');
      m.muted=true;m.preload='metadata';
      ready.push(new Promise(resolve=>{
        m.onloadedmetadata=()=>{metadata++;resolve()};
        m.onerror=()=>{errors++;resolve()};
      }));
      m.src=path;document.body.append(m);nodes.push(m);
      m.play().catch(()=>{});
    }
    const frame=document.createElement('iframe');
    frame.srcdoc='<audio preload="metadata" src="/tone.wav"></audio>';
    document.body.append(frame);
    await Promise.race([Promise.all(ready),delay(500)]);
    for(const m of nodes){m.pause();m.removeAttribute('src');m.load();m.remove()}
    frame.remove();
    await fetch('/report',{method:'POST',body:JSON.stringify({page,cycle,metadata,errors})});
    await delay(30);
  }
  if(page<5)location.replace('/?page='+(page+1));
  else {document.title='Media teardown done';document.body.textContent='Media lifetime fixture passed'}
}
start.onclick=run;
if(page>0)run();
</script>'''
                self.send_response(200)
                self.send_header('Content-Type', kind)
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                try:
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        rd, wr = os.pipe()
        xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(wr), '-screen', '0',
                                 '1600x1000x24', '-nolisten', 'tcp'], pass_fds=(wr,),
                                stderr=subprocess.DEVNULL)
        os.close(wr)
        with os.fdopen(rd) as stream:
            screen = ':'+stream.readline().strip()
        connection = display.Display(screen)
        compositor = PrivateWayland(connection, screen, root, out)
        env = {**os.environ, 'HOME': directory, 'DISPLAY': screen,
               'WAYLAND_DISPLAY': compositor.socket, 'FLTK_BACKEND': 'wayland',
               'LIBGL_ALWAYS_SOFTWARE': '1', 'XDG_RUNTIME_DIR': directory,
               'XDG_CONFIG_HOME': directory+'/config', 'XDG_DATA_HOME': directory+'/data',
               'XDG_STATE_HOME': directory+'/state', 'XDG_CACHE_HOME': directory+'/cache'}
        env.pop('WAYLAND_SOCKET', None)
        with (out/'browser.log').open('w') as log:
            command = [str(binary), '--no-restore', '--profile', directory+'/profile',
                       f'http://127.0.0.1:{server.server_port}/']
            if args.debugger:
                command = [str(args.debugger.resolve()), '-q', '-batch',
                           '-ex', 'set debuginfod enabled off',
                           '-ex', 'handle SIGPIPE nostop noprint pass',
                           '-ex', 'run', '-ex', 'bt 20', '--args', *command]
            app = subprocess.Popen(command, env=env, stdout=log, stderr=log,
                                    start_new_session=True)
            window = wait(lambda: next((w for w in compositor.windows()
                                        if 'Media teardown fixture' in w.get_wm_name()), None))
            window.set_input_focus()
            box = window.get_geometry()
            # FLTK chrome occupies 120 pixels above the document.
            xtest.fake_input(connection, X.MotionNotify, x=box.x+100, y=box.y+160)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            connection.sync()
            wait(lambda: len(reports) == 60)
            wait(lambda: 'Media teardown done' in window.get_wm_name())
            window.send_event()
            app.wait(timeout=20)
        result.update(exit_code=app.returncode, cycles=len(reports),
                      document_navigations=len({r['page'] for r in reports})-1,
                      loaded_metadata=sum(max(r['metadata'] for r in reports if r['page'] == page)
                                          for page in range(6)),
                      observed_errors=sum(max(r['errors'] for r in reports if r['page'] == page)
                                          for page in range(6)),
                      served_media_requests=sum(r in ['/tone.wav', '/clip.mp4'] for r in requests))
        log = (out/'browser.log').read_text()
        result['fatal_diagnostics'] = [line for line in log.splitlines()
                                       if any(s in line for s in ['panicked at', 'GLib-GObject-CRITICAL',
                                                                   'GStreamer-CRITICAL', 'Segmentation fault'])]
        result['passed'] = (app.returncode == 0 and len(reports) == 60 and
                            result['loaded_metadata'] > 0 and result['observed_errors'] > 0 and
                            not result['fatal_diagnostics'])
except BaseException as error:
    result['harness_error'] = str(error)
    raise
finally:
    if app is not None and app.poll() is None:
        os.killpg(app.pid, signal.SIGTERM)
        try:
            app.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(app.pid, signal.SIGKILL)
            app.wait()
    if app is not None:
        result.setdefault('exit_code', app.returncode)
    result.setdefault('cycles', len(reports))
    result.setdefault('passed', False)
    if compositor is not None:
        compositor.close()
    if connection is not None:
        connection.close()
    if xvfb is not None:
        xvfb.terminate()
        xvfb.wait(timeout=5)
    if server is not None:
        server.shutdown()
        server.server_close()
    (out/'events.json').write_text(json.dumps(reports, indent=2)+'\n')
    (out/'report.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result), flush=True)
assert result.get('passed'), result
