#!/usr/bin/env python3
"""Verify PDF navigation responses reach the desktop's PDF application intact."""
from pathlib import Path
import argparse,contextlib,gzip,hashlib,http.server,json,os,resource,signal,subprocess,sys,tempfile,threading,time
from urllib.parse import parse_qs,urlsplit
from Xlib import X,XK,display
from Xlib.ext import xtest
from private_wayland import PrivateWayland
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_PDF_TEST_BUS')!='1':os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_PDF_TEST_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);checks=[];failure=None;child=None;hits=[];completed=set();fetch_errors=[];release=threading.Event()
source=Path(__file__).resolve().parents[3];payload=(source/'tests/wpt/tests/html/browsers/browsing-the-web/navigating-across-documents/resources/portable-document-format-sample-valid.pdf').read_bytes();digest=hashlib.sha256(payload).hexdigest()
def stop(proc):
 if proc.poll() is None:
  proc.terminate()
  try:proc.wait(timeout=8)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()
def check(value,label):
 checks.append(dict(name=label,passed=bool(value)));print(('PASS ' if value else 'FAIL ')+label,flush=True);assert value,label
def wait(fn,seconds=20):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  if child and child.poll() is not None:raise AssertionError('Browser exited: '+str(child.returncode))
  value=fn()
  if value:return value
  time.sleep(.03)
 raise AssertionError('PDF fixture timed out')
