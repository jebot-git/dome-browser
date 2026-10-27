#!/usr/bin/env python3
"""Verify the pinned parser packages and the exact recorded contextual-creation patch."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
manifest = json.loads((root/'third_party/html-parser-upstream.json').read_text())
with tempfile.TemporaryDirectory(prefix='dome-parser-verify-') as directory:
    copy = Path(directory)
    for package, data in manifest.items():
        actual = {str(p.relative_to(root/'third_party'/package))
                  for p in (root/'third_party'/package).rglob('*') if p.is_file()}
        assert actual == set(data['files']), ('Unexpected package files', package, actual ^ set(data['files']))
        for name in data['files']:
            target = copy/package/name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(root/'third_party'/package/name, target)
    subprocess.run(['git', 'apply', '--reverse', str(root/'third_party/html-parser-dome.patch')],
                   cwd=copy, check=True)
    mismatches = [package+'/'+name for package, data in manifest.items()
                  for name, digest in data['files'].items()
                  if hashlib.sha256((copy/package/name).read_bytes()).hexdigest() != digest]
    assert not mismatches, mismatches
print('Verified', sum(len(d['files']) for d in manifest.values()), 'pinned upstream files and recorded parser patch.')
