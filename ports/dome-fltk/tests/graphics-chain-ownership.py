#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
'Validate swap-chain ownership and retry state with an in-memory backend.'
from pathlib import Path
import argparse,datetime,hashlib,json,os,re,shutil,subprocess,tempfile
W=Path(__file__).resolve().parents[3]
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--cargo',default=os.environ.get('CARGO','cargo'));p.add_argument('--output',type=Path,required=True);a=p.parse_args();out=a.output.resolve();out.mkdir(exist_ok=False,parents=True)
paths=[Path(__file__).resolve(),W/'Cargo.toml',W/'Cargo.lock']
for name in ['surfman','glow']:paths+=sorted(p for p in (W/'third_party'/name).rglob('*')if p.is_file())
sources={str(p.relative_to(W)):sha(p)for p in paths};(out/'source-manifest.json').write_text(json.dumps(sources,indent=2)+'\n')
report={'scope':'Production swap-chain ownership with in-memory backend failures; no display or GPU calls','base_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=W,text=True).strip(),'hardware_reset_requested':False,'desktop_sessions_restarted':False}
env={**os.environ,'CARGO_BUILD_JOBS':'1','CARGO_INCREMENTAL':'0'};env.pop('CARGO_TARGET_DIR',None)
for key in ['DISPLAY','WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(key,None)
with tempfile.TemporaryDirectory(prefix='dome-chain-ownership-')as temporary:
 h=Path(temporary);vendor=W/'third_party/surfman';toml=(vendor/'Cargo.toml').read_text()
 toml=re.sub(r'(?ms)^\[dev-dependencies[^\]]*\]\n.*?(?=^\[|\Z)','',toml)
 toml=re.sub(r'(?ms)^\[dependencies\.osmesa-sys\]\n.*?(?=^\[|\Z)','',toml)
 toml=re.sub(r'(?m)^(path|readme) = "([^"]+)"$',lambda m:m[1]+' = '+json.dumps(str(vendor/m[2])),toml)
 toml=toml.replace('build = "build.rs"','build = '+json.dumps(str(vendor/'build.rs')))
 toml+='\n[workspace]\n[patch.crates-io]\nglow = { path = '+json.dumps(str(W/'third_party/glow'))+' }\n'
 (h/'Cargo.toml').write_text(toml);shutil.copyfile(W/'Cargo.lock',h/'Cargo.lock')
 with (out/'resolution.log').open('w')as log:r=subprocess.run([a.cargo,'metadata','--manifest-path',str(h/'Cargo.toml'),'--offline','--format-version','1'],env=env,stdout=log,stderr=subprocess.STDOUT)
 assert r.returncode==0,'Temporary test package resolution failed'
 shutil.copyfile(h/'Cargo.lock',out/'Cargo.lock')
 command=[a.cargo,'test','--manifest-path',str(h/'Cargo.toml'),'--release','--locked','--offline','--lib','chains::ownership_tests::','--','--test-threads=1','--nocapture'];report['command']=command
 with (out/'tests.log').open('w')as log:r=subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT)
 report['exit_code']=r.returncode
 text=(out/'tests.log').read_text();m=re.search(r'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored',text)
 report.update(compilation_completed=m is not None,passed=r.returncode==0)
 if m:report.update(tests_passed=int(m[1]),tests_failed=int(m[2]),tests_ignored=int(m[3]))
 report['source_freeze_verified']=all(sha(W/p)==h for p,h in sources.items());assert report['source_freeze_verified']
 report['finished_utc']=datetime.datetime.now(datetime.timezone.utc).isoformat();(out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
 print(json.dumps({k:report[k]for k in ['passed','exit_code','compilation_completed','tests_passed','tests_failed','source_freeze_verified']if k in report},indent=2))
 raise SystemExit(r.returncode)
