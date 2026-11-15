#!/usr/bin/env python3
"""Download/Save As/MIME handoff through a real browser on a private Wayland seat."""
import argparse,contextlib,hashlib,http.server,json,os,signal,subprocess,sys,tempfile,threading,time
from pathlib import Path
from urllib.parse import urlsplit,parse_qs
from Xlib import X,XK,display
from Xlib.ext import xtest
from private_wayland import PrivateWayland
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_DOWNLOAD_HANDOFF_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_DOWNLOAD_HANDOFF_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--xvfb',default=os.environ.get('DOME_XVFB','Xvfb'));a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);checks=[];hits=[];failure=None;child=None
payload=b'PK\x03\x04'+bytes(range(256))*4096;blob=b'blob\x00\xffdata';record=None

def stop(proc):
 if proc.poll() is None:
  try:os.killpg(proc.pid,signal.SIGTERM)
  except ProcessLookupError:pass
  try:proc.wait(timeout=8)
  except subprocess.TimeoutExpired:os.killpg(proc.pid,signal.SIGKILL);proc.wait()
def wait(fn,seconds=25):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  if child and child.poll()is not None:raise AssertionError('Browser exited: '+str(child.returncode))
  value=fn()
  if value:return value
  time.sleep(.04)
 raise AssertionError('Timed out; see '+str(out))
def check(ok,label):
 checks.append(dict(name=label,passed=bool(ok)));print(('PASS 'if ok else'FAIL ')+label,flush=True);assert ok,label
