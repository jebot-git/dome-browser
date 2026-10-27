#!/usr/bin/env python3
"""Run upstream WebXR transform and inline FOV assertions with real session setup.

The five upstream assertion bodies are unchanged. Three upstream tests normally
request a fake immersive device although their assertions only use transforms;
their setup helper is replaced with a real, sensor-free inline session. This is
an adapted subset, not a run of the complete WebXR WPT suite.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

here=Path(__file__).resolve().parent
source=here.parents[2]
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
out=args.output.resolve();out.mkdir(parents=True,exist_ok=False)
wpt=source/'tests/wpt/tests'
names=['xrRigidTransform_constructor','xrRigidTransform_inverse','xrRigidTransform_matrix','xrRigidTransform_sameObject','render_state_vertical_fov_inline']
setup="""function xr_session_promise_test(name, callback) {
 promise_test(async t => {
  const session = await navigator.xr.requestSession('inline');
  t.add_cleanup(() => session.end());
  if (name.includes('inlineVerticalFieldOfView')) {
   const canvas = document.createElement('canvas');document.body.appendChild(canvas);
   session.updateRenderState({baseLayer:new XRWebGLLayer(session,canvas.getContext('webgl'))});
  }
  return callback(session, null, t);
 }, name);
} """
hashes={}
pages=[]
for name in names:
 path=wpt/'webxr'/(name+'.https.html')
 hashes[str(path.relative_to(source))]=hashlib.sha256(path.read_bytes()).hexdigest()
 def embed(match):
  relative=match.group(1)
  if relative.endswith('webxr_util.js'):
   body=setup
  elif relative.endswith('testharnessreport.js'):
   body="""add_completion_callback((tests, status) => {
    parent.postMessage({suite:SUITE, status:status.status,
     results:tests.map(t=>({name:t.name, status:t.status, message:t.message}))}, parent.location.origin);
   });""".replace('SUITE',json.dumps(name))
  else:
   dependency=wpt/relative.lstrip('/') if relative.startswith('/') else path.parent/relative
   hashes[str(dependency.relative_to(source))]=hashlib.sha256(dependency.read_bytes()).hexdigest()
   body=dependency.read_text()
  return '<script>'+body.replace('</script','<\\/script')+'</script>'
 page=re.sub(r'<script src="([^"]+)"></script>',embed,path.read_text())
 assert 'script src=' not in page
 pages.append(page)
payload=json.dumps(pages).replace('<','\\u003c')
parent="""<!doctype html><meta charset=utf-8><title>Upstream WebXR transform assertions</title>
<script>
const results = new Map();
addEventListener('message', event => {
 if (event.origin !== location.origin || !event.data.suite) return;
 results.set(event.data.suite, event.data);
 if (results.size === SUITE_COUNT) {
  const checks = [...results.values()].flatMap(s => [
   {name:s.suite+' harness completed', passed:s.status===0},
   ...s.results.map(t=>({name:t.name, passed:t.status===0, error:t.message}))]);
  fetch('/report', {method:'POST', body:JSON.stringify({checks, suites:[...results.values()]})});
 }
});
addEventListener('load', () => { for (const page of PAGES) {
 const frame=document.createElement('iframe');frame.srcdoc=page;document.body.appendChild(frame);
}});
</script>""".replace('SUITE_COUNT',str(len(names))).replace('PAGES',payload)
fixture=out/'transforms.html';fixture.write_text(parent)
(out/'sources.json').write_text(json.dumps({'upstream_sha256':hashes,'adaptation':'Replace only fake-device session setup with a real inline session, and WPT visual reporter with a result callback. Assertion bodies remain unchanged.','suites':names},indent=2)+'\n')
command=[sys.executable,str(here/'cache-api.py'),'--binary',str(args.binary.resolve()),'--output',str(out/'browser'),'--fixture',str(fixture),'--stages','write','--timeout','45']
with (out/'browser-driver.log').open('w') as log:
 result=subprocess.run(command,env={**os.environ,'DOME_WEBXR_SOCKET':str(out/'no-native-xr.sock')},stdout=log,stderr=log)
report_path=out/'browser/report.json'
report=json.loads(report_path.read_text()) if report_path.exists() else None
runs=report.get('runs',[]) if report else []
checks=runs[0]['checks'] if len(runs)==1 else []
passed=result.returncode==0 and len(checks)==2*len(names) and all(c['passed'] for c in checks)
(out/'report.json').write_text(json.dumps({'binary_sha256':hashlib.sha256(args.binary.read_bytes()).hexdigest(),'passed':passed,'exit_code':result.returncode,'checks':checks,'upstream_source_manifest':'sources.json','fixture_sha256':hashlib.sha256(fixture.read_bytes()).hexdigest(),'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'browser_driver_sha256':hashlib.sha256((here/'cache-api.py').read_bytes()).hexdigest(),'scope':'Four adapted upstream WebXR transform suites and one inline FOV suite, unchanged assertion bodies; real sensor-free inline setup in a private X11 session. No fake immersive device, physical hardware or full-WPT claim.'},indent=2)+'\n')
for check in checks:print(('PASS ' if check['passed'] else 'FAIL ')+check['name'],flush=True)
if not passed:raise SystemExit('Adapted WebXR transform assertions failed; inspect '+str(out))
