#!/usr/bin/env python3
"""Test gamepad policy reporting with private documents and no controller gestures."""
from pathlib import Path
import argparse,contextlib,datetime,hashlib,http.server,json,os,resource,signal,subprocess,sys,tempfile,threading,time
from urllib.parse import urlsplit,parse_qs
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_GAMEPAD_POLICY_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_GAMEPAD_POLICY_TEST_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--cases',nargs='+');a=p.parse_args()
HERE=Path(__file__).resolve().parent;W=next((path for path in HERE.parents if(path/'components/script').is_dir()),Path.cwd());T=W/'ports/dome-fltk/tests';assert(T/'private_wayland.py').is_file(),'Run from the Servo checkout root';sys.path.insert(0,str(T))
from Xlib import display
from private_wayland import PrivateWayland
out=a.output.resolve();out.mkdir(mode=0o700,parents=True,exist_ok=False);sha=lambda path:hashlib.sha256(path.read_bytes()).hexdigest();fixture=(HERE/'gamepad-policy-reporting.html').read_text()
allow='gamepad=*';deny='gamepad=();report-to=enforced';ro_deny='gamepad=();report-to=reportonly'
def case(operation='read',enforced=deny,report_only=allow,count=1,disposition='enforce',destination='enforced',allowed=False,calls=1):
    return dict(operation=operation,enforced=enforced,report_only=report_only,expected_reports=count,disposition=disposition,destination=destination,allowed=allowed,calls=calls)
cases={
 'enforce':case(),
 'report-only':case(enforced=allow,report_only=ro_deny,disposition='report',destination='reportonly',allowed=True),
 'precedence':case(report_only=ro_deny),
 'allowed':case(enforced=allow,count=0,allowed=True),
 'observer-only':case(enforced='gamepad=()',destination=None),
 'unknown-endpoint':case(enforced='gamepad=();report-to=missing',destination=None),
 'repeated':case(calls=2,count=2),
 'introspection':case(operation='query',count=0),
 'query-report-only':case(operation='query',enforced=allow,report_only=ro_deny,count=0,allowed=True),
 'listeners':case(operation='listeners',count=0),
 'detached':case(operation='detached',enforced=allow,count=0,allowed=True),
 'default-cross':case(operation='cross',enforced='',report_only='',count=0,allowed=True),
 'buffer':case(calls=110,count=110),
 'snapshot':case(),
}
if a.cases:
    assert all(name in cases for name in a.cases);cases={name:cases[name]for name in a.cases}
results,deliveries,errors={}, {}, [];servers=[]
def stop(proc):
    if proc.poll()is None:
        proc.terminate()
        try:proc.wait(timeout=8)
        except subprocess.TimeoutExpired:proc.kill();proc.wait()
class FixtureServer(http.server.ThreadingHTTPServer):request_queue_size=256
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_GET(self):
        url=urlsplit(self.path);query=parse_qs(url.query);name=query.get('case',[''])[0]
        if url.path=='/gamepad-policy-caller.js':
            body=(HERE/'gamepad-policy-caller.js').read_bytes();self.send_response(200);self.send_header('Content-Type','text/javascript');self.end_headers();self.wfile.write(body);return
        if url.path=='/child':
            if name=='detached':body=b'<!doctype html><p>Detached gamepad policy control</p>'
            else:body=b'<!doctype html><script>let reports=[];new ReportingObserver(batch=>reports.push(...batch),{types:["permissions-policy-violation"]}).observe();let result;try{result={allowed:true,length:navigator.getGamepads().length}}catch(error){result={allowed:false,error:error.name}}setTimeout(()=>parent.postMessage({...result,reports:reports.length},"*"),100)</script>'
            self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(body)))
            if name=='detached':self.send_header('Permissions-Policy','gamepad=()')
            self.end_headers();self.wfile.write(body);return
        if url.path!='/fixture'  or name not in cases:self.send_error(404);return
        cfg=dict(cases[name],case=name,cross_url=f'http://127.0.0.1:{servers[1].server_port}/child?case={name}');body=fixture.replace('CONFIGURATION',json.dumps(cfg)).encode()
        self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8');self.send_header('Content-Length',str(len(body)));self.send_header('Permissions-Policy',cfg['enforced']);self.send_header('Permissions-Policy-Report-Only',cfg['report_only']);self.send_header('Reporting-Endpoints',', '.join(f'{key}="/__endpoint?case={name}&name={key}"'for key in ['enforced','reportonly','default']));self.end_headers();self.wfile.write(body)
    def do_POST(self):
        url=urlsplit(self.path);query=parse_qs(url.query);name=query.get('case',[''])[0]
        try:
            data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if url.path=='/__result':results[name]=data
            elif url.path=='/__endpoint':
                assert self.headers.get_content_type()=='application/reports+json'and isinstance(data,list);deliveries.setdefault(name,{}).setdefault(query['name'][0],[]).extend(data)
            else:raise ValueError('Unexpected fixture POST')
            self.send_response(204);self.end_headers()
        except Exception as error:errors.append(str(error));self.send_error(400)
