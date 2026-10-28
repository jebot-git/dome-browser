#!/usr/bin/env python3
"""Exercise live renderer reconstruction without replacing documents."""
import argparse,contextlib,hashlib,http.server,json,os,queue,resource,signal,subprocess,sys,tempfile,threading,time
from pathlib import Path
from Xlib import display
from PIL import Image
from io import BytesIO
import shutil,re
from private_wayland import PrivateWayland
signal.pthread_sigmask(signal.SIG_SETMASK,[])
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_RENDERER_RECOVERY_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_RENDERER_RECOVERY_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--hardware',action='store_true');p.add_argument('--webgl2',action='store_true');a=p.parse_args()
font_path=Path('/usr/share/fonts/google-noto/NotoSansMono-Regular.ttf');font_bytes=font_path.read_bytes()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);page=Path(__file__).with_suffix('.html').read_bytes();checks=[];child=None;failure=None;replies=queue.Queue();token=0;log_path=out/'embedder.log'
def check(ok,name):
 checks.append({'name':name,'passed':bool(ok)});assert ok,name
def stop(proc):
 if proc.poll()is None:
  proc.terminate()
  try:proc.wait(timeout=8)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()
def ask(op,**values):
 global token
 token+=1;wanted=token;child.stdin.write(json.dumps({'op':op,'token':wanted,**values})+'\n');child.stdin.flush();end=time.monotonic()+10
 while time.monotonic()<end:
  row=replies.get(timeout=max(.01,end-time.monotonic()))
  if row.get('token')==wanted:
   assert not row.get('error') or (op=='renderer-recover' and values.get('reject') is True),row;return row
 raise AssertionError('Native test command timed out')
def evaluate(script):return json.loads(ask('eval',script='JSON.stringify('+script+')')['value'])
def snapshot():return evaluate('snapshot()')
def wait(predicate):
 end=time.monotonic()+10
 while time.monotonic()<end:
  value=snapshot()
  if predicate(value):return value
  time.sleep(.03)
 raise AssertionError('Restoration state timed out: '+json.dumps(value))
class Server(http.server.BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def do_GET(self):
  body=page;kind='text/html;charset=utf-8'
  if self.path.startswith('/tile.png'):
   stream=BytesIO();Image.new('RGB',(8,8),(0,40,220)).save(stream,format='PNG');body=stream.getvalue();kind='image/png'
  elif self.path=='/font.ttf':body=font_bytes;kind='font/ttf'
  elif self.path=='/frame':body=b"<style>@font-face{font-family:RecoveryWebfont;src:url('/font.ttf')}body{margin:0;background:rgb(220,100,10);color:rgb(0,200,220);font:20px RecoveryWebfont}</style>Same live iframe<script>window.identity=Math.random();document.fonts.ready.then(()=>window.ready=true)</script>"
  self.send_response(200);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(body)));self.send_header('Cache-Control','no-store');self.end_headers();self.wfile.write(body)
