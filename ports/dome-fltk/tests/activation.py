#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise single-instance activation replies on a private session bus."""
import argparse,asyncio,fcntl,hashlib,json,os,signal,sys,tempfile,time
from pathlib import Path
from dbus_next import Message,MessageType,RequestNameReply
from dbus_next.aio import MessageBus
signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_ACTIVATION_TEST_BUS')!='1':
 os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_ACTIVATION_TEST_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--expect-silent-failure',action='store_true');a=p.parse_args()
binary=a.binary.resolve();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False,mode=0o700);checks=[];runs=[]
def check(ok,name):checks.append({'name':name,'passed':bool(ok)});assert ok,name
class Application:
 def __init__(self,bus):self.bus=bus;self.mode='healthy';self.calls=[];self.pending=[]
 def handle(self,message):
  if message.message_type!=MessageType.METHOD_CALL or message.path!='/org/domed/Browser' or message.interface!='org.freedesktop.Application':return None
  self.calls.append({'method':message.member,'uris':message.body[0] if message.member=='Open' else []})
  if self.mode=='busy':
   self.pending.append(asyncio.get_running_loop().call_later(4,lambda:self.bus.send(Message.new_method_return(message))))
   return True
  if self.mode=='reject':return Message.new_error(message,'org.freedesktop.DBus.Error.Failed','Fixture activation refused')
  return Message.new_method_return(message)
async def main():
 bus=await MessageBus().connect();app=Application(bus);bus.add_message_handler(app.handle)
 check(await bus.request_name('org.domed.Browser')==RequestNameReply.PRIMARY_OWNER,'Private fixture owns activation name')
 try:
  with tempfile.TemporaryDirectory(prefix='dome-activation-') as folder:
   root=Path(folder);root.chmod(0o700);profile=root/'profile';profile.mkdir();sentinel=profile/'session.json';saved=b'{"fixture":"unchanged"}\n';sentinel.write_bytes(saved)
   with (profile/'browser.lock').open('wb') as lock:
    fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB);original_inode=os.fstat(lock.fileno()).st_ino
    env={**os.environ,'HOME':folder,'XDG_CONFIG_HOME':folder+'/config','XDG_DATA_HOME':folder+'/data','XDG_STATE_HOME':folder+'/state','XDG_CACHE_HOME':folder+'/cache','WAYLAND_DISPLAY':'missing-test-display','FLTK_BACKEND':'wayland'};env.pop('DISPLAY',None);env.pop('WAYLAND_SOCKET',None)
    cases=[('healthy-activate','healthy','about:blank',True),('healthy-open','healthy','https://example.invalid/activation-fixture',True),('busy-activate','busy','about:blank',False),('rejected-open','reject','https://example.invalid/activation-fixture',False)]
    for name,mode,url,success in cases:
     app.mode=mode;before=len(app.calls);start=time.monotonic()
     child=await asyncio.create_subprocess_exec(str(binary),'--profile',str(profile),url,env=env,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.PIPE)
     try:stdout,stderr=await asyncio.wait_for(child.communicate(),8)
     except BaseException:child.kill();await child.wait();raise
     elapsed=time.monotonic()-start;(out/(name+'.log')).write_bytes(stdout+stderr)
     expected_success=success or a.expect_silent_failure
     check((child.returncode==0)==expected_success,name+': exit status reflects activation outcome')
     check(len(app.calls)==before+1 and app.calls[-1]=={'method':'Activate' if url=='about:blank' else 'Open','uris':[] if url=='about:blank' else [url]},name+': request reaches the existing owner')
     if not success and not a.expect_silent_failure:check(b'Could not activate the existing Dome Browser instance' in stderr,name+': failure explains the existing-instance blocker')
     if mode=='busy':check(2.8<=elapsed<6,name+': unresponsive owner has a bounded wait')
     check(sentinel.read_bytes()==saved and {x.name for x in profile.iterdir()}=={'session.json','browser.lock'},name+': no competing profile writes')
     check((profile/'browser.lock').stat().st_ino==original_inode,name+': held profile lock is preserved')
     runs.append({'name':name,'passed':True,'elapsed_seconds':elapsed,'exit_code':child.returncode});print('PASS',name,flush=True)
 finally:
  for pending in app.pending:pending.cancel()
  bus.disconnect()
try:asyncio.run(main())
finally:(out/'report.json').write_text(json.dumps({'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'runs':runs,'checks':checks,'passed':len(runs)==4 and all(c['passed'] for c in checks),'silent_failure_baseline':a.expect_silent_failure,'scope':'Private D-Bus fixture; unavailable display and a held fixture profile lock. No user session, existing browser or live profile is accessed.'},indent=2)+'\n')
