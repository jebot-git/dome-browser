#!/usr/bin/env python3
"""Validate capability-gated reset queries and loss-aware private EGL cleanup.

No GPU reset is requested. Display invalidation affects only a probe subprocess's
private offscreen EGL connection, never a desktop/display-manager session.
"""
import argparse,datetime,hashlib,json,os,re,shutil,subprocess,tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[3]
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--cargo',default=os.environ.get('CARGO','cargo'));a=p.parse_args()
 out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
 vendor=ROOT/'third_party/surfman'
 paths=[ROOT/'Cargo.toml',ROOT/'Cargo.lock',Path(__file__).resolve(),*(ROOT/'ports/dome-fltk/examples'/name for name in ['graphics-reset-probe.rs','graphics-retirement-probe.rs'])]
 paths+=sorted(p for p in vendor.rglob('*')if p.is_file())
 manifest={str(p.relative_to(ROOT)):sha(p)for p in paths};(out/'source-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
 env={**os.environ,'CARGO_BUILD_JOBS':'1','CARGO_INCREMENTAL':'0'};env.pop('CARGO_TARGET_DIR',None)
 with tempfile.TemporaryDirectory(prefix='dome-reset-retirement-')as temporary:
  h=Path(temporary);toml=f'''[package]
name = "dome-reset-retirement-validation"
version = "0.1.0"
edition = "2021"
[workspace]
[dependencies]
surfman = {{ path = {json.dumps(str(vendor))} }}
glow = "=0.17.0"
euclid = "0.22"
serde_json = "1"
[[test]]
name = "reset-retirement-policy"
path = "policy.rs"
'''
  for name in ['graphics-reset-probe','graphics-retirement-probe']:toml+=f'[[bin]]\nname = "{name}"\npath = {json.dumps(str(ROOT/"ports/dome-fltk/examples"/(name+".rs")))}\n'
  (h/'Cargo.toml').write_text(toml)
  source='pub use surfman::{ContextAttributeFlags, Error, GLApi};\npub type Gl = glow::Context;\n'
  for name,relative in [('robustness','src/base/egl/robustness.rs'),('graphics_reset','src/graphics_reset.rs'),('retirement','src/retirement.rs')]:source+=f'#[allow(dead_code)]\n#[path = {json.dumps(str(vendor/relative))}]\nmod {name};\n'
  (h/'policy.rs').write_text(source);shutil.copyfile(ROOT/'Cargo.lock',h/'Cargo.lock')
  def cargo(args,name):
   with(out/name).open('w')as log:subprocess.run([a.cargo,*args,'--manifest-path',str(h/'Cargo.toml')],check=True,env=env,stdout=log,stderr=subprocess.STDOUT)
  cargo(['metadata','--offline','--format-version','1'],'resolution.log');shutil.copyfile(h/'Cargo.lock',out/'harness-Cargo.lock')
  cargo(['test','--release','--locked','--offline','--test','reset-retirement-policy'],'unit-tests.log')
  match=re.search(r'test result: ok\. (\d+) passed; 0 failed',(out/'unit-tests.log').read_text());assert match and int(match[1])==16,'Policy test coverage changed'
  cargo(['build','--release','--locked','--offline','--bins'],'build.log')
  reports={};binaries={}
  for binary_name in ['graphics-reset-probe','graphics-retirement-probe']:
   binary=out/binary_name;shutil.copy2(h/'target/release'/binary_name,binary);binaries[binary_name]=sha(binary)
   for renderer in ['native','software']:
    name=binary_name+'-'+renderer;runtime=env.copy();runtime.pop('LIBGL_ALWAYS_SOFTWARE',None);command=[str(binary)]
    if renderer=='software':runtime['LIBGL_ALWAYS_SOFTWARE']='1';command.append('--software')
    with(out/(name+'.jsonl')).open('w')as stdout,(out/(name+'.stderr')).open('w')as stderr:subprocess.run(command,check=True,env=runtime,stdout=stdout,stderr=stderr)
    rows=[json.loads(line)for line in(out/(name+'.jsonl')).read_text().splitlines()];expected=5 if binary_name=='graphics-retirement-probe' else 4;assert len(rows)==expected and len({r['mode']for r in rows})==expected
    for row in rows:
     assert row['renderer']
     if binary_name=='graphics-retirement-probe':assert row['passed']and row['gpu_reset_requested']is False
     else:
      checks=['actual_strategy_verified','actual_robust_access_verified','descriptor_roundtrip','noncurrent_descriptor_verified','borrowed_native_descriptor_verified','native_import_binding_restored','reset_query_verified','shared_context_verified','incompatible_share_rejected','render_readback_verified']
      assert all(row.get(check)is True for check in checks)
    reports[name]=rows
 assert manifest=={str(p.relative_to(ROOT)):sha(p)for p in paths},'Source changed during validation'
 result={'verified_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'passed':True,'policy_unit_tests':16,'creation_query_configurations':8,'retirement_configurations':10,'binaries':binaries,'reports':reports,'hardware_reset_requested':False,'desktop_sessions_restarted':False,'browser_driver_recovery_enabled':False,'scope':'Real healthy reset queries and private context/display retirement; not hardware reset or whole-browser recovery'}
 (out/'report.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps({k:v for k,v in result.items()if k!='reports'},indent=2))
if __name__=='__main__':main()
