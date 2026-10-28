#!/usr/bin/env python3
"""Validate fallible native metadata loading and private EGL initialization rollback.

The native tests use offscreen connections in their own subprocesses. Display
invalidation stays within those processes; no GPU or desktop reset is requested.
"""
import argparse,datetime,hashlib,json,os,re,shutil,subprocess,tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[3]
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--cargo',default=os.environ.get('CARGO','cargo'));a=p.parse_args()
 out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
 paths=[ROOT/'Cargo.toml',ROOT/'Cargo.lock',Path(__file__).resolve()]
 for name in ['glow','surfman']:paths+=sorted(p for p in(ROOT/'third_party'/name).rglob('*')if p.is_file())
 manifest={str(p.relative_to(ROOT)):sha(p)for p in paths};(out/'source-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
 env={**os.environ,'CARGO_BUILD_JOBS':'1','CARGO_INCREMENTAL':'0'};env.pop('CARGO_TARGET_DIR',None)
 with tempfile.TemporaryDirectory(prefix='dome-native-initialization-')as temporary:
  folder=Path(temporary);results={}
  for name in ['glow','surfman']:
   vendor=ROOT/'third_party'/name;h=folder/name;h.mkdir()
   # Compile the production library and its private unit modules from the
   # original normalized package manifest, with source paths made absolute.
   toml=(vendor/'Cargo.toml').read_text()
   if name=='surfman':
    # These dependencies are used by examples, not the library unit modules.
    toml=re.sub(r'(?ms)^\[dev-dependencies[^\]]*\]\n.*?(?=^\[|\Z)','',toml)
    # The upstream manifest retains an unused optional OSMesa dependency;
    # no library module references it and these tests use EGL exclusively.
    toml=re.sub(r'(?ms)^\[dependencies\.osmesa-sys\]\n.*?(?=^\[|\Z)','',toml)
   toml=re.sub(r'(?m)^(path|readme) = "([^"]+)"$',lambda m:m[1]+' = '+json.dumps(str(vendor/m[2])),toml)
   toml=toml.replace('build = "build.rs"','build = '+json.dumps(str(vendor/'build.rs')))
   toml+='\n[workspace]\n[patch.crates-io]\nglow = { path = '+json.dumps(str(ROOT/'third_party/glow'))+' }\n'
   (h/'Cargo.toml').write_text(toml);shutil.copyfile(ROOT/'Cargo.lock',h/'Cargo.lock')
   def cargo(arguments,log,runtime=env):
    with(out/log).open('w')as stream:subprocess.run([a.cargo,arguments[0],'--manifest-path',str(h/'Cargo.toml'),*arguments[1:]],check=True,env=runtime,stdout=stream,stderr=subprocess.STDOUT)
   cargo(['metadata','--offline','--format-version','1'],name+'-resolution.log');shutil.copyfile(h/'Cargo.lock',out/(name+'-Cargo.lock'))
   if name=='glow':
    cargo(['test','--release','--locked','--offline','--lib'],'glow-unit-tests.log')
    log=(out/'glow-unit-tests.log').read_text();m=re.search(r'test result: ok\. (\d+) passed; 0 failed',log);assert m,'Glow unit tests did not finish'
    added=len(re.findall(r'^test native::native_loader_tests::.* \.\.\. ok$',log,re.M));assert added==26,('Native loader coverage changed',added)
    results['glow']={'passed':True,'library_unit_tests':int(m[1]),'native_loader_tests':added}
   else:
    cargo(['test','--release','--locked','--offline','--lib','recovery::tests','--','--test-threads=1'],'reset-monitor-policy.log')
    policy=re.search(r'test result: ok\. (\d+) passed; 0 failed',(out/'reset-monitor-policy.log').read_text());assert policy and int(policy[1])>=7,'Missing reset-monitor policy coverage'
    results['reset_monitor_policy']={'passed':True,'tests':int(policy[1])}
    for renderer in ['native','software']:
     runtime=env.copy();runtime.pop('LIBGL_ALWAYS_SOFTWARE',None);runtime.pop('DOME_NATIVE_INIT_SOFTWARE',None)
     for key in ['DISPLAY','WAYLAND_DISPLAY','WAYLAND_SOCKET']:runtime.pop(key,None)
     if renderer=='software':runtime.update(LIBGL_ALWAYS_SOFTWARE='1',DOME_NATIVE_INIT_SOFTWARE='1')
     cargo(['test','--release','--locked','--offline','--lib','initialization_','--','--test-threads=1','--nocapture'],renderer+'-rollback-tests.log',runtime)
     probe=(out/(renderer+'-rollback-tests.log')).read_text();m=re.search(r'test result: ok\. (\d+) passed; 0 failed',probe);assert m and int(m[1])>=12,'Missing native initialization/reset-monitor coverage'
     assert '::optional_robust_context_reports_resets_and_preserves_the_peer_binding ... 'in probe,'Missing native reset-monitor case'
     names=sorted(set(re.findall(r'DOME_INITIALIZATION_RENDERER=([^\n]+)',probe)));assert names,'Native renderer evidence missing'
     software=all(any(token in name.lower()for token in ['llvmpipe','softpipe','software'])for name in names)
     assert software==(renderer=='software'),('Unexpected renderer',renderer,names)
     results[renderer]={'passed':True,'initialization_tests':int(m[1]),'reset_monitor_tests':1,'renderers':names}
 assert manifest=={str(p.relative_to(ROOT)):sha(p)for p in paths},'Sources changed during initialization validation'
 report={'verified_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'passed':True,'results':results,'gpu_reset_requested':False,'desktop_sessions_restarted':False,'browser_driver_reset_tested':False,'manifest_scope':'Production library/test sources unchanged; example-only dev dependencies and unused optional OSMesa manifest entry omitted in temporary unit package','scope':'Production loader, reset-monitor policy and private native/software EGL rollback/healthy reset-query tests; not whole-browser or hardware-reset recovery'}
 (out/'report.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report,indent=2))
if __name__=='__main__':main()
