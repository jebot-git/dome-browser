#!/usr/bin/env python3
"""Require engine shutdown to finish asynchronous GStreamer initialization.

Linux test only. LD_PRELOAD delays loading the PulseAudio plugin by four seconds;
no production delay or user profile is involved. Run with python-xlib.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import time
from Xlib import X, display, protocol

signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_MEDIA_INIT_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
              {**os.environ, 'DOME_MEDIA_INIT_TEST_BUS': '1'})
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
out = a.output.resolve(); out.mkdir(parents=True, exist_ok=True)
source = Path(__file__).with_name('media-init-delay.c')
shim = out/'media-init-delay.so'
flags = shlex.split(subprocess.check_output(['pkg-config', '--cflags', '--libs', 'gstreamer-1.0'], text=True))
subprocess.run(['cc', '-shared', '-fPIC', '-Wall', '-Wextra', str(source), '-o', str(shim), *flags, '-ldl'], check=True)
record = {'binary_sha256': hashlib.sha256(a.binary.read_bytes()).hexdigest(),
          'interposer_sha256': hashlib.sha256(source.read_bytes()).hexdigest()}
app = xvfb = connection = None
try:
    with tempfile.TemporaryDirectory(prefix='dome-media-init-') as directory:
        runtime = Path(directory); runtime.chmod(0o700); marker = runtime/'media'
        rd, wr = os.pipe()
        xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(wr), '-screen', '0', '1280x900x24', '-nolisten', 'tcp'], pass_fds=(wr,), stderr=subprocess.DEVNULL)
        os.close(wr)
        with os.fdopen(rd) as stream: screen = ':'+stream.readline().strip()
        connection = display.Display(screen)
        env = {**os.environ, 'DISPLAY': screen, 'FLTK_BACKEND': 'x11', 'LIBGL_ALWAYS_SOFTWARE': '1',
               'XDG_RUNTIME_DIR': directory, 'XDG_CONFIG_HOME': directory+'/config',
               'XDG_DATA_HOME': directory+'/data', 'XDG_CACHE_HOME': directory+'/cache',
               'LD_PRELOAD': str(shim), 'DOME_TEST_MEDIA_INIT_MARKER': str(marker)}
        for key in ['WAYLAND_DISPLAY', 'WAYLAND_SOCKET']: env.pop(key, None)
        with (out/'browser.log').open('w') as log:
            app = subprocess.Popen([str(a.binary.resolve()), '--software', '--profile', directory+'/profile', 'about:blank'], env=env, stdout=log, stderr=log, start_new_session=True)
            deadline = time.monotonic()+30; windows = []
            while app.poll() is None and time.monotonic() < deadline:
                windows = [w for w in connection.screen().root.query_tree().children
                           if w.get_wm_name() and w.get_attributes().map_state == X.IsViewable]
                if windows and marker.with_suffix('.started').exists(): break
                time.sleep(.02)
            assert windows and marker.with_suffix('.started').exists(), 'No window during delayed backend initialization'
            assert not marker.with_suffix('.finished').exists(), 'Backend delay finished before close request'
            record['initialization_in_flight_at_close'] = True
            for window in windows:
                window.send_event(protocol.event.ClientMessage(window=window,
                    client_type=connection.intern_atom('WM_PROTOCOLS'),
                    data=(32, [connection.intern_atom('WM_DELETE_WINDOW'), X.CurrentTime, 0, 0, 0])), event_mask=0)
            connection.sync(); started = time.monotonic()
            app.wait(timeout=15)
            record.update(exit_code=app.returncode, shutdown_seconds=round(time.monotonic()-started, 3),
                          initialization_finished_before_exit=marker.with_suffix('.finished').exists())
except Exception as error:
    record['harness_error'] = str(error)
finally:
    if app is not None and app.poll() is None:
        os.killpg(app.pid, signal.SIGTERM)
        try: app.wait(timeout=5)
        except subprocess.TimeoutExpired: os.killpg(app.pid, signal.SIGKILL); app.wait()
    if connection is not None: connection.close()
    if xvfb is not None: xvfb.terminate(); xvfb.wait(timeout=5)
    (out/'report.json').write_text(json.dumps(record, indent=2)+'\n')
    print(json.dumps(record), flush=True)
assert record.get('exit_code') == 0 and record.get('initialization_finished_before_exit') and not record.get('harness_error'), record
