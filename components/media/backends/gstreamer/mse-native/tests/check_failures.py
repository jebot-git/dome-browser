#!/usr/bin/env python3
"""Test native parser failures and active/idle removal under fatal warnings."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repeats', type=int, default=3)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error('--repeats must be positive')
    package = Path(__file__).resolve().parents[1]
    stage = args.stage.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    patch_hash = digest(package / 'dome-mse.patch')
    info = json.loads((stage / 'backend-info.json').read_text())
    assert info['patch_sha256'] == patch_hash, 'Stage uses another patch'
    library = stage / 'lib/libgstmse-1.0.so.0.2807.0'
    assert digest(library) == info['library_sha256']
    env = dict(os.environ)
    env['PKG_CONFIG_PATH'] = str(stage / 'lib/pkgconfig') + os.pathsep + env.get('PKG_CONFIG_PATH', '')
    advertised = subprocess.check_output(
        ['pkg-config', '--variable=dome_mse_patch_sha256', 'gstreamer-mse-1.0'],
        env=env, text=True).strip()
    assert advertised == patch_hash, 'pkg-config selected another MSE library'
    flags = shlex.split(subprocess.check_output(
        ['pkg-config', '--cflags', '--libs', 'gstreamer-mse-1.0'], env=env, text=True))
    binary = output / 'failure-lifecycle'
    source = package / 'tests/failure-lifecycle.c'
    subprocess.run(['cc', '-O3', '-march=native', '-DGST_USE_UNSTABLE_API',
                    str(source), '-o', str(binary), '-Wl,-rpath,' + str(stage / 'lib'),
                    *flags], check=True)
    fixture = output / 'video.mp4'
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi',
                    '-i', 'testsrc2=size=160x90:rate=15', '-t', '2', '-c:v', 'libx264',
                    '-profile:v', 'baseline', '-pix_fmt', 'yuv420p', '-g', '15',
                    '-bf', '0', '-movflags', 'frag_keyframe+empty_moov+default_base_moof',
                    str(fixture)], check=True)
    env.update(LD_LIBRARY_PATH=str(stage / 'lib'), G_DEBUG='fatal-warnings',
               GST_DEBUG_NO_COLOR='1')

    def run(job):
        case, repeat = job
        log_path = output / f'{case}-{repeat}.log'
        with log_path.open('w') as log:
            try:
                result = subprocess.run([str(binary), case, str(fixture)], env=env,
                                        stdout=log, stderr=subprocess.STDOUT, timeout=15)
                code = result.returncode
            except subprocess.TimeoutExpired:
                code = 124
        records = [json.loads(line) for line in log_path.read_text().splitlines()
                   if line.startswith('{')]
        return {'case': case, 'repeat': repeat, 'exit_code': code,
                'records': records, 'passed': code == 0 and len(records) == 1
                and records[0]['case'] == case and records[0]['passed']}

    jobs = [(case, repeat) for repeat in range(args.repeats)
            for case in ['malformed', 'active-remove', 'idle-remove']]
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(run, jobs))
    assert digest(library) == info['library_sha256'], 'Library changed during testing'
    report = {'scope': 'native backend only; not browser MSE or live X validation',
              'library_sha256': info['library_sha256'], 'patch_sha256': patch_hash,
              'probe_sha256': digest(source), 'runner_sha256': digest(Path(__file__)),
              'fixture_sha256': digest(fixture), 'fatal_warnings': True,
              'results': results, 'passed': all(r['passed'] for r in results)}
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed': report['passed'], 'jobs': len(jobs)}))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