try:
 with contextlib.ExitStack()as stack:
  root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-renderer-recovery-')));root.chmod(0o700)
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1200x900x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd)
  connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,root,out,hardware=a.hardware);stack.callback(wayland.close)
  env={k:v for k,v in os.environ.items()if not k.startswith('DOME_')and k not in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']};env.update(HOME=str(root),DISPLAY=screen,WAYLAND_DISPLAY=wayland.socket,XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_CACHE_HOME=str(root/'cache'),XDG_DATA_HOME=str(root/'data'),XR_INPUT_PROFILE=str(root/'profile'),DOME_GRAPHICS_LOSS_TEST='1',FONTCONFIG_FILE='/etc/fonts/fonts.conf',LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn,webrender::device::gl=info')
  if a.hardware:env.pop('LIBGL_ALWAYS_SOFTWARE',None);env['DOME_RENDERER_RECOVERY_HARDWARE']='1'
  log=stack.enter_context(log_path.open('w'));child=subprocess.Popen([str(a.binary.resolve()),f'http://127.0.0.1:{server.server_port}/?version=' + ('webgl2' if a.webgl2 else 'webgl')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,text=True,bufsize=1,env=env);stack.callback(stop,child)
  def read():
   for line in child.stdout:
    if line.startswith('XR_INPUT '):replies.put(json.loads(line[len('XR_INPUT '):]))
  threading.Thread(target=read,daemon=True).start()
  # Wait for the document without depending on wall-clock load timing.
  end=time.monotonic()+15
  ready=False
  while time.monotonic()<end:
   assert child.poll()is None,'Test embedder exited during startup'
   try:ready=evaluate('typeof snapshot')=='function'
   except AssertionError as error:
    if 'Err(InternalError)'not in str(error):raise
   if ready:break
   time.sleep(.03)
  check(ready,'Target document becomes ready before graphics commands')
  wait(lambda s:s['ready']and s['imageReady']and s['frameIdentity']is not None and s['frameReady'] and s['y']>0)
  ask('renderer-zoom',zoom=1.2);before=wait(lambda s:s['width']<850 and s['y']>0)
  def pixels(name):
   path=Path(ask('renderer-frame')['path']);image=Image.open(path).convert('RGB');shutil.copyfile(path,out/(name+'.png'))
   values=list(image.getdata());return {'blue':sum(b>180 and r<30 and g<80 for r,g,b in values),'green':sum(g>150 and r<30 and b<70 for r,g,b in values),'main_glyphs':sum(r>120 and g<70 and b>120 for r,g,b in values),'iframe_glyphs':sum(r<30 and g>150 and b>150 for r,g,b in values),'iframe_background':sum(r>190 and 70<g<130 and b<40 for r,g,b in values),'gpu':sum(45<r<85 and 185<g<225 and 10<b<50 for r,g,b in values)}
  def visible(colors):return colors['blue']>3000 and colors['green']>2000 and colors['main_glyphs']>150 and colors['iframe_glyphs']>100 and colors['iframe_background']>15000 and colors['gpu']>3000
  visual_samples=[]
  end=time.monotonic()+10
  while time.monotonic()<end:
   baseline=pixels('before')
   if visible(baseline):break
   time.sleep(.05)
  visual_samples.append({'stage':'before',**baseline})
  check(visible(baseline),'Initial fonts, bitmap, CSS and iframe render on the native target')
  for cycle in range(3):
   ask('renderer-loss');check(snapshot()['nonce']==before['nonce'],'Live document remains available while native context is retired')
   failed=ask('renderer-recover',reject=True);check(not failed['recovered'],'Failed replacement preserves the current document')
   evaluate('mutate()');changed=snapshot();check(changed['value']=='changed during native loss'and changed['start']==3 and changed['end']==10,'Form and selection remain editable while graphics are unavailable')
   check(ask('renderer-recover')['recovered'],'A fresh renderer reconstructs the scene')
   after=wait(lambda s:s['restored']==cycle+1 and not s['glLost'])
   check(after['nonce']==before['nonce']and after['origin']==before['origin']and after['frameIdentity']==before['frameIdentity'],'Document, JS global and iframe identities survive reconstruction')
   check(after['value']==changed['value']and after['start']==3 and after['end']==10 and abs(after['y']-changed['y'])<2 and after['width']==changed['width'],'Form, selection, scrolling and page zoom survive reconstruction')
   check(after['oldInvalid']and after['error']==0 and after['pixel'][1]>190,'WebGL producer receives loss and restores a clean resource generation')
   end=time.monotonic()+10
   while time.monotonic()<end:
    colors=pixels('after-'+str(cycle))
    if visible(colors):break
    time.sleep(.05)
   visual_samples.append({'stage':'after-'+str(cycle),**colors})
   check(visible(colors),'Replayed fonts, bitmap, CSS and iframe pixels remain visible')
   if cycle==0:evaluate('resources()');time.sleep(1)
  ask('renderer-loss');ask('quit');check(child.wait(timeout=10)==0,'Shutdown completes with a retired root renderer')
  renderer_names=sorted(set(re.findall(r'Renderer: ([^\r\n]+)',log_path.read_text(errors='replace'))))
  check(bool(renderer_names)and all(any(token in name.lower()for token in ['llvmpipe','softpipe','software rasterizer'])==(not a.hardware)for name in renderer_names),'Actual native renderer matches the requested graphics configuration')
except BaseException as error:failure=str(error);raise
finally:
 record={'passed':failure is None,'failure':failure,'checks':checks,'binary_sha256':hashlib.sha256(a.binary.read_bytes()).hexdigest(),'fixture_sha256':hashlib.sha256(page).hexdigest(),'script_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'hardware':a.hardware,'webgl_version':2 if a.webgl2 else 1,'renderers':locals().get('renderer_names',[]),'font_source_sha256':hashlib.sha256(font_bytes).hexdigest(),'visual_samples':locals().get('visual_samples',[]),'scope':'Private native embedder; retire only owned contexts, reconstruct live scenes and preserve DOM/resource generations. No driver reset requested.'};(out/'report.json').write_text(json.dumps(record,indent=2)+'\n')
