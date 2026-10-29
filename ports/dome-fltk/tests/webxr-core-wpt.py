#!/usr/bin/env python3
"""Audit original top-level WebXR WPT cases using the isolated xr-native-input example.

The WebDriver-dependent window-state page is recorded separately. The command
returns 1 for nonpassing original assertions, while preserving every result for
specification/setup review; harness completion alone never means conformance.
"""
from pathlib import Path
import argparse
import datetime
import hashlib
import json
import os
import re
import subprocess
import sys


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path, help='optimized xr-native-input example')
    parser.add_argument('output', type=Path, help='new output directory')
    args = parser.parse_args()
    tests = Path(__file__).resolve().parent
    repo = tests.parents[2]
    wpt = repo / 'tests/wpt/tests'
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    driver = output / 'driver'
    driver.mkdir()
    binary = args.binary.resolve()
    binary_hash = digest(binary)
    sources = {}
    routes = {}
    selected = []
    excluded = []
    for page in sorted((wpt / 'webxr').glob('*.https.html')):
        name = page.name.removesuffix('.https.html')
        sources[str(page.relative_to(repo))] = digest(page)
        if name == 'xrSession_visibilityState_inline':
            excluded.append({'suite': name, 'reason': 'Requires WebDriver window-state commands; run webxr-inline.py --native-visibility separately'})
            continue

        def inline(match):
            src = match.group(1)
            if src.endswith('testharnessreport.js'):
                body = ('add_completion_callback((tests,status)=>parent.postMessage({suite:' + json.dumps(name) +
                        ',status:status.status,message:status.message,results:tests.map(t=>({name:t.name,status:t.status,message:t.message}))},parent.location.origin));')
            else:
                dependency = wpt / src.lstrip('/') if src.startswith('/') else page.parent / src
                sources[str(dependency.relative_to(repo))] = digest(dependency)
                body = dependency.read_text()
            return '<script>' + body.replace('</script', '<\\/script') + '</script>'

        body = re.sub(r'<script\s+src=["\']?([^"\' >]+)["\']?\s*>\s*</script>', inline, page.read_text(), flags=re.I)
        headers = []
        header_file = page.with_name(page.name + '.headers')
        if header_file.exists():
            sources[str(header_file.relative_to(repo))] = digest(header_file)
            for line in header_file.read_text().splitlines():
                if line.strip():
                    key, value = line.split(':', 1)
                    headers.append([key.strip(), value.strip()])
        routes['/webxr/' + page.name] = {'body': body, 'headers': headers}
        selected.append(name)
    if not selected:
        raise SystemExit('No local WebXR WPT pages found')
    (driver / 'responses.json').write_text(json.dumps(routes))
    html = '''<!doctype html><meta charset=utf-8><title>Core WebXR candidate audit</title><script>
const state=window.test={ready:true,finished:false,error:null,checks:[],frames:0,suites:[],current:null};state.snapshot=()=>({...state,snapshot:undefined});
const names=NAMES;let index=0,frame=null,order=0;
function progress(phase){fetch('/progress',{method:'POST',body:JSON.stringify({order:++order,phase,index,total:names.length,current:state.current,completed:state.suites.length})}).catch(()=>{});}
function next(){if(index===names.length){state.current=null;state.finished=true;progress('complete');return;}state.current=names[index];progress('start');frame=document.createElement('iframe');frame.src='/webxr/'+names[index]+'.https.html';document.body.appendChild(frame);}
addEventListener('message',event=>{if(event.source!==frame.contentWindow||event.origin!==location.origin||event.data.suite!==names[index])return;state.suites.push(event.data);for(const test of event.data.results)state.checks.push(test.name);progress('suite-complete');frame.remove();index++;next();});addEventListener('DOMContentLoaded',next);
</script><body></body>'''.replace('NAMES', json.dumps(selected))
    (driver / 'core-audit.html').write_text(html)
    template = tests / 'webxr-bounded.py'
    code = template.read_text().replace('deadline=time.monotonic()+12', 'deadline=time.monotonic()+1800').replace('time.sleep(.025)', 'time.sleep(.1)').replace('bounded-space', 'core-WebXR-audit')
    old_handler = """  class Handler(http.server.BaseHTTPRequestHandler):
   def do_GET(self):
    body=Path(__file__).with_suffix('.html').read_bytes();self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(body)"""
    new_handler = """  routes=json.loads(Path(__file__).with_name('responses.json').read_text());progress_order=[0];progress_lock=threading.Lock()
  class Handler(http.server.BaseHTTPRequestHandler):
   def do_GET(self):
    route=self.path.split('?',1)[0]
    if route=='/':body=Path(__file__).with_suffix('.html').read_bytes();headers=[]
    elif route in routes:body=routes[route]['body'].encode();headers=routes[route]['headers']
    else:self.send_response(404);self.end_headers();return
    self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(body)))
    for key,value in headers:self.send_header(key,value)
    self.end_headers();self.wfile.write(body)
   def do_POST(self):
    data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
    with progress_lock:
     if self.path=='/progress'and data['order']>progress_order[0]:
      progress_order[0]=data['order'];temp=out/'progress.tmp';temp.write_text(json.dumps(data));os.replace(temp,out/'progress.json')
    self.send_response(204);self.end_headers()"""
    if old_handler not in code:
        raise SystemExit('Private embedder scaffold changed; review the audit adapter')
    code = code.replace(old_handler, new_handler)
    (driver / 'core-audit.py').write_text(code)
    (driver / 'private_wayland.py').write_bytes((tests / 'private_wayland.py').read_bytes())
    for source in [Path(__file__).resolve(), template, tests / 'private_wayland.py']:
        sources[str(source.relative_to(repo))] = digest(source)
    manifest = {'source_sha256': sources, 'assertion_bodies_changed': False, 'setup_helpers_changed': False,
                'headers_preserved': True, 'vendor_reporting_adapter': True, 'transport': 'Loopback HTTP with original paths, headers and verbatim helper scripts',
                'pages_selected': len(selected), 'excluded': excluded, 'full_wpt_runner': False, 'geolocation_excluded': True}
    (output / 'source.json').write_text(json.dumps(manifest, indent=2) + '\n')
    env = {key: value for key, value in os.environ.items() if not key.startswith('XR_INPUT_')}
    env['XR_INPUT_STRICT_USER_ACTIVATION'] = '1'
    command = [sys.executable, str(driver / 'core-audit.py'), str(binary), str(output / 'runtime')]
    with (output / 'controller.log').open('w') as log:
        child = subprocess.Popen(command, cwd=repo, env=env, stdout=log, stderr=log)
        job = {'pid': child.pid, 'command': command, 'started_utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}
        (output / 'job.json').write_text(json.dumps(job, indent=2) + '\n')
        job['exit_code'] = child.wait()
    job['finished_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    (output / 'job.json').write_text(json.dumps(job, indent=2) + '\n')
    assert digest(binary) == binary_hash and all(digest(repo / name) == value for name, value in sources.items())
    runtime = json.loads((output / 'runtime/report.json').read_text())
    suites = (runtime.get('events') or {}).get('suites', [])
    cases = [{**case, 'suite': suite['suite']} for suite in suites for case in suite['results']]
    complete = job['exit_code'] == 0 and runtime.get('passed') and len(suites) == len(selected)
    nonpassing = [case for case in cases if case['status'] != 0]
    record = {'harness_completed': complete, 'all_original_assertions_passed': complete and not nonpassing,
              'suite_count': len(suites), 'suites': suites, 'passed_cases': sum(case['status'] == 0 for case in cases),
              'nonpassing_cases': nonpassing, 'nonpassing_suite_statuses': {suite['suite']: suite['status'] for suite in suites if suite['status'] != 0},
              'binary': str(binary), 'binary_sha256': binary_hash, 'source': 'source.json', 'excluded': excluded,
              'full_wpt_runner': False, 'geolocation_excluded': True}
    (output / 'report.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps({key: record[key] for key in ['harness_completed', 'all_original_assertions_passed', 'suite_count', 'passed_cases']}))
    return 0 if record['all_original_assertions_passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
