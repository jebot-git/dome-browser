#!/usr/bin/env python3
"""Build the pinned Servo source port with the local DomeD FLTK toolkit."""
import argparse
import os
import signal
from pathlib import Path
import subprocess

signal.pthread_sigmask(signal.SIG_SETMASK, [])
port = Path(__file__).resolve().parent
source = port.parents[1]
suite = source.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--jobs', type=int, default=1)
parser.add_argument('--release', action='store_true')
parser.add_argument('--test', action='store_true', help='Run the port unit tests')
args = parser.parse_args()
env = dict(os.environ)
toolchain = suite.parent / 'dome-compositor/.local-deps/toolchain/rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin'
if env.get('SERVO_RUST_BIN'):
    toolchain = Path(env['SERVO_RUST_BIN'])
if toolchain.is_dir():
    env['PATH'] = str(toolchain) + os.pathsep + env.get('PATH', '')
env.setdefault('DOME_FLTK_PREFIX', str(suite / '_toolkit'))
env.setdefault('LIBCLANG_PATH', '/usr/lib/llvm-21/lib')
resource = suite / '_deps/servo-clang/usr/lib/llvm-21/lib/clang/21'
if resource.is_dir():
    env.setdefault('BINDGEN_EXTRA_CLANG_ARGS', '-resource-dir=' + str(resource))
env.update(CARGO_BUILD_JOBS=str(max(1, args.jobs)), CARGO_PROFILE_DEV_DEBUG='0', CARGO_INCREMENTAL='0')
command = ['cargo', 'test' if args.test else 'build', '--locked', '-p', 'dome-browser']
if args.release:
    command.append('--release')
subprocess.run(command, cwd=source, env=env, check=True)
print(source / 'target' / ('release' if args.release else 'debug') / 'dome-browser')
