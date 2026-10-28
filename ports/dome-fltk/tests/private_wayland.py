#!/usr/bin/env python3
"""Private nested DomeD surfaces for protocol UI fixtures; never uses the desktop."""
import json,os,re,socket,subprocess,time,urllib.request
from pathlib import Path
from types import SimpleNamespace
from Xlib import X

class PrivateWayland:
    def __init__(self,connection,screen,runtime,out,*,hardware=False):
        self.connection=connection;self.out=out
        self.http=urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
        self.base=f'http://127.0.0.1:{port}'
        env={key:value for key,value in os.environ.items() if not key.startswith('DOME_')}
        env.update(HOME=str(runtime),DISPLAY=screen,XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME=str(runtime/'compositor-config'),XDG_DATA_HOME=str(runtime/'compositor-data'),XDG_STATE_HOME=str(runtime/'compositor-state'),XDG_CACHE_HOME=str(runtime/'compositor-cache'),LIBGL_ALWAYS_SOFTWARE='1',DOME_LOGIN_SESSION='0',DOME_SESSION_RESTORE='0',DOME_START_XR='0',DOME_RENDERER='gles',DOME_PREVIEW_PORT=str(port),DOME_APPEARANCE_SERVICE='0',RUST_LOG='warn')
        if hardware:
            # Offscreen Vulkan composition advertises a real render device to
            # EGL clients even when Xvfb presents the private host window.
            env.pop('LIBGL_ALWAYS_SOFTWARE',None)
            env['DOME_RENDERER']='vulkan'
        for name in ['TRAY','KDECONNECT','KWALLET','POLKIT','IME','XWAYLAND']:env['DOME_START_'+name]='0'
        for name in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(name,None)
        self.log=(out/'compositor.log').open('w')
        source=Path(__file__).resolve().parents[3]
        self.compositor=Path(os.environ.get('DOME_COMPOSITOR',source.parent/'DomeD/target/release/dome-compositor'))
        self.child=subprocess.Popen([str(self.compositor),'--backend','nested'],env=env,stdout=self.log,stderr=self.log)
        try:
            self.wait(lambda:self.scene()['source']=='wayland')
            self.socket=self.wait(lambda:re.search(r'Dome Wayland socket: (\S+)',(out/'compositor.log').read_text()))[1]
            self.host=self.wait(lambda:next((w for w in connection.screen().root.query_tree().children if str(w.get_wm_name() or '').startswith('Dome')),None))
        except BaseException:self.close();raise
    def api(self,path,data=None):
        with self.http.open(urllib.request.Request(self.base+path,data=None if data is None else json.dumps(data).encode(),headers={'Content-Type':'application/json'}),timeout=2) as response:return json.load(response)
    def scene(self):return self.api('/api/state')['scene']
    def wait(self,fn):
        deadline=time.monotonic()+20
        while time.monotonic()<deadline:
            if self.child.poll() is not None:raise AssertionError('Private compositor exited')
            try:
                value=fn()
                if value:return value
            except (OSError,ValueError,KeyError):pass
            time.sleep(.05)
        raise AssertionError('Private compositor timed out')
    def windows(self):return [Surface(self,w['id']) for w in self.scene()['windows'] if w['desktop_visible'] and not w['minimized']]
    def close(self):
        if self.child.poll() is None:
            self.child.terminate()
            try:self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:self.child.kill();self.child.wait()
        self.log.close()

class Surface:
    def __init__(self,owner,identity):self.owner=owner;self.id=identity
    def value(self):return next(w for w in self.owner.scene()['windows'] if w['id']==self.id)
    def get_wm_name(self):return self.value()['title']
    def get_geometry(self):
        scene=self.owner.scene();host=self.owner.host.get_geometry()
        # DomeD positions transient toplevels relative to their parent, then
        # clamps them to the work area. Their saved desktop position is not
        # their displayed position (including for native file choosers).
        work={'x':0,'y':36,'w':host.width,'h':max(1,host.height-96)}
        def rectangle(identity,seen=()):
            assert identity not in seen,'Cyclic private fixture window parents'
            window=next(w for w in scene['windows'] if w['id']==identity)
            popup=next((p for p in scene['popups'] if p['id']==identity),None)
            if popup is None:return work if window['maximized'] else window['desktop']
            assert popup.get('context_at') is None and popup.get('viewport') is None,'Expected a native transient toplevel'
            parent=rectangle(popup['parent'],(*seen,identity));anchor=popup['anchor']
            return {'x':min(max(parent['x']+anchor['x'],work['x']),max(work['x'],work['x']+work['w']-anchor['w'])),
                    'y':min(max(parent['y']+anchor['y'],work['y']),max(work['y'],work['y']+work['h']-anchor['h'])),
                    'w':anchor['w'],'h':anchor['h']}
        r=rectangle(self.id)
        return SimpleNamespace(x=int(r['x']),y=int(r['y']),width=int(r['w']),height=int(r['h']))
    def set_input_focus(self,*_):
        self.owner.api('/api/action',{'op':'focus','id':self.id})
        self.owner.host.set_input_focus(X.RevertToParent,X.CurrentTime);self.owner.connection.sync()
        # The API updates scene state before the compositor applies Wayland
        # keyboard focus on its next frame. Do not type into the previous window.
        time.sleep(.15)
    def get_image(self,x,y,w,h,format,mask):
        r=self.get_geometry();return self.owner.host.get_image(r.x+x,r.y+y,w,h,format,mask)
    def send_event(self,*_,**__):self.owner.api('/api/action',{'op':'close','id':self.id})
