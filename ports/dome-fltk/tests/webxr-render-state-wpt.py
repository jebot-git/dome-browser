#!/usr/bin/env python3
"""Run unchanged upstream XR render-state suites with the private mock embedder."""
import argparse, hashlib, json, re, subprocess, sys
from pathlib import Path
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
source = next((p for p in Path(__file__).resolve().parents if (p/'components/script').is_dir()), Path.cwd())
here = source/'ports/dome-fltk/tests'; wpt = source/'tests/wpt/tests'
out = a.output.resolve(); out.mkdir(parents=True, exist_ok=False)
driver = out/'driver'; driver.mkdir(); hashes = {}; pages = []
names = ['exclusive_requestFrame_nolayer', 'render_state_update', 'render_state_update_inline',
         'render_state_vertical_fov_immersive', 'render_state_vertical_fov_inline']
for name in names:
    path = wpt/'webxr'/(name+'.https.html')
    hashes[str(path.relative_to(source))] = hashlib.sha256(path.read_bytes()).hexdigest()
    def script(match):
        src = match.group(1)
        if src.endswith('testharnessreport.js'):
            body = 'add_completion_callback((tests,status)=>parent.postMessage({suite:'+json.dumps(name)+',status:status.status,results:tests.map(t=>({name:t.name,status:t.status,message:t.message}))},parent.location.origin));'
        else:
            dep = wpt/src.lstrip('/') if src.startswith('/') else path.parent/src
            hashes[str(dep.relative_to(source))] = hashlib.sha256(dep.read_bytes()).hexdigest()
            body = dep.read_text()
        return '<script>'+body.replace('</script','<\\/script')+'</script>'
    html = re.sub(r'<script\s+src=["\']?([^"\' >]+)["\']?\s*>\s*</script>', script, path.read_text(), flags=re.I)
    pages.append([name, html])
html = '''<!doctype html><meta charset=utf-8><title>WebXR render-state upstream tests</title><script>
const state=window.test={ready:true,finished:false,error:null,checks:[],frames:0,suites:[]};
state.snapshot=()=>({...state,snapshot:undefined});let index=0,frame=null;const pages=PAGES;
function next(){if(index===pages.length){state.finished=true;return;}frame=document.createElement('iframe');frame.srcdoc=pages[index][1];document.body.appendChild(frame);}
addEventListener('message',e=>{if(e.source!==frame.contentWindow||e.data.suite!==pages[index][0])return;
 state.suites.push(e.data);for(const test of e.data.results)state.checks.push(test.name);
 frame.remove();index++;next();});addEventListener('DOMContentLoaded',next);
</script><body></body>'''.replace('PAGES', json.dumps(pages).replace('</','<\\/'))
(driver/'webxr-render-state-wpt-embed.html').write_text(html)
code = (here/'webxr-bounded.py').read_text().replace('Test embedder starts the bounded-space fixture', 'Test embedder completes upstream render-state suites').replace('Bounded-space test embedder shuts down cleanly', 'Upstream render-state test embedder shuts down cleanly')
(driver/'webxr-render-state-wpt-embed.py').write_text(code)
(driver/'private_wayland.py').write_bytes((here/'private_wayland.py').read_bytes())
(out/'upstream-source.json').write_text(json.dumps({'source_sha256':hashes,
    'assertion_bodies_changed':False, 'setup_helper_changed':False, 'test_api_adapter':None,
    'transport':'Loopback HTTP and sequential srcdoc aggregation; embed original resources and add completion reporting'}, indent=2)+'\n')
subprocess.run([sys.executable, str(driver/'webxr-render-state-wpt-embed.py'), str(a.binary.resolve()), str(out/'runtime')], check=True)
r = json.loads((out/'runtime/report.json').read_text()); assert r['passed']
suites = r['events']['suites']; assert len(suites) == len(names)
passed = all(s['status'] == 0 and s['results'] and all(t['status'] == 0 for t in s['results']) for s in suites)
(out/'report.json').write_text(json.dumps({'passed':passed, 'suites':suites,
    'passed_cases':sum(t['status']==0 for s in suites for t in s['results']),
    'binary_sha256':r['binary_sha256'], 'runtime_report':'runtime/report.json',
    'upstream_source':'upstream-source.json', 'full_wpt_runner':False}, indent=2)+'\n')
assert passed, 'An original render-state suite failed; inspect report.json'
