#!/usr/bin/env python3
"""Exercise secondary-view identity, lifecycle and GPU pixels in the mock embedder."""
from pathlib import Path
import argparse,json,subprocess,sys
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
here=Path(__file__).resolve().parent;out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);driver=out/'driver';driver.mkdir()
for name in ['private_wayland.py','webxr-secondary-views.html']:(driver/name).write_bytes((here/name).read_bytes())
code=(here/'webxr-bounded.py').read_text().replace('Test embedder starts the bounded-space fixture','Test embedder starts the secondary-view fixture').replace('Bounded-space test embedder shuts down cleanly','Secondary-view embedder shuts down cleanly').replace('time.monotonic()+12','time.monotonic()+45')
(driver/'webxr-secondary-views.py').write_text(code)
subprocess.run([sys.executable,str(driver/'webxr-secondary-views.py'),str(a.binary.resolve()),str(out/'runtime')],check=True)
d=json.loads((out/'runtime/report.json').read_text());(out/'report.json').write_text(json.dumps(d,indent=2)+'\n');assert d['passed']
