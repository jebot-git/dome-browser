#!/usr/bin/env python3
"""Verify Dome's vendored Stylo snapshot and explicitly recorded local patch."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = root / 'third_party/stylo'
manifest = json.loads((source / 'UPSTREAM_FILES.json').read_text())
with tempfile.TemporaryDirectory(prefix='dome-stylo-verify-') as directory:
    copy = Path(directory)
    for name in manifest:
        target = copy / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source / name, target)
    subprocess.run(['git', 'apply', '--reverse', str(source / 'dome.patch')],
                   cwd=copy, check=True)
    mismatches = [name for name, digest in manifest.items()
                  if hashlib.sha256((copy / name).read_bytes()).hexdigest() != digest]
    assert not mismatches, f'Unexpected changes to pinned Stylo source: {mismatches}'
print(f'Verified {len(manifest)} upstream files and the recorded Dome patch.')
