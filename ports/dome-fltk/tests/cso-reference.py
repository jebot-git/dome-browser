#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Read-only browser interoperability with an independently built Qi server.

The caller supplies a private Qi build and synthetic database. The inetd and
observation adapters bind only loopback and preserve every response byte.
"""
import argparse,hashlib,json,os,selectors,signal,socket,socketserver,subprocess,tempfile,threading,time
from pathlib import Path
signal.pthread_sigmask(signal.SIG_SETMASK,[])
from PIL import Image
from Xlib import X,XK,display,protocol
from Xlib.ext import xtest

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,required=True)
p.add_argument('--qi',type=Path,required=True)
p.add_argument('--output',type=Path,required=True)
p.add_argument('--wayland',action='store_true')
p.add_argument('--cases',nargs='+',choices=['index','lookup','ph','return-fields','no-match','gopher-item2'])
a=p.parse_args();binary=a.binary.resolve();qi=a.qi.resolve();out=a.output.resolve()
assert not out.exists();out.mkdir(parents=True,mode=0o700)
checks=[];records=[];processes=[];errors=[];runs=[];child=wayland=xvfb=connection=None
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def check(ok,name):
 checks.append({'name':name,'passed':bool(ok)});assert ok,name;print('PASS',name,flush=True)
def wait(fn,seconds=20):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  value=fn()
  if value:return value
  time.sleep(.05)
 return None
class Server(socketserver.ThreadingTCPServer):allow_reuse_address=True;daemon_threads=True
class Backend(socketserver.BaseRequestHandler):
 def handle(self):
  with (out/'qi.log').open('a') as log:
   # A connected IPv4 socket gives Qi its normal anonymous network context.
   proc=subprocess.Popen([str(qi),'-q','-l'],stdin=self.request,stdout=self.request,stderr=log)
   processes.append(proc)
   try:proc.wait(timeout=35)
   except subprocess.TimeoutExpired:proc.terminate();proc.wait(timeout=3);errors.append('Qi timed out')
backend=Server(('127.0.0.1',0),Backend)
threading.Thread(target=backend.serve_forever,daemon=True).start()
class Observe(socketserver.BaseRequestHandler):
 def handle(self):
  record={'commands':[],'response_hex':''};records.append(record);pending=b'';response=bytearray()
  try:
   with socket.create_connection(backend.server_address,timeout=5) as peer,selectors.DefaultSelector() as select:
    select.register(self.request,selectors.EVENT_READ,peer);select.register(peer,selectors.EVENT_READ,self.request)
    while select.get_map():
     events=select.select(timeout=25)
     if not events:raise TimeoutError('Reference exchange timed out')
     for key,_ in events:
      source=key.fileobj;target=key.data;data=source.recv(65536)
      if not data:
       select.unregister(source)
       try:target.shutdown(socket.SHUT_WR)
       except OSError:pass
       continue
      if source is self.request:
       pending+=data
       while b'\r\n' in pending:
        line,pending=pending.split(b'\r\n',1)
        command=line.decode('latin1');record['commands'].append(command)
        assert command in ['set charset=iso-8859-1','fields','siteinfo','quit'] or command.startswith('query '),'Unexpected reference command'
      else:
       response.extend(data)
       assert len(response)<1024*1024,'Synthetic reference response exceeds expected size'
      target.sendall(data)
  except (BrokenPipeError,ConnectionResetError):pass
  except Exception as error:errors.append(repr(error))
  finally:record['response_hex']=response.hex()
proxy=Server(('127.0.0.1',0),Observe)
threading.Thread(target=proxy.serve_forever,daemon=True).start()
def browser():
 if wayland:return next((w for w in wayland.windows() if str(w.get_wm_name() or '').endswith(' — Dome Browser')),None)
 for w in connection.screen().root.query_tree().children:
  if w.get_attributes().map_state==X.IsViewable and w.get_geometry().width>=1000 and w.get_geometry().height>=700:return w
def frame(w):
 g=w.get_geometry();raw=w.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
 return Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX')
def close(w):
 if wayland:wayland.api('/api/action',{'op':'close','id':w.id})
 else:
  w.send_event(protocol.event.ClientMessage(window=w,client_type=connection.intern_atom('WM_PROTOCOLS'),data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0])),event_mask=0);connection.sync()
try:
 with tempfile.TemporaryDirectory(prefix='dome-cso-reference-') as directory:
  runtime=Path(directory);runtime.chmod(0o700)
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1600x1100x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL);os.close(wr)
  with os.fdopen(rd) as stream:screen=':'+stream.readline().strip()
  connection=display.Display(screen)
  if a.wayland:
   from private_wayland import PrivateWayland
   wayland=PrivateWayland(connection,screen,runtime,out)
  for name in a.cases or ['index','lookup','ph','return-fields','no-match','gopher-item2']:
   folder=out/name;folder.mkdir();home=runtime/name;home.mkdir();profile=home/'profile'
   env={**os.environ,'HOME':str(home),'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1','XDG_RUNTIME_DIR':directory,'XDG_DATA_HOME':str(home/'data'),'XDG_CONFIG_HOME':str(home/'config'),'XDG_CACHE_HOME':str(home/'cache'),'XDG_STATE_HOME':str(home/'state'),'RUST_LOG':'warn'}
   for k in list(env):
    if k in ['WAYLAND_DISPLAY','WAYLAND_SOCKET'] or k.startswith(('DOME_BROWSER_FRAME_CAPTURE','DOME_BROWSER_LAYOUT_CAPTURE','DOME_BROWSER_MEMORY_CAPTURE')):env.pop(k,None)
   if wayland:env.pop('DISPLAY',None);env.update(FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket)
   port=proxy.server_address[1];scheme='ph' if name=='ph' else 'cso'
   path='/' if name in ['index','gopher-item2'] else '/?q=fixture&field=name'
   if name=='return-fields':path+='&return=name%2Cemail'
   if name=='no-match':path='/?q=absent-fixture&field=alias'
   url=f'{scheme}://127.0.0.1:{port}{path}'
   if name=='gopher-item2':url=f'gopher://127.0.0.1:{port}/2ignored-selector'
   start=len(records)
   with (folder/'browser.log').open('w') as log:
    child=subprocess.Popen(['dbus-run-session','--',str(binary),'--software','--no-restore','--profile',str(profile),url],env=env,stdout=log,stderr=log,start_new_session=True)
    title='Directory server' if name in ['index','gopher-item2'] else 'Directory results for'
    w=wait(lambda:browser() if browser() and title in str(browser().get_wm_name() or '') else None)
    check(w is not None,name+': independent Qi response becomes a native directory page')
    title_observed=str(w.get_wm_name() or '')
    # Let the compositor and browser settle before collecting visual evidence.
    time.sleep(.7);frame(w).save(folder/'frame.png')
    w.set_input_focus(X.RevertToParent,X.CurrentTime)
    if not wayland:w.configure(stack_mode=X.Above)
    for event,symbol in [(X.KeyPress,XK.XK_Control_L),(X.KeyPress,XK.XK_s),(X.KeyRelease,XK.XK_s),(X.KeyRelease,XK.XK_Control_L)]:xtest.fake_input(connection,event,connection.keysym_to_keycode(symbol))
    connection.sync();saved=home/'Downloads/directory.txt'
    check(wait(saved.exists),name+': decoded directory source is available through native Save')
    source=saved.read_text();(folder/'decoded-source.txt').write_text(source)
    if name in ['index','gopher-item2']:check('name' in source and 'Full name.' in source,name+': field schema survives browser decoding')
    elif name=='no-match':check('CSO 501:' in source,name+': no-match status survives browser decoding')
    else:check('Dome Fixture' in source and 'Second Fixture' in source,name+': both result records survive browser decoding')
    close(w);check(child.wait(timeout=10)==0,name+': browser closes cleanly');child=None
   check(wait(lambda:len(records)>start and records[-1]['commands'] and records[-1]['commands'][-1]=='quit'),name+': orderly quit reaches Qi')
   session=records[start];wire=bytes.fromhex(session['response_hex'])
   check(session['commands'][0]=='set charset=iso-8859-1' and b'513:No option recognized.\n' in wire,name+': unsupported charset negotiation falls back')
   check(b'\n' in wire and b'\r\n' not in wire,name+': actual LF-only response bytes are preserved')
   if name in ['index','gopher-item2']:check(session['commands'][1:-1]==['fields','siteinfo'] and b'name:max 256 Indexed Lookup Public Default' in wire,name+': field schema and site information reach the browser')
   elif name=='no-match':check(b'501:' in wire,name+': no matching entry is a readable directory status')
   else:
    check(b'Dome Fixture' in wire and b'Second Fixture' in wire,name+': both independent records are returned')
    expected='query name="fixture"'+(' return name email' if name=='return-fields' else '')
    check(session['commands'][1]==expected,name+': requested field and return selection reach Qi')
   check(not (profile/'gemini-trust.json').exists(),name+': plain directory browsing creates no TLS identity state')
   runs.append({'name':name,'passed':True,'window_title':title_observed})
 check(not errors,'Reference adapter completed without errors')
 check(wait(lambda:all(proc.poll() is not None for proc in processes)) and all(proc.returncode==0 for proc in processes),'Every private Qi process exits normally')
finally:
 if child and child.poll() is None:
  os.killpg(child.pid,signal.SIGTERM);child.wait(timeout=5)
 proxy.shutdown();proxy.server_close();backend.shutdown();backend.server_close()
 for proc in processes:
  if proc.poll() is None:proc.terminate();proc.wait(timeout=5)
 if wayland:wayland.close()
 if connection:connection.close()
 if xvfb:xvfb.terminate();xvfb.wait(timeout=5)
 report={'time':time.time(),'passed':len(runs)==len(a.cases or range(6)) and all(c['passed'] for c in checks),'binary_sha256':sha(binary),'driver_sha256':sha(Path(__file__)),'qi_sha256':sha(qi),'backend':'native-wayland' if a.wayland else 'private-x11','checks':checks,'runs':runs,'requests':records,'errors':errors,'scope':'Read-only interoperability with historical Qi built using documented modern-libc portability changes and a private synthetic database. Loopback inetd adapter and observer preserve protocol bytes; not full protocol conformance.'}
 (out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
