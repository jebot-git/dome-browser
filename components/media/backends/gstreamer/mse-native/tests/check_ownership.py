#!/usr/bin/env python3
"""Decode three synthetic clips, fully detach, and check GstSample ownership."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    package = Path(__file__).resolve().parents[1]
    stage = args.stage.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    patch_hash = hashlib.sha256((package / 'dome-mse.patch').read_bytes()).hexdigest()
    info = json.loads((stage / 'backend-info.json').read_text())
    assert info['patch_sha256'] == patch_hash, 'Stage was built from another patch'
    library = stage / 'lib/libgstmse-1.0.so.0.2807.0'
    assert hashlib.sha256(library.read_bytes()).hexdigest() == info['library_sha256']
    env = dict(os.environ)
    env['PKG_CONFIG_PATH'] = str(stage / 'lib/pkgconfig') + os.pathsep + env.get('PKG_CONFIG_PATH', '')
    advertised = subprocess.check_output(
        ['pkg-config', '--variable=dome_mse_patch_sha256', 'gstreamer-mse-1.0'],
        env=env, text=True).strip()
    assert advertised == patch_hash, 'pkg-config selected the wrong MSE library'
    flags = shlex.split(subprocess.check_output(
        ['pkg-config', '--cflags', '--libs', 'gstreamer-mse-1.0'], env=env, text=True))
    binary = output / 'sample-ownership'
    subprocess.run(['cc', '-O3', '-march=native', '-DGST_USE_UNSTABLE_API',
                    str(package / 'tests/sample-ownership.c'), '-o', str(binary),
                    '-Wl,-rpath,' + str(stage / 'lib'), *flags], check=True)
    fixture = output / 'video.mp4'
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi',
                    '-i', 'testsrc2=size=160x90:rate=15', '-t', '2', '-c:v', 'libx264',
                    '-profile:v', 'baseline', '-pix_fmt', 'yuv420p', '-g', '15',
                    '-bf', '0', '-movflags', 'frag_keyframe+empty_moov+default_base_moof',
                    str(fixture)], check=True)
    env.update(LD_LIBRARY_PATH=str(stage / 'lib'), GST_TRACERS='leaks(filters="GstSample")',
               GST_DEBUG='GST_TRACER:7', GST_DEBUG_NO_COLOR='1')
    log_path = output / 'probe.log'
    with log_path.open('w') as log:
        result = subprocess.run([str(binary), str(fixture), '3'], env=env,
                                stdout=log, stderr=subprocess.STDOUT, timeout=25)
    log = log_path.read_text()
    playbacks = [json.loads(line) for line in log.splitlines() if line.startswith('{')]
    live = sum('object-alive,' in line and 'type-name=(string)GstSample' in line
               for line in log.splitlines())
    report = {'library_sha256': info['library_sha256'], 'patch_sha256': patch_hash,
              'exit_code': result.returncode, 'playbacks': playbacks,
              'tracer_active': 'object-alive.class' in log,
              'samples_alive_after_release': live}
    report['passed'] = (result.returncode == 0 and report['tracer_active'] and live == 0
                        and len(playbacks) == 3
                        and all(p['playback_passed'] and p['frames'] == 30 for p in playbacks))
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
