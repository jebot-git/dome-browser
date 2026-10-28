#!/usr/bin/env python3
"""Test potential iframe policy diagnostics using private local documents only."""
from pathlib import Path
import argparse,contextlib,datetime,hashlib,http.server,json,os,resource,signal,struct,subprocess,sys,tempfile,threading,time
from urllib.parse import urlsplit,parse_qs
signal.pthread_sigmask(signal.SIG_SETMASK,[]);resource.setrlimit(resource.RLIMIT_CORE,(0,0))
if os.environ.get('DOME_IFRAME_REPORT_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],{**os.environ,'DOME_IFRAME_REPORT_TEST_BUS':'1'})
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--cases',nargs='+');a=p.parse_args()
HERE=Path(__file__).resolve().parent;W=next((path for path in HERE.parents if(path/'components/script').is_dir()),Path.cwd());T=W/'ports/dome-fltk/tests';assert(T/'private_wayland.py').is_file(),'Run from the Servo checkout root';sys.path.insert(0,str(T))
from Xlib import display
from private_wayland import PrivateWayland
out=a.output.resolve();out.mkdir(mode=0o700,parents=True,exist_ok=False);sha=lambda path:hashlib.sha256(path.read_bytes()).hexdigest();fixture=(HERE/'iframe-policy-reporting.html').read_text()
other='geolocation=*,gamepad=*, ';allow=other+'xr-spatial-tracking=*;report-to=enforced';ro_allow=other+'xr-spatial-tracking=*;report-to=reportonly';deny=other+'xr-spatial-tracking=();report-to=enforced';ro_deny=other+'xr-spatial-tracking=();report-to=reportonly';all_attributes='geolocation *; gamepad *; xr-spatial-tracking *'
def case(enforced=deny,report_only=ro_allow,count=1,disposition='enforce',destination='enforced',policy=False,**extra):
    return dict(enforced=enforced,report_only=report_only,expected_count=count,disposition=disposition,destination=destination,expected_policy=policy,expected_loads=1,allow_present=True,allow=all_attributes,src_present=True,src_mode='http',srcdoc=False,sandbox='',action='',**extra)
cases={
 'enforce':case(),
 'report-only':case(enforced=allow,report_only=ro_deny,disposition='report',destination='reportonly',policy=True),
 'precedence':case(report_only=ro_deny),
 'attribute-denied':case(enforced=allow,report_only=ro_allow,attribute_override="geolocation *; gamepad *; xr-spatial-tracking 'none'"),
 'allowed':case(enforced=allow,count=0,policy=True),
 'observer-only':case(enforced=other+'xr-spatial-tracking=()',destination=None),
 'unknown-endpoint':case(enforced=other+'xr-spatial-tracking=();report-to=missing',destination=None),
 'missing-attributes':case(attributes_missing=True,srcdoc_mode=True),
 'empty-attributes':case(attributes_empty=True,srcdoc_mode=True),
 'header-self-cross':case(enforced=other+'xr-spatial-tracking=self;report-to=enforced',src_mode_override='cross'),
 'declared-redirect':case(enforced=other+'xr-spatial-tracking=self;report-to=enforced',count=0,policy=True,src_mode_override='redirect',attribute_override="geolocation *; gamepad *; xr-spatial-tracking 'src'"),
 'child-response-policy':case(enforced=allow,count=0,policy=True,child_denied=True),
 'srcdoc-origin':case(enforced=other+'xr-spatial-tracking=self;report-to=enforced',count=0,policy=True,src_mode_override='cross',srcdoc_mode=True),
 'opaque-src':case(enforced=allow,count=0,policy=True,srcdoc_mode=True,sandbox_override='allow-scripts',attribute_override="geolocation *; gamepad *; xr-spatial-tracking 'src'"),
 'onload-remove':case(action_override='remove'),
 'onload-allow':case(enforced=allow,count=0,policy=True,action_override='allow',attribute_override="geolocation *; gamepad *; xr-spatial-tracking 'none'",after_allow=all_attributes),
 'onload-restrict':case(enforced=allow,action_override='allow',after_allow="geolocation *; gamepad *; xr-spatial-tracking 'none'"),
 'onload-navigate':case(action_override='navigate',count=2,loads_override=2),
 'onload-adopt':case(action_override='adopt',count=0,policy=True),
 'unattached':case(count=0,loads_override=0),
 'unicode-attributes':case(srcdoc_mode=True,unicode_attributes=True),
 'buffer':case(count=110,loads_override=110),
 'snapshot':case(),
}
if a.cases:
    assert all(name in cases for name in a.cases);cases={name:cases[name]for name in a.cases}
results,deliveries,errors={}, {}, [];servers=[]
def units(text):
    if text is None:return None
    b=text.encode('utf-16-le','surrogatepass');return list(struct.unpack('<'+'H'*(len(b)//2),b))
def stop(proc):
    if proc.poll()is None:
        proc.terminate()
        try:proc.wait(timeout=8)
        except subprocess.TimeoutExpired:proc.kill();proc.wait()
class FixtureServer(http.server.ThreadingHTTPServer):request_queue_size=256
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_GET(self):
        url=urlsplit(self.path);query=parse_qs(url.query);name=query.get('case',[''])[0];config=cases.get(name,{})
        if url.path=='/redirect':
            self.send_response(302);self.send_header('Location',f'http://127.0.0.1:{servers[1].server_port}/child?case={name}');self.end_headers();return
        headers=[]
        if url.path in ['/child','/child-again']:
            body=b'<!doctype html><p>Private child loaded</p>'
            if config.get('child_denied'):headers.append(('Permissions-Policy','xr-spatial-tracking=()'))
        elif url.path=='/fixture'and name in cases:
            cfg=dict(config,case=name);base=f'http://127.0.0.1:{servers[0].server_port}';cross=f'http://127.0.0.1:{servers[1].server_port}'
            cfg['src']=(cross if cfg.get('src_mode_override')=='cross'else base)+('/redirect'if cfg.get('src_mode_override')=='redirect'else'/child')+f'?case={name}'
            cfg['after_src']=base+f'/child-again?case={name}';cfg['expected_loads']=cfg.get('loads_override',cfg['expected_loads'])
            for field in ['action','sandbox','allow']:
                key='attribute_override'if field=='allow'else field+'_override'
                if key in cfg:cfg[field]=cfg[key]
            if cfg.get('srcdoc_mode'):cfg['srcdoc']=True
            if cfg.get('attributes_missing'):cfg.update(allow_present=False,src_present=False)
            if cfg.get('attributes_empty'):cfg.update(allow='',src='')
            if cfg.get('unicode_attributes'):cfg.update(allow='xr-spatial-tracking; note żółć🦊\ud800a\udc00"\\\0',src='raw 🦊\ud800b\udc00"\\\n\0')
            body=fixture.replace('CONFIGURATION',json.dumps(cfg)).encode();headers.extend([('Permissions-Policy',cfg['enforced']),('Permissions-Policy-Report-Only',cfg['report_only'])]);headers.append(('Reporting-Endpoints',', '.join(f'{key}="/__endpoint?case={name}&name={key}&batch=a,b";ignored=?1'for key in ['enforced','reportonly','default'])))
        else:self.send_error(404);return
        self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8');self.send_header('Content-Length',str(len(body)))
        for key,value in headers:self.send_header(key,value)
        self.end_headers();self.wfile.write(body)
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
    root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='dome-iframe-reporting-')));root.chmod(0o700);rd,wr=os.pipe();xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1200x900x24','-nolisten','tcp'],pass_fds=(wr,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);stack.callback(stop,xvfb);os.close(wr);screen=':'+os.read(rd,64).decode().strip();os.close(rd);connection=display.Display(screen);stack.callback(connection.close);wayland=PrivateWayland(connection,screen,root,out);stack.callback(wayland.close)
    for _ in range(2):
        server=FixtureServer(('127.0.0.1',0),Handler);servers.append(server);threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
    env={k:v for k,v in os.environ.items()if not k.startswith(('DOME_','SERVO_','XR_INPUT_'))and k not in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']};env.update(HOME=str(root),DISPLAY=screen,WAYLAND_DISPLAY=wayland.socket,XDG_RUNTIME_DIR=str(root),XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),XDG_CACHE_HOME=str(root/'cache'),LIBGL_ALWAYS_SOFTWARE='1',FONTCONFIG_FILE='/etc/fonts/fonts.conf',RUST_LOG='warn')
    for index,(name,cfg)in enumerate(cases.items()):
        env['XR_INPUT_PROFILE']=str(root/f'profile-{index}')
        with(out/(name+'.log')).open('w')as log:
            proc=subprocess.Popen([str(a.binary),f'http://127.0.0.1:{servers[0].server_port}/fixture?case={name}'],stdin=subprocess.PIPE,stdout=log,stderr=log,text=True,env=env);stack.callback(stop,proc);deadline=time.monotonic()+(70 if name=='buffer'else 35)
            while name not in results and proc.poll()is None and time.monotonic()<deadline:time.sleep(.025)
            row=results.get(name,{'checks':[],'expected':[],'error':'No fixture result within deadline'});time.sleep(.2);live=proc.poll()is None
            if live:proc.stdin.write('{"op":"quit","token":1}\n');proc.stdin.flush()
            try:proc.wait(timeout=10)
            except subprocess.TimeoutExpired:stop(proc)
        groups=deliveries.get(name,{});expected_count=cfg['expected_count']if cfg['destination']else 0;expected_groups={cfg['destination']:expected_count}if expected_count else{};actual_groups={key:len(value)for key,value in groups.items()};wire=[report for value in groups.values()for report in value]
        schema=all(report.get('type')=='potential-permissions-policy-violation'and report.get('body',{}).get('featureId')=='xr-spatial-tracking'and report['body'].get('disposition')==cfg['disposition']and 'allowAttribute'in report['body']and 'srcAttribute'in report['body']for report in wire)
        expected_attributes=row['expected'];wire_attributes=[{'allow':units(report['body']['allowAttribute']),'src':units(report['body']['srcAttribute'])}for report in wire if report.get('type')=='potential-permissions-policy-violation'and'allowAttribute'in report.get('body',{})and'srcAttribute'in report['body']]
        # Delivery batches may complete in a different order; source snapshots must agree.
        attribute_match=sorted(map(json.dumps,wire_attributes))==sorted(map(json.dumps,expected_attributes))if expected_count else not wire
        row['checks'] += [{'name':'Only the parent-selected endpoint receives exact potential-report counts','passed':actual_groups==expected_groups},{'name':'Endpoint potential body schema is correct','passed':schema},{'name':'Network attributes preserve observer UTF-16 snapshots','passed':attribute_match},{'name':'Browser stays live until requested exit','passed':live},{'name':'Browser closes cleanly','passed':proc.returncode==0}]
        row.update(name=name,exit_code=proc.returncode,actual_endpoints=actual_groups,expected_endpoints=expected_groups);row['passed']=not row.get('error')and all(check['passed']for check in row['checks']);runs.append(row);print(name,'PASS'if row['passed']else'FAIL',len(row['checks']),row.get('error'),actual_groups,flush=True)
record=dict(binary=str(a.binary),binary_sha256=sha(a.binary),source_sha256={p.name:sha(p)for p in [Path(__file__),HERE/'iframe-policy-reporting.html']},runs=runs,request_errors=errors,runtime_checks=sum(len(row['checks'])for row in runs),passed=not errors and all(row['passed']for row in runs),finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),scope='Private local iframe diagnostics and lifecycle checks; supported XR policy feature, software rendering, inline-only registry. No immersive device acquired; no whole-API conformance claim.');(out/'report.json').write_text(json.dumps(record,indent=2)+'\n');assert record['passed'],'Iframe reporting regression failed'
