#!/usr/bin/env python3
"""Build and stage the pinned experimental MSE library as a regular user."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import urllib.request


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build-root', type=Path, required=True,
                        help='New directory for verified sources, build and stage')
    parser.add_argument('--source-dir', type=Path,
                        help='Use an existing upstream source directory without downloading')
    parser.add_argument('--jobs', type=int, default=2)
    args = parser.parse_args()
    if os.geteuid() == 0:
        parser.error('Build as a regular user; this tool does not install system files')
    if args.jobs < 1:
        parser.error('--jobs must be positive')

    package = Path(__file__).resolve().parent
    manifest = json.loads((package / 'upstream.json').read_text())
    expected = json.loads((package / 'patched-sources.json').read_text())
    build_root = args.build_root.resolve()
    build_root.mkdir(parents=True, exist_ok=False)
    original = build_root / 'upstream'
    original.mkdir()

    def acquire(item):
        name, digest = item
        if not re.fullmatch(r'[a-z][a-z0-9.-]*', name):
            raise ValueError('Invalid pinned filename: ' + name)
        path = original / name
        if args.source_dir:
            shutil.copyfile(args.source_dir.resolve() / name, path)
        else:
            with urllib.request.urlopen(manifest['base_url'] + name, timeout=30) as response:
                path.write_bytes(response.read())
        if sha256(path) != digest:
            raise ValueError('Upstream source hash mismatch: ' + name)

    with concurrent.futures.ThreadPoolExecutor(max_workers=min(args.jobs, 4)) as pool:
        list(pool.map(acquire, manifest['files'].items()))

    source = build_root / 'source'
    shutil.copytree(original, source)
    patch = package / 'dome-mse.patch'
    subprocess.run(['patch', '--batch', '-p1'], input=patch.read_bytes(), cwd=source, check=True)
    for name, digest in expected.items():
        if sha256(source / name) != digest:
            raise ValueError('Patched source differs from the validated input: ' + name)
    shutil.copyfile(package / 'meson.build.in', source / 'meson.build')

    namespace = source / 'gst/mse'
    namespace.mkdir(parents=True)
    for header in source.glob('*.h'):
        (namespace / header.name).symlink_to('../../' + header.name)
    for name in ['mse-enumtypes.h', 'mse-enumtypes-private.h']:
        (namespace / name).symlink_to('../../build/' + name)
    subprocess.run(['meson', 'setup', 'build'], cwd=source, check=True)
    subprocess.run(['meson', 'compile', '-C', 'build', '-j', str(args.jobs)],
                   cwd=source, check=True)

    stage = build_root / 'stage'
    library_dir = stage / 'lib'
    library_dir.mkdir(parents=True)
    name = 'libgstmse-1.0.so.0.2807.0'
    library = library_dir / name
    shutil.copyfile(source / 'build' / name, library)
    (library_dir / 'libgstmse-1.0.so.0').symlink_to(name)
    (library_dir / 'libgstmse-1.0.so').symlink_to(name)
    includes = stage / 'include/gstreamer-1.0/gst/mse'
    includes.mkdir(parents=True)
    public_headers = ['gstmediasource.h', 'gstsourcebuffer.h', 'gstsourcebufferlist.h',
                      'gstmsesrc.h', 'mse.h', 'mse-prelude.h']
    for header in public_headers:
        shutil.copyfile(source / header, includes / header)
    shutil.copyfile(source / 'build/mse-enumtypes.h', includes / 'mse-enumtypes.h')
    licenses = stage / 'share/licenses/dome-gstreamer-mse'
    licenses.mkdir(parents=True)
    shutil.copyfile(package / 'COPYING', licenses / 'COPYING')
    pkgconfig = library_dir / 'pkgconfig'
    pkgconfig.mkdir()
    patch_hash = sha256(patch)
    (pkgconfig / 'gstreamer-mse-1.0.pc').write_text(
        f'prefix={stage}\nlibdir=${{prefix}}/lib\nincludedir=${{prefix}}/include\n'
        f'dome_mse_patch_sha256={patch_hash}\n'
        'Name: Dome experimental GStreamer MSE\n'
        'Description: Private pinned native MSE backend for Dome validation\n'
        'Version: 1.28.7\nRequires: gstreamer-1.0\n'
        'Requires.private: gstreamer-app-1.0 gstreamer-base-1.0\n'
        'Libs: -L${libdir} -lgstmse-1.0\n'
        'Cflags: -I${includedir}/gstreamer-1.0\n')
    info = {'upstream_tag': manifest['tag'], 'patch_sha256': patch_hash,
            'upstream_manifest_sha256': sha256(package / 'upstream.json'),
            'patched_manifest_sha256': sha256(package / 'patched-sources.json'),
            'library_sha256': sha256(library), 'stage': str(stage),
            'machine': platform.machine(), 'build_type': 'release',
            'cpu_tuning': '-march=native', 'system_installation': False}
    (stage / 'backend-info.json').write_text(json.dumps(info, indent=2) + '\n')
    print(json.dumps(info))


if __name__ == '__main__':
    main()
