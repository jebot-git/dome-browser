"""Private headless Chromium session for browser fixture comparisons."""
import json
import os
from pathlib import Path
import signal
import subprocess
import time
import urllib.request
from websockets.sync.client import connect
from websockets.exceptions import ConnectionClosed

class ReferenceBrowser:
    def __init__(self, binary, runtime, url, env, log, capture_exceptions=False):
        runtime = Path(runtime)
        profile = runtime/'reference-profile'
        env = {**env, 'FONTCONFIG_FILE':os.environ.get('DE_XR_FONTCONFIG_BASE','/etc/fonts/fonts.conf'),
               'DBUS_SESSION_BUS_ADDRESS':'unix:path='+str(runtime/'no-reference-bus')}
        endpoint=profile/'DevToolsActivePort'
        endpoint.unlink(missing_ok=True)
        self.events=[]
        self.capture_exceptions=capture_exceptions
        self.socket=None
        self.process = subprocess.Popen([str(binary), '--headless=new', '--disable-gpu',
            '--disable-background-networking', '--no-first-run', '--no-default-browser-check',
            '--remote-debugging-port=0', '--remote-allow-origins=*', '--user-data-dir='+str(profile),
            'about:blank'], env=env, stdout=log, stderr=log, start_new_session=True)
        try:
            deadline=time.monotonic()+25
            while not endpoint.exists():
                assert self.process.poll() is None and time.monotonic()<deadline,'Reference browser did not start'
                time.sleep(.1)
            port=endpoint.read_text().splitlines()[0]
            targets=json.load(urllib.request.urlopen('http://127.0.0.1:'+port+'/json/list'))
            target=next(target for target in targets if target['type']=='page')
            self.socket=connect(target['webSocketDebuggerUrl'],proxy=None)
            self.serial=0
            self.command('Page.enable')
            if capture_exceptions:
                self.command('Runtime.enable')
                self.command('Log.enable')
            self.command('Page.navigate',{'url':url})
        except BaseException:
            if self.socket is not None:self.socket.close()
            if self.process.poll() is None:
                os.killpg(self.process.pid,signal.SIGKILL)
                self.process.wait(timeout=5)
            raise

    def command(self, method, params=None):
        self.serial+=1
        self.socket.send(json.dumps({'id':self.serial,'method':method,'params':params or {}}))
        while True:
            message=json.loads(self.socket.recv(timeout=10))
            if self.capture_exceptions and message.get('method') in ['Runtime.exceptionThrown','Log.entryAdded']:
                self.events.append(message)
            if message.get('id')==self.serial:
                assert 'error' not in message,message
                return message.get('result',{})

    def close(self):
        try:self.command('Browser.close')
        except ConnectionClosed:pass
        self.socket.close()
        try:self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:return False
        return self.process.returncode==0