try:
 with contextlib.ExitStack() as stack:
  root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-pdf-')));root.chmod(0o700)
  config=root/'config';data=root/'data';apps=data/'applications';apps.mkdir(parents=True);config.mkdir();(root/'system-data').mkdir();(root/'system-config').mkdir()
  downloads=root/'PDF downloads';(config/'user-dirs.dirs').write_text(f'XDG_DOWNLOAD_DIR="{downloads}"\n')
  record=root/'viewer.jsonl';helper=root/'PDF viewer fixture.py'
  helper.write_text('''#!/usr/bin/python3
import hashlib,json,os,sys,urllib.parse
arg=sys.argv[1];path=urllib.parse.unquote(urllib.parse.urlsplit(arg).path) if arg.startswith('file:') else arg
with open(path,'rb') as f:body=f.read()
with open(os.environ['DOME_PDF_TEST_RECORD'],'a') as f:f.write(json.dumps(dict(args=sys.argv[1:],name=os.path.basename(path),sha256=hashlib.sha256(body).hexdigest(),size=len(body),permissions=os.stat(path).st_mode&0o777))+'\\n')
''');helper.chmod(0o700)
  original=helper.read_text();helper.write_text(original.replace('dict(args=',"dict(handler='file',args="));uri_helper=root/'PDF URI viewer fixture.py';uri_helper.write_text(original.replace('dict(args=',"dict(handler='uri',args="));uri_helper.chmod(0o700)
  for kind,field in [('file','%f'),('uri','%u')]:
   (apps/('pdf-'+kind+'.desktop')).write_text('[Desktop Entry]\nType=Application\nName=PDF fixture '+kind+'\nExec="'+str(helper if kind=='file' else uri_helper)+'" '+field+'\nMimeType=application/pdf;\nNoDisplay=true\n')
  scoped=config/'dome-mimeapps.list';scoped.write_text('[Default Applications]\napplication/pdf=pdf-file.desktop;\n');(config/'mimeapps.list').write_text('[Default Applications]\napplication/pdf=pdf-uri.desktop;\n')
  class Site(http.server.BaseHTTPRequestHandler):
   def log_message(self,*_):pass
   def do_POST(self):
    body=self.rfile.read(int(self.headers.get('Content-Length','0')))
    if self.path=='/post':self.document('post',body);return
    if self.path=='/fetch-result':fetch_errors.append(body.decode())
    self.send_response(204);self.end_headers()
   def do_GET(self):
    q=parse_qs(urlsplit(self.path).query);case=q.get('case',['document'])[0]
    if self.path.startswith('/download'):self.document(case);return
    if self.path=='/redirect':self.send_response(302);self.send_header('Location','/download?case=redirect');self.end_headers();hits.append(dict(method='GET',path=self.path));return
    if self.path=='/post-form':body=b'<title>Submitting PDF form</title><form method=post action=/post><input name=mode value=posted></form><script>document.forms[0].submit()</script>'
    elif self.path=='/embedded':body=b'<title>Embedded PDF fixture</title><iframe src="/download?case=embedded"></iframe>'
    elif self.path=='/fetch-test':body=b"<title>Body error fixture</title><script>fetch('/download?case=fetch-truncated').then(r=>r.arrayBuffer()).then(()=>fetch('/fetch-result',{method:'POST',body:'unexpected success'}),()=>fetch('/fetch-result',{method:'POST',body:'body rejected'}))</script>"
    elif self.path=='/html.pdf':body=b'<title>HTML with PDF suffix</title><p>This is HTML'
    else:body=b'<title>PDF handoff fixture</title><body style="background:#234;color:white">PDF handoff fixture'
    self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(body)))
    if self.path=='/seed':self.send_header('Set-Cookie','fixture=pdf; Path=/; HttpOnly; SameSite=Lax')
    self.end_headers();self.wfile.write(body)
   def document(self,case,posted=None):
    hits.append(dict(method='POST' if posted is not None else 'GET',path=self.path,cookie=self.headers.get('Cookie',''),body=None if posted is None else posted.decode()))
    encoded=gzip.compress(payload) if case=='gzip' else payload
    body=encoded if case not in ['truncated','fetch-truncated'] else payload[:max(2048,len(payload)//2)]
    self.send_response(200);self.send_header('Content-Type','application/pdf; charset=binary');self.send_header('Content-Disposition',"inline; filename*=UTF-8''"+case+"%20document.pdf");self.send_header('Content-Length',str(len(encoded)));
    if case=='gzip':self.send_header('Content-Encoding','gzip')
    self.end_headers()
    try:
     for at in range(0,len(body),256):
      self.wfile.write(body[at:at+256]);self.wfile.flush()
      if case=='cancel' and at==2048:release.wait(10)
      time.sleep(.001)
    except (BrokenPipeError,ConnectionResetError):pass
    finally:completed.add(case);self.close_connection=True
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Site);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown);stack.callback(release.set)
  origin=f'http://127.0.0.1:{server.server_port}'
  rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1400x1000x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd)
  connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,root,out);stack.callback(wayland.close)
  env={k:v for k,v in os.environ.items() if not k.startswith('DOME_')};env.update(HOME=str(root),XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(config),XDG_DATA_HOME=str(data),XDG_DATA_DIRS=str(root/'system-data'),XDG_CONFIG_DIRS=str(root/'system-config'),XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),XDG_CURRENT_DESKTOP='Dome',FLTK_BACKEND='wayland',WAYLAND_DISPLAY=wayland.socket,LIBGL_ALWAYS_SOFTWARE='1',RUST_LOG='warn',DOME_PDF_TEST_RECORD=str(record))
  for name in ['DISPLAY','WAYLAND_SOCKET']:env.pop(name,None)
  log=stack.enter_context((out/'browser.log').open('w'));child=subprocess.Popen([str(a.binary.resolve()),'--software','--no-restore','--profile',str(root/'profile'),origin+'/seed'],env=env,stdout=log,stderr=log);stack.callback(stop,child)
  window=wait(lambda:next((w for w in wayland.windows() if w.value()['client_pid']==child.pid and str(w.get_wm_name()).startswith('PDF handoff fixture')),None));window.set_input_focus();check(True,'Browser loads the private PDF fixture')
  def calls():return [json.loads(l) for l in record.read_text().splitlines()] if record.exists() else []
  def key(symbol,ctrl=False):
   if ctrl:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(XK.XK_Control_L))
   code=connection.keysym_to_keycode(symbol);xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
   if ctrl:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(XK.XK_Control_L))
   connection.sync();time.sleep(.005)
  def navigate(url):
   window.set_input_focus();key(XK.XK_l,True);key(XK.XK_a,True)
   for char in url:
    symbol=XK.string_to_keysym(char) or ord(char);code=connection.keysym_to_keycode(symbol);shift=connection.keycode_to_keysym(code,0)!=symbol and connection.keycode_to_keysym(code,1)==symbol
    if shift:xtest.fake_input(connection,X.KeyPress,connection.keysym_to_keycode(XK.XK_Shift_L))
    xtest.fake_input(connection,X.KeyPress,code);xtest.fake_input(connection,X.KeyRelease,code)
    if shift:xtest.fake_input(connection,X.KeyRelease,connection.keysym_to_keycode(XK.XK_Shift_L))
    connection.sync();time.sleep(.002)
   key(XK.XK_Return)
  def opened(path,count):
   navigate(origin+path);wait(lambda:len(calls())==count);c=calls()[-1]
   check(c['sha256']==digest and c['size']==len(payload),'Viewer receives the entire original PDF response '+str(count));check(c['permissions']==0o600,'Saved PDF uses private file permissions '+str(count));return c
  first=opened('/download?case=domed',1)
  check(first['handler']=='file' and len(first['args'])==1 and not first['args'][0].startswith('file:'),'DomeD PDF preference supports a file-only desktop application')
  check(first['name']=='domed document.pdf','PDF MIME detection handles an extensionless URL and UTF-8 disposition filename')
  opened('/post-form',2);post=[h for h in hits if h['path']=='/post'];check(len(post)==1 and post[0]['method']=='POST' and post[0]['body']=='mode=posted' and 'fixture=pdf' in post[0]['cookie'],'Authenticated PDF POST is handed off without repeating the request')
  opened('/redirect',3);check(sum(h['path']=='/redirect' for h in hits)==1 and sum(h['path']=='/download?case=redirect' for h in hits)==1,'Redirected PDF is handed off without a second download')
  local=root/'local fixture.pdf';local.write_bytes(payload);navigate(local.as_uri());wait(lambda:len(calls())==4);check(calls()[-1]['sha256']==digest,'Local PDF files open through the same default viewer')
  scoped.unlink();standard=opened('/download?case=system',5);check(standard['handler']=='uri','System MIME default is used when DomeD has no PDF override')
  navigate(origin+'/html.pdf');wait(lambda:str(window.get_wm_name()).startswith('HTML with PDF suffix'));time.sleep(.3);check(len(calls())==5,'HTML with a PDF filename remains HTML')
  navigate(origin+'/embedded');wait(lambda:'embedded' in completed);time.sleep(.4);check(len(calls())==5,'Embedded PDFs do not automatically launch desktop applications')
  navigate(origin+'/download?case=truncated');wait(lambda:'truncated' in completed);wait(lambda:not (downloads/'truncated document.pdf').exists());check(len(calls())==5 and not (downloads/'truncated document.pdf').exists(),'Incomplete PDF responses are not opened or retained as complete files')
  navigate(origin+'/download?case=cancel');wait(lambda:(downloads/'cancel document.pdf').exists() and (downloads/'cancel document.pdf').stat().st_size>0);navigate(origin+'/seed');wait(lambda:str(window.get_wm_name()).startswith('PDF handoff fixture'));release.set();wait(lambda:'cancel' in completed);time.sleep(.3);check(len(calls())==5 and not (downloads/'cancel document.pdf').exists(),'Navigating away cancels the partial PDF handoff')
  scoped.write_text('[Default Applications]\napplication/pdf=missing.desktop;\n');navigate(origin+'/download?case=missing');wait(lambda:'missing' in completed);wait(lambda:(downloads/'missing document.pdf').exists() and (downloads/'missing document.pdf').stat().st_size==len(payload));time.sleep(.3);check(len(calls())==5 and child.poll() is None,'A missing configured viewer preserves the saved PDF without choosing another application')
  check(all(sum(h['path']==path for h in hits)==1 for path in ['/download?case=domed','/download?case=system']),'PDF handoff uses original navigation streams')
  scoped.write_text('[Default Applications]\napplication/pdf=pdf-uri.desktop;\n');opened('/download?case=gzip',6)
  navigate(origin+'/fetch-test');wait(lambda:fetch_errors);check(fetch_errors==['body rejected'],'JavaScript response consumption also rejects an interrupted body')
  wayland.api('/api/action',{'op':'close','id':window.id});check(child.wait(timeout=10)==0,'Browser closes cleanly after PDF handoffs');child=None
except BaseException as error:failure=str(error);raise
finally:
 (out/'report.json').write_text(json.dumps(dict(passed=failure is None and bool(checks),failure=failure,checks=checks,binary_sha256=hashlib.sha256(a.binary.read_bytes()).hexdigest(),pdf_fixture_sha256=digest,scope='Native Wayland browser with isolated PDF desktop handlers, original authenticated/POST/redirect/local response bytes; no user default applications changed'),indent=2)+'\n')
