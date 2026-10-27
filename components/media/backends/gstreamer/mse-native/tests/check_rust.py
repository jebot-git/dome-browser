#!/usr/bin/env python3
"""Build and exercise Rust sessions and Servo's actual player under leak tracing."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repeats', type=int, default=3)
    parser.add_argument('--debugger', type=Path, help='Optional gdb executable for slow-process stacks')
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error('--repeats must be positive')
    package = Path(__file__).resolve().parents[1]
    backend = package.parent
    repo = package.parents[4]
    stage = args.stage.resolve()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    info = json.loads((stage / 'backend-info.json').read_text())
    assert digest(package / 'dome-mse.patch') == info['patch_sha256'], 'Stale native stage'
    library = stage / 'lib/libgstmse-1.0.so.0.2807.0'
    assert digest(library) == info['library_sha256']
    names = ['mse_session', 'mse_player', 'player_network']
    sources = [backend / name for name in ['mse.rs', 'player.rs', 'build.rs', 'Cargo.toml']]
    sources += [backend / 'tests' / (name + '.rs') for name in names]
    sources += [repo / 'components/media/player/lib.rs', Path(__file__).resolve()]
    hashes = {str(path.relative_to(repo)): digest(path) for path in sources}
    env = dict(os.environ)
    env['PKG_CONFIG_PATH'] = str(stage / 'lib/pkgconfig') + os.pathsep + env.get('PKG_CONFIG_PATH', '')
    command = ['cargo', 'test', '-p', 'servo-media-gstreamer', '--features', 'native-mse']
    for name in names:
        command += ['--test', name]
    command += ['--no-run', '--message-format=json']
    with (out / 'build.jsonl').open('w') as stdout, (out / 'build.log').open('w') as stderr:
        result = subprocess.run(command, cwd=repo, env=env, stdout=stdout, stderr=stderr)
    if result.returncode:
        raise RuntimeError('Rust test build failed; inspect build.log')
    binaries = {}
    for line in (out / 'build.jsonl').read_text().splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get('reason') == 'compiler-artifact' and record.get('executable'):
            name = record['target']['name']
            if name in names:
                binaries[name] = Path(record['executable'])
    assert set(binaries) == set(names), 'Missing Rust test executable'
    fixture = out / 'video.mp4'
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi',
                    '-i', 'testsrc2=size=160x90:rate=15', '-t', '2', '-c:v', 'libx264',
                    '-profile:v', 'baseline', '-pix_fmt', 'yuv420p', '-g', '15',
                    '-bf', '0', '-movflags', 'frag_keyframe+empty_moov+default_base_moof',
                    str(fixture)], check=True)
    types = {'GstSample', 'ServoSrc', 'GstPlay', 'GstMediaSource', 'GstSourceBuffer'}
    env.update(LD_LIBRARY_PATH=str(stage / 'lib'), DOME_MSE_TEST_VIDEO=str(fixture),
               G_DEBUG='fatal-warnings', GST_TRACERS='leaks(filters="' + ','.join(sorted(types)) + '")',
               GST_DEBUG='GST_TRACER:7', GST_DEBUG_NO_COLOR='1')

    def run(job):
        name, repeat = job
        log_path = out / f'{name}-{repeat}.log'
        with log_path.open('w') as log:
            process = subprocess.Popen([str(binaries[name]), '--test-threads=1', '--nocapture'],
                                       env=env, stdout=log, stderr=subprocess.STDOUT)
            try:
                code = process.wait(timeout=12)
            except subprocess.TimeoutExpired:
                if args.debugger and process.poll() is None:
                    with (out / f'{name}-{repeat}-stack.log').open('w') as stack:
                        try:
                            subprocess.run([str(args.debugger.resolve()), '-q', '-batch',
                                            '-ex', 'set debuginfod enabled off',
                                            '-ex', 'thread apply all bt 35', '-p', str(process.pid)],
                                           stdout=stack, stderr=subprocess.STDOUT, timeout=8)
                        except subprocess.TimeoutExpired:
                            pass
                try:
                    code = process.wait(timeout=25)
                except subprocess.TimeoutExpired:
                    process.terminate()
                    try:
                        process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    code = 124
        log = log_path.read_text()
        live = []
        for line in log.splitlines():
            if 'object-alive,' in line:
                match = re.search(r'type-name=\(string\)(\w+)', line)
                if match and match[1] in types:
                    live.append(match[1])
        active = 'object-alive.class' in log
        return {'test': name, 'repeat': repeat, 'exit_code': code, 'live_objects': live,
                'tracer_active': active, 'passed': code == 0 and active and not live
                and 'test result: ok. 1 passed' in log}

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(run, [(name, repeat) for repeat in range(args.repeats) for name in names]))
    assert all(digest(path) == hashes[str(path.relative_to(repo))] for path in sources), 'Source changed during tests'
    assert digest(library) == info['library_sha256'], 'Library changed during tests'
    report = {'scope': 'Native Rust sessions, Servo GStreamerPlayer software frames/seek/cancel and byte-fed regression; no DOM or live X validation',
              'library_sha256': info['library_sha256'], 'patch_sha256': info['patch_sha256'],
              'source_manifest': hashes, 'binaries': {name: digest(path) for name, path in binaries.items()},
              'fixture_sha256': digest(fixture), 'fatal_warnings': True,
              'results': results, 'passed': all(r['passed'] for r in results)}
    (out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed': report['passed'], 'jobs': len(results)}))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
