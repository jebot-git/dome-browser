#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
'Validate native/software offscreen swap-chain pixels and GL state on Linux.'
from pathlib import Path
import argparse,datetime,hashlib,json,os,shutil,subprocess,tempfile
W=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--cargo',default=os.environ.get('CARGO','cargo'));p.add_argument('--output',type=Path,required=True);a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
paths=[Path(__file__).resolve(),W/'Cargo.toml',W/'Cargo.lock',W/'ports/dome-fltk/examples/graphics-chain-recovery-probe.rs']
for name in ['surfman','glow']:paths+=sorted(p for p in (W/'third_party'/name).rglob('*')if p.is_file())
manifest={str(p.relative_to(W)):sha(p)for p in paths};(out/'source-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
env={**os.environ,'CARGO_BUILD_JOBS':'1','CARGO_INCREMENTAL':'0'};env.pop('CARGO_TARGET_DIR',None)
with tempfile.TemporaryDirectory(prefix='dome-native-chain-')as temporary:
 h=Path(temporary)
 toml='\n'.join(['[package]','name="dome-native-chain-probe"','version="0.1.0"','edition="2024"','[workspace]','[[bin]]','name="chain-probe"','path='+json.dumps(str(W/'ports/dome-fltk/examples/graphics-chain-recovery-probe.rs')),'[dependencies]','euclid="0.22"','glow="0.17.0"','serde_json="1"','surfman={path='+json.dumps(str(W/'third_party/surfman'))+'}','[patch.crates-io]','glow={path='+json.dumps(str(W/'third_party/glow'))+'}','[profile.release]','opt-level=3','codegen-units=1','lto="thin"',''])
 (h/'Cargo.toml').write_text(toml);shutil.copyfile(W/'Cargo.lock',h/'Cargo.lock')
 for command,name in [(['metadata','--offline','--format-version','1'],'resolution.log'),(['build','--release','--locked','--offline'],'build.log')]:
  with (out/name).open('w')as log:r=subprocess.run([a.cargo,command[0],'--manifest-path',str(h/'Cargo.toml'),*command[1:]],env=env,stdout=log,stderr=subprocess.STDOUT)
  assert r.returncode==0,('Native probe setup failed',name,r.returncode)
 shutil.copyfile(h/'Cargo.lock',out/'Cargo.lock');binary=out/'chain-probe';shutil.copy2(h/'target/release/chain-probe',binary);results={}
 for mode in ['native','software']:
  runtime=env.copy()
  for key in ['DISPLAY','WAYLAND_DISPLAY','WAYLAND_SOCKET','LIBGL_ALWAYS_SOFTWARE']:runtime.pop(key,None)
  args=[]
  if mode=='software':runtime['LIBGL_ALWAYS_SOFTWARE']='1';args=['--software']
  r=subprocess.run([str(binary),*args],env=runtime,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
  (out/(mode+'.stdout')).write_text(r.stdout);(out/(mode+'.stderr')).write_text(r.stderr)
  assert r.returncode==0,(mode,r.returncode)
  d=json.loads(r.stdout);assert d['passed'] and len(d['checks'])==12
  software=any(s in d['renderer'].lower()for s in ['llvmpipe','softpipe','software rasterizer']);assert software==(mode=='software')
  results[mode]=d
 assert all(sha(W/p)==h for p,h in manifest.items()),'Probe source inputs changed during validation'
 report={'verified_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'passed':True,'binary_sha256':sha(binary),'results':results,'source_freeze_verified':True,'hardware_reset_requested':False,'desktop_sessions_restarted':False,'scope':'Healthy native/software offscreen EGL state, attachment and pixel checks; controlled failures remain separate in-memory unit tests'}
 (out/'report.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps({mode:{'renderer':d['renderer'],'checks':len(d['checks']),'passed':d['passed']}for mode,d in results.items()},indent=2))