try:
 with contextlib.ExitStack()as stack:
  root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-download-handoff-')));root.chmod(0o700)
  config=root/'config';data=root/'data';apps=data/'applications';apps.mkdir(parents=True);config.mkdir();(root/'system-data').mkdir();(root/'system-config').mkdir()
  downloads=root/'Downloads';downloads.mkdir();chosen=root/'Chosen Files';chosen.mkdir()
  (config/'user-dirs.dirs').write_text(f'XDG_DOWNLOAD_DIR="{downloads}"\n')
  record=root/'opened.jsonl';helper=root/'viewer.py'
  helper.write_text('''#!/usr/bin/python3
import hashlib,json,os,sys,urllib.parse
arg=sys.argv[2];path=urllib.parse.unquote(urllib.parse.urlsplit(arg).path)if arg.startswith('file:')else arg
with open(path,'rb')as f:body=f.read()
with open(os.environ['DOME_DOWNLOAD_RECORD'],'a')as f:f.write(json.dumps(dict(handler=sys.argv[1],path=path,sha256=hashlib.sha256(body).hexdigest(),args=sys.argv[2:]))+'\\n')
''');helper.chmod(0o700)
  for kind,field in [('file','%f'),('uri','%u')]:
   (apps/('fixture-'+kind+'.desktop')).write_text(f'[Desktop Entry]\nType=Application\nName=Fixture {kind} handler\nExec="{helper}" {kind} {field}\nMimeType=application/zip;application/pdf;application/x-dome-fixture;\nNoDisplay=true\n')
  scoped=config/'dome-mimeapps.list';scoped.write_text('[Default Applications]\napplication/zip=fixture-file.desktop;\napplication/pdf=fixture-file.desktop;\napplication/x-dome-fixture=fixture-file.desktop;\n')
  (config/'mimeapps.list').write_text('[Default Applications]\napplication/zip=fixture-uri.desktop;\n')
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*_):pass
   def do_POST(self):
    body=self.rfile.read(int(self.headers.get('Content-Length','0')));self.send_file('post',body)
   def do_GET(self):
    parsed=urlsplit(self.path);case=parse_qs(parsed.query).get('case',['file'])[0]
    if parsed.path=='/file':self.send_file(case);return
    if parsed.path=='/inline':
     hits.append(dict(method='GET',path=self.path,cookie=self.headers.get('Cookie','')))
     body=b'saved inline text';self.send_response(200);self.send_header('Content-Type','text/plain');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body);return
    if parsed.path=='/redirect':self.send_response(302);self.send_header('Location','/file?case=redirect');self.end_headers();return
    if parsed.path=='/attribute':body=b'<title>Attribute download</title><a id=a href="/inline" download="hint.txt">save</a><script>a.click()</script>'
    elif parsed.path=='/blob':body=b'''<title>Blob download</title><script>let u=URL.createObjectURL(new Blob([new Uint8Array([98,108,111,98,0,255,100,97,116,97])],{type:'application/octet-stream'}));window.a=document.createElement('a');a.href=u;a.download='blob.bin';a.click();URL.revokeObjectURL(u)</script>'''
    elif parsed.path=='/post-form':body=b'<title>Submitting download</title><form method=post action=/post><input name=mode value=posted></form><script>document.forms[0].submit()</script>'
    elif parsed.path=='/embedded':body=b'<title>Embedded file</title><iframe src="/file?case=embedded"></iframe>'
    elif parsed.path=='/sandbox':body=b'<title>Sandbox test</title><iframe sandbox="allow-scripts" src="/attribute"></iframe>'
    else:body=b'<title>Download test page</title><p>Saved page source</p>'
    self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(body)))
    if parsed.path=='/seed':self.send_header('Set-Cookie','fixture=secret; Path=/; HttpOnly; SameSite=Lax')
    self.end_headers();self.wfile.write(body)
   def send_file(self,case,posted=None):
    hits.append(dict(method='POST'if posted is not None else'GET',path=self.path,cookie=self.headers.get('Cookie',''),body=posted.decode()if posted is not None else''))
    body=b'<title>Attachment HTML</title>'if case=='html'else payload
    kind='text/html'if case=='html'else'application/x-dome-fixture'if case=='custom'else'application/pdf'if case=='pdf'else'application/zip'
    self.send_response(404 if case=='missing'else 200);self.send_header('Content-Type',kind+'; charset=binary')
    self.send_header('Content-Disposition',"attachment; filename*=UTF-8''"+case+"%20report."+('html'if case=='html'else'pdf'if case=='pdf'else'zip'))
    self.send_header('Content-Length',str(len(body)));self.end_headers()
    if case=='truncated':body=body[:4096]
    try:self.wfile.write(body)
    except (BrokenPipeError,ConnectionResetError):pass
    if case=='truncated':self.close_connection=True
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
  origin=f'http://127.0.0.1:{server.server_port}'
  rd,wr=os.pipe();xvfb=subprocess.Popen([a.xvfb,'-displayfd',str(wr),'-screen','0','1400x1000x24','-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL,start_new_session=True);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd)
  connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,root,out);stack.callback(wayland.close)
  env={k:v for k,v in os.environ.items()if not k.startswith('DOME_')};env.update(HOME=str(root),XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(config),XDG_DATA_HOME=str(data),XDG_DATA_DIRS=str(root/'system-data'),XDG_CONFIG_DIRS=str(root/'system-config'),XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),XDG_CURRENT_DESKTOP='Dome',FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket,LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn',DOME_DOWNLOAD_RECORD=str(record),DOME_BROWSER_TRACE='1',LD_LIBRARY_PATH=os.environ.get('DOME_BROWSER_TEST_LIBRARY_PATH',os.environ.get('LD_LIBRARY_PATH','')))
  for name in ['DISPLAY','WAYLAND_SOCKET']:env.pop(name,None)
  log=stack.enter_context((out/'browser.log').open('w'));child=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',str(root/'profile'),origin+'/seed'],env=env,stdout=log,stderr=log,start_new_session=True);stack.callback(stop,child)
  def find(title):return next((w for w in wayland.windows()if w.value()['client_pid']==child.pid and title in str(w.get_wm_name())),None)
  window=wait(lambda:find('Download test page'));window.set_input_focus()
  def key(name,ctrl=False,alt=False):
   modifiers=([XK.XK_Control_L]if ctrl else[])+([XK.XK_Alt_L]if alt else[])
   for sym in modifiers:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(sym))
   sym=XK.string_to_keysym(name)or ord(name);code=connection.keysym_to_keycode(sym);xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
   for sym in reversed(modifiers):xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(sym))
   connection.sync();time.sleep(.08)
  def type_text(text):
   for char in text:
    sym=XK.string_to_keysym(char)or ord(char);code=connection.keysym_to_keycode(sym);shift=connection.keycode_to_keysym(code,0)!=sym and connection.keycode_to_keysym(code,1)==sym
    if shift:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(XK.XK_Shift_L))
    xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
    if shift:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(XK.XK_Shift_L))
    connection.sync();time.sleep(.003)
  def navigate(path):
   window.set_input_focus();key('l',ctrl=True);key('a',ctrl=True);type_text(path if '://'in path or path.startswith('data:')else origin+path);key('Return')
  def ready(action):
   prompt=wait(lambda:find('Download ready'));prompt.set_input_focus();key('o'if action=='open'else's',alt=True)if action!='cancel'else key('Escape')
  def save_as(path=None):
   chooser=wait(lambda:find('Save As'));chooser.set_input_focus()
   if path:key('a',ctrl=True);type_text(str(path))
   key('Return')
  def calls():return [json.loads(line)for line in record.read_text().splitlines()]if record.exists()else[]
  def saved(path,body=payload):return path.exists()and path.read_bytes()==body
  navigate('/file?case=save');ready('save');save_as(chosen/'custom file.zip');wait(lambda:saved(chosen/'custom file.zip'))
  check(True,'Save As chooses a custom folder and filename and preserves the complete binary body')
  check(not list(downloads.iterdir()),'Save As does not leave an extra copy in Downloads')
  check(sum(h['path']=='/file?case=save'for h in hits)==1 and 'fixture=secret'in hits[-1]['cookie'],'Download uses the original authenticated navigation response')
  destination=chosen/'custom file.zip';destination.write_bytes(b'keep existing')
  for accept in [False,True]:
   navigate('/file?case=overwrite');ready('save');save_as(destination)
   confirm=wait(lambda:next((w for w in wayland.windows()if w.value()['client_pid']==child.pid and w.id!=window.id and 'Save As'not in str(w.get_wm_name())),None));confirm.set_input_focus();key('Return'if accept else'Escape');time.sleep(.4)
   if accept:wait(lambda:saved(destination))
   check(destination.read_bytes()==(payload if accept else b'keep existing'),'Overwrite confirmation '+('replaces only the chosen file after acceptance'if accept else'preserves the existing file when cancelled'))
  navigate('/file?case=cancel');ready('save');chooser=wait(lambda:find('Save As'));chooser.set_input_focus();key('Escape');time.sleep(.3)
  check(not (downloads/'cancel report.zip').exists()and not list((root/'cache/dome-browser/downloads').glob('*.part')),'Cancelling Save As removes the staged file')
  for path,case in [('/file?case=domed','domed'),('/post-form','post'),('/redirect','redirect'),('/file?case=custom','custom'),('/file?case=pdf','pdf')]:
   before=len(calls());navigate(path);ready('open');wait(lambda:len(calls())==before+1)
   check(calls()[-1]['sha256']==hashlib.sha256(payload).hexdigest()and len(calls()[-1]['args'])==1,case+': MIME handoff receives the complete original response as one file argument')
  post=[h for h in hits if h['path']=='/post'];check(len(post)==1 and post[0]['body']=='mode=posted'and post[0]['method']=='POST','POST download is not repeated as a GET')
  scoped.unlink();navigate('/file?case=system');ready('open');wait(lambda:len(calls())==6)
  check(calls()[-1]['handler']=='uri','System MIME default supports URI-only applications when no DomeD override exists')
  scoped.write_text('[Default Applications]\napplication/zip=missing.desktop;\n');navigate('/file?case=nohandler');ready('save');save_as();wait(lambda:saved(downloads/'nohandler report.zip'))
  check(len(calls())==6,'An unavailable DomeD override offers Save As without launching another handler')
  navigate('/file?case=html');ready('save');save_as();wait(lambda:saved(downloads/'html report.html',b'<title>Attachment HTML</title>'))
  check(True,'Content-Disposition attachment overrides an otherwise renderable HTML MIME type')
  navigate('/attribute');save_as();wait(lambda:saved(downloads/'hint.txt',b'saved inline text'))
  check('Attribute download'in str(window.get_wm_name()),'The download attribute preserves the page and suggested filename')
  navigate('/blob');save_as();wait(lambda:saved(downloads/'blob.bin',blob))
  check('Blob download'in str(window.get_wm_name()),'Blob download retains its activation-time claim after immediate URL revocation')
  navigate('/seed');wait(lambda:'Download test page'in str(window.get_wm_name()));key('s',ctrl=True);save_as(chosen/'page.html');wait(lambda:(chosen/'page.html').exists())
  check(b'Saved page source'in(chosen/'page.html').read_bytes(),'Ctrl+S uses Save As for the current page source')
  for case in ['missing','truncated']:
   navigate('/file?case='+case);wait(lambda:any(h['path']=='/file?case='+case for h in hits));time.sleep(1)
   check(not find('Download ready')and not (downloads/(case+' report.zip')).exists(),case+': unsuccessful responses never offer or launch incomplete files')
  for path,title in [('/embedded','Embedded file'),('/sandbox','Sandbox test')]:
   navigate(path);wait(lambda:title in str(window.get_wm_name()));time.sleep(.5)
   check(not find('Download ready')and not find('Save As'),title+': embedded and sandboxed pages do not launch download dialogs')
  window.send_event();check(child.wait(timeout=15)==0,'Browser closes cleanly after downloads and native dialogs');child=None
except BaseException as error:failure=str(error);raise
finally:
 (out/'report.json').write_text(json.dumps(dict(passed=failure is None and bool(checks),failure=failure,checks=checks,hits=hits,binary_sha256=hashlib.sha256(a.binary.read_bytes()).hexdigest(),scope='Real native Wayland browser, private HTTP responses, native Save As and isolated desktop MIME handlers'),indent=2)+'\n')