runs=[]
with contextlib.ExitStack()as stack:
    root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-gamepad-policy-')));root.chmod(0o700);rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1200x900x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd);connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,root,out);stack.callback(wayland.close)
    for _ in range(2):
        server=FixtureServer(('127.0.0.1',0),Handler);servers.append(server);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
    env={k:v for k,v in os.environ.items()if not k.startswith(('DOME_','SERVO_','XR_INPUT_'))and k not in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']};env.update(HOME=str(root),DISPLAY=screen,WAYLAND_DISPLAY=wayland.socket,XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),XDG_CACHE_HOME=str(root/'cache'),LIBGL_ALWAYS_SOFTWARE='1',FONTCONFIG_FILE='/etc/fonts/fonts.conf',RUST_LOG='warn')
    env['XR_INPUT_POLICY_TEST']='1'
    for index,(name,cfg)in enumerate(cases.items()):
        env['XR_INPUT_PROFILE']=str(root/f'profile-{index}')
        with(out/(name+'.log')).open('w')as log:
            proc=subprocess.Popen([str(a.binary),f'http://127.0.0.1:{servers[0].server_port}/fixture?case={name}'],stdin=subprocess.PIPE,stdout=log,stderr=log,text=True,env=env);stack.callback(stop,proc);deadline=time.monotonic()+25
            while name not in results and proc.poll()is None and time.monotonic()<deadline:time.sleep(.025)
            row=results.get(name,{'checks':[],'reports':[],'error':'No fixture result within deadline'});time.sleep(.2);live=proc.poll()is None
            if live:proc.stdin.write('{"op":"quit","token":1}\n');proc.stdin.flush()
            try:proc.wait(timeout=10)
            except subprocess.TimeoutExpired:stop(proc)
        groups=deliveries.get(name,{});count=cfg['expected_reports']if cfg['destination']else 0;expected_groups={cfg['destination']:count}if count else{};actual_groups={key:len(value)for key,value in groups.items()};wire=[report for value in groups.values()for report in value]
        target=f'http://127.0.0.1:{servers[0].server_port}/fixture?case={name}'
        schema=all(report.get('type')=='permissions-policy-violation'and report.get('url')==target and isinstance(report.get('age'),int)and report['age']>=0 and report.get('body',{}).get('featureId')=='gamepad'and report['body'].get('disposition')==cfg['disposition']and 'allowAttribute'not in report['body']and'srcAttribute'not in report['body']for report in wire)
        snapshots=[report['body']for report in row.get('reports',[])];wire_bodies=[report['body']for report in wire]
        match=sorted(json.dumps(body,sort_keys=True)for body in wire_bodies)==sorted(json.dumps(body,sort_keys=True)for body in snapshots)if count else not wire
        row['checks'] += [{'name':'Response-owned endpoints receive exact gamepad-report counts','passed':actual_groups==expected_groups},{'name':'Gamepad endpoint body and report age are correct','passed':schema},{'name':'Endpoint bodies agree with observer snapshots','passed':match},{'name':'Browser stays live until requested exit','passed':live},{'name':'Browser closes cleanly','passed':proc.returncode==0}]
        row.update(name=name,exit_code=proc.returncode,actual_endpoints=actual_groups,expected_endpoints=expected_groups);row['passed']=not row.get('error')and all(check['passed']for check in row['checks']);runs.append(row);print(name,'PASS'if row['passed']else'FAIL',len(row['checks']),row.get('error'),actual_groups,flush=True)
record=dict(binary=str(a.binary),binary_sha256=sha(a.binary),source_sha256={p.name:sha(p)for p in [Path(__file__),HERE/'gamepad-policy-reporting.html',HERE/'gamepad-policy-caller.js']},runs=runs,request_errors=errors,runtime_checks=sum(len(row['checks'])for row in runs),passed=not errors and all(row['passed']for row in runs),finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),scope='Private local gamepad policy-reporting checks; software rendering, separate profiles, inline-only registry and no controller gesture. Geolocation is excluded. No physical-device or whole-API conformance claim.');(out/'report.json').write_text(json.dumps(record,indent=2)+'\n');assert record['passed'],'Gamepad policy reporting regression failed'
