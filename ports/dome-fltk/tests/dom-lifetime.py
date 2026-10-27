#!/usr/bin/env python3
"""Reduce DOM allocation/replacement lifetime independently of filtering or frame loops."""
import argparse
from pathlib import Path
import subprocess
import sys
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=Path,required=True)
p.add_argument('--output',type=Path,required=True)
a=p.parse_args()
command=[sys.executable,str(Path(__file__).with_name('cache-api.py')),'--binary',str(a.binary.resolve()),'--fixture',str(Path(__file__).with_suffix('.html')),'--stages','write','--disable-adblock','--timeout','180','--output',str(a.output.resolve())]
raise SystemExit(subprocess.run(command).returncode)
