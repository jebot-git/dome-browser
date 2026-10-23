#!/usr/bin/env python3
"""Exercise Discord scheduler fallback and persistent IndexedDB in real Servo."""
import contextlib
import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading

signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_COMPATIBILITY_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
              {**os.environ, 'DOME_COMPATIBILITY_TEST_BUS': '1'})

port = Path(__file__).resolve().parents[1]
source = port.parents[1]
artifacts = source.parent / 'artifacts/browser-compatibility-tests'
artifacts.mkdir(parents=True, exist_ok=True)
results = []
page = '''<!doctype html><title>Compatibility fixture</title><h1>Compatibility fixture</h1>
<script>
(async () => {
 const checks = [];
 function check(ok, name) { if (!ok) throw Error(name); checks.push(name); }
 try {
  check(typeof requestIdleCallback === 'undefined', 'fallback is absent on unrelated origins');
  const install = new Function('location', await (await fetch('/shim.js')).text());
  install({protocol:'http:', hostname:'discord.com'});
  install({protocol:'https:', hostname:'discord.com.example.org'});
  check(typeof requestIdleCallback === 'undefined', 'fallback requires a Discord HTTPS origin');
  const native = () => 123;
  globalThis.requestIdleCallback = native;
  install({protocol:'https:', hostname:'discord.com'});
  check(requestIdleCallback === native, 'existing native scheduler is preserved');
  delete globalThis.requestIdleCallback;
  install({protocol:'https:', hostname:'canary.discord.com'});
  let cancelled = true;
  cancelIdleCallback(requestIdleCallback(() => cancelled = false));
  const deadline = await new Promise(resolve => requestIdleCallback(resolve, {timeout:0}));
  await new Promise(resolve => setTimeout(resolve, 20));
  check(cancelled, 'cancelled callbacks do not run');
  check(deadline.didTimeout && deadline.timeRemaining() >= 0 && deadline.timeRemaining() <= 8,
        'fallback supplies a bounded deadline and timeout flag');
  let rejected = false;
  try { requestIdleCallback(null); } catch(e) { rejected = e instanceof TypeError; }
  check(rejected, 'invalid callback is rejected');
  const cache = await caches.open('asset-fixture');
  check(cache instanceof Cache && await caches.has('asset-fixture'), 'Discord asset loader can open a cache');
  await cache.put('/fixture.wasm', new Response('fixture'));
  check(await cache.match('/fixture.wasm') === undefined, 'network-only cache never returns a fabricated response');
  check(await caches.delete('asset-fixture') && !(await caches.has('asset-fixture')), 'cache names can be removed');
  const db = await new Promise((resolve, reject) => {
   const request = indexedDB.open('dome-compatibility-fixture', 1);
   request.onupgradeneeded = () => request.result.createObjectStore('values');
   request.onsuccess = () => resolve(request.result);
   request.onerror = () => reject(request.error);
  });
  const previous = await new Promise((resolve, reject) => {
   const request = db.transaction('values').objectStore('values').get('visits');
   request.onsuccess = () => resolve(request.result || 0);
   request.onerror = () => reject(request.error);
  });
  await new Promise((resolve, reject) => {
   const tx = db.transaction('values', 'readwrite');
   tx.objectStore('values').put(previous + 1, 'visits');
   tx.oncomplete = resolve; tx.onerror = () => reject(tx.error);
  });
  db.close();
  checks.push('IndexedDB read/write transaction completes');
  await fetch('/result', {method:'POST',body:JSON.stringify({checks, previous})});
 } catch(e) {
  await fetch('/result', {method:'POST',body:JSON.stringify({checks,error:String(e)})});
 }
})();
</script>'''

class Server(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        data = (port/'src/discord-compatibility.js').read_bytes() if self.path == '/shim.js' else page.encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/javascript' if self.path == '/shim.js' else 'text/html')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)
    def do_POST(self):
        results.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
        self.send_response(204)
        self.end_headers()
    def log_message(self, *args):
        pass

with tempfile.TemporaryDirectory(prefix='browser-compatibility-') as directory:
    root = Path(directory)
    root.chmod(0o700)
    rd, wr = os.pipe()
    xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(wr), '-screen', '0', '1280x900x24',
                             '-nolisten', 'tcp'], pass_fds=(wr,), stderr=subprocess.DEVNULL)
    os.close(wr)
    with os.fdopen(rd) as stream:
        screen = ':' + stream.readline().strip()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Server)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    env = {**os.environ, 'DISPLAY':screen, 'FLTK_BACKEND':'x11', 'LIBGL_ALWAYS_SOFTWARE':'1',
           'XDG_RUNTIME_DIR':directory, 'XDG_DATA_HOME':str(root/'data'),
           'XDG_CONFIG_HOME':str(root/'config'), 'XDG_CACHE_HOME':str(root/'cache')}
    for key in ('WAYLAND_DISPLAY', 'WAYLAND_SOCKET'):
        env.pop(key, None)
    binary = os.environ.get('DOME_BROWSER_BINARY', str(source/'target/release/dome-browser'))
    try:
        with (artifacts/'browser.log').open('w') as log:
            for run in range(2):
                subprocess.run([binary, '--profile', str(root/'profile'), '--smoke-test',
                                str(artifacts/f'run-{run}.ppm'), f'http://127.0.0.1:{server.server_port}/'],
                               env=env, stdout=log, stderr=log, check=True, timeout=90)
                assert len(results) == run + 1, results
                assert 'error' not in results[-1], results[-1]
                assert results[-1]['previous'] == run, results[-1]
        for name in results[0]['checks']:
            print('PASS', name, flush=True)
        print('PASS IndexedDB persists across clean browser restarts', flush=True)
    finally:
        (artifacts/'results.json').write_text(json.dumps(results, indent=2)+'\n')
        server.shutdown()
        server.server_close()
        xvfb.terminate()
        with contextlib.suppress(subprocess.TimeoutExpired):
            xvfb.wait(timeout=5)
