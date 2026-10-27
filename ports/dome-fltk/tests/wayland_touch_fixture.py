"""Isolated nested compositor input fixture; never attaches to the desktop."""
import json, os, shlex, subprocess, time
from pathlib import Path
from Xlib import display
from private_wayland import PrivateWayland


class TouchWayland:
    def __init__(self, binary, runtime, out):
        self.out = out
        self.token = 0
        self.closed = False
        self.wayland = self.connection = self.xvfb = None
        self.fd = None
        fifo = runtime / 'touch-input'
        os.mkfifo(fifo, 0o600)
        self.fd = os.open(fifo, os.O_RDWR | os.O_NONBLOCK)
        wrapper = runtime / 'touch-compositor'
        wrapper.write_text('#!/bin/sh\nexec ' + shlex.quote(str(binary)) +
                           ' "$@" --touch-fixture < ' + shlex.quote(str(fifo)) + '\n')
        wrapper.chmod(0o700)
        self.old_compositor = os.environ.get('DOME_COMPOSITOR')
        os.environ['DOME_COMPOSITOR'] = str(wrapper)
        try:
            read, write = os.pipe()
            self.xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(write), '-screen', '0',
                                         '1280x900x24', '-nolisten', 'tcp'], pass_fds=[write],
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            os.close(write)
            with os.fdopen(read) as stream:
                self.screen = ':' + stream.readline().strip()
            self.connection = display.Display(self.screen)
            self.wayland = PrivateWayland(self.connection, self.screen, runtime, out)
        except BaseException:
            self.close()
            raise

    def command(self, action, slot=None, point=None, **attributes):
        self.token += 1
        value = {'token': self.token, 'action': action}
        if slot is not None:
            value['slot'] = slot
        if point is not None:
            value['point'] = point
        value.update(attributes)
        os.write(self.fd, (json.dumps(value) + '\n').encode())
        def ack():
            for line in (self.out / 'compositor.log').read_text().splitlines():
                if line.startswith('DOME_TOUCH_ACK '):
                    result = json.loads(line[len('DOME_TOUCH_ACK '):])
                    if result.get('token') == self.token:
                        assert result.get('handled'), result
                        return result
        return self.wayland.wait(ack)

    def close(self):
        if self.closed:
            return
        self.closed = True
        if self.wayland:
            self.wayland.close()
        if self.connection:
            self.connection.close()
        if self.xvfb and self.xvfb.poll() is None:
            self.xvfb.terminate()
            self.xvfb.wait(timeout=5)
        if self.fd is not None:
            os.close(self.fd)
        if self.old_compositor is None:
            os.environ.pop('DOME_COMPOSITOR', None)
        else:
            os.environ['DOME_COMPOSITOR'] = self.old_compositor
