#!/usr/bin/env python3
"""Test stylesheet font-face replacement using bundled, licensed DejaVu fonts."""
import argparse
import base64
from pathlib import Path
import subprocess
import sys

source=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,default=source/'target/release/dome-browser')
p.add_argument('--reference-browser',type=Path)
p.add_argument('--check-csp',action='store_true',help='Also check restrictive policy changes with an already-loaded font URL')
p.add_argument('--output',type=Path,required=True)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True)
fonts=source/'components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf'
def data_url(name):return 'data:font/ttf;base64,'+base64.b64encode((fonts/name).read_bytes()).decode()
regular=data_url('DejaVuSans.ttf');bold=data_url('DejaVuSans-Bold.ttf')
faces=''.join('@font-face{font-family:ReplacementFamily;src:url('+url+');font-weight:'+str(weight)+';font-display:swap}'for weight,url in [(400,regular),(700,bold)])
fixture=out/'font-replacement.html';fixture.write_text(Path(__file__).with_suffix('.html').read_text().replace('__FONT_FACES__',faces).replace('__BOLD_FONT__',bold).replace('__CHECK_CSP__',str(a.check_csp).lower()))
command=[sys.executable,str(Path(__file__).with_name('cache-api.py')),'--binary',str(a.binary.resolve()),'--fixture',str(fixture),'--stages','write','--capture-frame','--output',str(out/'run')]
if a.reference_browser:command+=['--reference-browser',str(a.reference_browser.resolve())]
result=subprocess.run(command)
report=out/'run/report.json'
if report.exists():(out/'report.json').write_text(report.read_text())
assert result.returncode==0,'Font replacement fixture failed; see run/report.json'
