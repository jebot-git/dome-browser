#!/usr/bin/env python3
"""Exercise native smallnet loading against private local fixtures."""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import signal
import socketserver
import subprocess
import sys
import tempfile
import threading
import time

signal.pthread_sigmask(signal.SIG_SETMASK, [])
if os.environ.get('DOME_SMALLNET_TEST_BUS') != '1':
    os.execvpe('dbus-run-session', ['dbus-run-session', '--', sys.executable, *sys.argv],
              {**os.environ, 'DOME_SMALLNET_TEST_BUS': '1'})

from PIL import Image, ImageChops
from Xlib import X, XK, display, protocol
from Xlib.ext import xtest

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--extended', action='store_true')
parser.add_argument('--gopher-plus', action='store_true')
parser.add_argument('--cases', nargs='+')
parser.add_argument('--cancellation-scheme', choices=['nex', 'http'], default='nex')
parser.add_argument('--cancellation-after-load', action='store_true')
args = parser.parse_args()
root = Path(__file__).resolve().parents[3]
binary = args.binary.resolve()
driver_sha256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
out = args.output.resolve()
assert not out.exists(), 'Use a fresh validation directory'
out.mkdir(parents=True, mode=0o700)
records = []
checks = []
servers = []
probes = []
completed = []
interactions = []
cancellation_complete = not args.extended or bool(args.cases and 'navigation-cancellation' not in args.cases)
cancellation_observation = {}


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(5)
        request = self.rfile.readline(4097)
        body = b''
        if self.server.kind == 'spartan':
            fields = request.rstrip(b'\r\n').split(b' ')
            if len(fields) != 3 or not fields[2].isdigit():
                return
            length = int(fields[2])
            if length > 8 * 1024 * 1024:
                return
            body = self.rfile.read(length)
            request += body
        elif self.server.kind == 'gopher-plus' and request.endswith(b'\t1\r\n'):
            header = self.rfile.readline(32)
            if not header.startswith(b'+') or not header[1:].strip().isdigit():
                return
            length = int(header[1:])
            if length > 8 * 1024 * 1024:
                return
            body = self.rfile.read(length)
            request += header + body
        records.append((self.server.kind, request))
        if self.server.kind == 'http':
            path = request.split(b' ')[1]
            while self.rfile.readline(4097) not in [b'\r\n', b'']:
                pass
            if path == b'/slow':
                self.server.slow_started.set()
                self.connection.settimeout(20)
                try:
                    if self.connection.recv(1) == b'':
                        self.server.slow_closed.set()
                except OSError:
                    pass
                return
            html = b'<!doctype html><title>HTTP ' + path + b'</title><h1>Local HTTP fixture</h1>'
            self.wfile.write(b'HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: ' + str(len(html)).encode() + b'\r\nConnection: close\r\n\r\n' + html)
            return
        if self.server.kind == 'gopher':
            if request == b'\r\n':
                data = ('iLocal Gopher fixture\tfake\tnull\t0\r\n'
                        f'0Text item\t/docs/example\t127.0.0.1\t{self.server.server_address[1]}\t+\r\n'
                        '.\r\n').encode()
            elif request == b'/docs/example\r\n':
                data = 'Gopher text 世界\r\n..literal period\r\n.\r\n'.encode()
            elif request == b'/docs/example\t+\r\n':
                body = 'Gopher text 世界\r\n.literal period\r\n'.encode()
                data = b'+' + str(len(body)).encode() + b'\r\n' + body
            elif request == b'/search\tlocal query\r\n':
                data = b'iSearch completed\tfake\tnull\t0\r\n.\r\n'
            else:
                data = b'3Unexpected request\tfake\tnull\t0\r\n.\r\n'
        elif self.server.kind == 'gopher-plus':
            selector = request.rstrip(b'\r\n').split(b'\t')[0]
            port = self.server.server_address[1]
            attributes = (f'+INFO: 0Local plus item\t/file\t127.0.0.1\t{port}\t+\r\n'
                          '+ADMIN:\r\n Admin: Local fixture <local@example.invalid>\r\n'
                          '+VIEWS:\r\n text/plain De_DE: <20>\r\n'
                          '+ABSTRACT:\r\n Escaped <script> text\r\n').encode()
            if b'!+ASK' in request:
                body = f'+INFO: 0Form\t/form\t127.0.0.1\t{port}\t?\r\n+ASK:\r\n Ask: Name\r\n'.encode()
                data = b'+-1\r\n' + body + b'.\r\n'
            elif b'!' in request or b'$' in request:
                if selector == b'/raw':
                    data = attributes + b'.\r\n'
                else:
                    data = b'+-1\r\n' + attributes + b'.\r\n'
            elif selector == b'/dot':
                data = b'+-1\r\nDot framed text\r\n..literal period\r\n.\r\n'
            elif selector == b'/eof':
                data = b'+-2\r\nConnection framed text\r\n.\r\nText after literal dot\r\n'
            elif selector == b'/error':
                data = b'--1\r\n2 <local@example.invalid>\r\nTry again later\r\n.\r\n'
            elif selector == b'/search':
                body = b'iPlus search completed\tfake\tnull\t0\r\n'
                data = b'+' + str(len(body)).encode() + b'\r\n' + body
            else:
                body = 'Counted Gopher+ text 世界\r\n.\r\nText after literal dot\r\n'.encode()
                data = b'+' + str(len(body)).encode() + b'\r\n' + body
        elif self.server.kind == 'nex':
            if request == b'/slow\r\n':
                self.server.slow_started.set()
                self.connection.settimeout(20)
                try:
                    if self.connection.recv(1) == b'':
                        self.server.slow_closed.set()
                except OSError:
                    pass
                return
            data = b'Nex directory\n=> about.txt About\n' if request == b'/\r\n' else 'Nex text 世界\n  /\\_/\\\n'.encode()
        elif self.server.kind == 'spartan':
            path = fields[1]
            if path == b'/redirect':
                data = b'3 /document\r\n'
            elif path == b'/client-error':
                data = b'4 Local client error\r\n'
            elif path == b'/server-error':
                data = b'5 Local server error\r\n'
            elif path == b'/plain':
                data = b'2 text/plain; charset=iso-8859-1\r\ncaf\xe9\n'
            elif path == b'/data':
                data = b'2 text/plain\r\nBody received with correct byte length\n'
            elif path == b'/input-page':
                data = b'2 text/gemini\r\n' + (b'# Input received\nlocal query\n' if body else b'=: /input-page Enter query\n')
            elif path == b'/html':
                destination = f'http://127.0.0.1:{self.server.canary_port}/'
                html = f'<meta http-equiv="refresh" content="0;url={destination}"><h1>Local inert HTML fixture</h1><script>fetch("{destination}")</script>'
                data = b'2 text/html; charset=utf-8\r\n' + html.encode()
            else:
                data = ('2 text/gemini; lang=en\r\n# Local Spartan fixture\nText 世界\n\n## Section\n* List item\n> A quote\n```ASCII art\n  /\\_/\\\n```\n=> /plain Plain text\n=: /input-page Input\n').encode()
        elif self.server.kind == 'canary':
            data = b'HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n'
        else:
            data = b'Login: alice\r\nName: Local fixture\r\n'
        self.wfile.write(data)


def start_server(kind):
    server = Server(('127.0.0.1', 0), Handler)
    server.kind = kind
    server.slow_started = threading.Event()
    server.slow_closed = threading.Event()
    threading.Thread(target=server.serve_forever, daemon=True).start()
    servers.append(server)
    return server.server_address[1]


def check(value, name):
    checks.append({'name': name, 'passed': bool(value)})
    assert value, name


def answer_search(connection):
    for window in connection.screen().root.query_tree().children:
        try:
            if window.get_attributes().map_state != X.IsViewable:
                continue
            if str(window.get_wm_name() or '') != 'Dome Browser' or window.get_geometry().height >= 500:
                continue
            window.set_input_focus(X.RevertToParent, X.CurrentTime)
            connection.sync()
            type_text(connection, 'local query\n')
            return True
        except Exception:
            continue
    return False


def answer_simple_plus_form(connection):
    for window in connection.screen().root.query_tree().children:
        try:
            if window.get_attributes().map_state == X.IsViewable and str(window.get_wm_name() or '') == 'Gopher+ form':
                window.set_input_focus(X.RevertToParent, X.CurrentTime)
                connection.sync()
                type_text(connection, 'local query\n')
                return True
        except Exception:
            pass
    return False


def type_text(connection, text):
    for char in text:
        symbol = XK.XK_Return if char == '\n' else (XK.string_to_keysym(char) or ord(char))
        code = connection.keysym_to_keycode(symbol)
        shifted = symbol != XK.XK_Return and connection.keycode_to_keysym(code, 0) != symbol and connection.keycode_to_keysym(code, 1) == symbol
        shift = connection.keysym_to_keycode(XK.XK_Shift_L)
        if shifted:
            xtest.fake_input(connection, X.KeyPress, shift)
        xtest.fake_input(connection, X.KeyPress, code)
        xtest.fake_input(connection, X.KeyRelease, code)
        if shifted:
            xtest.fake_input(connection, X.KeyRelease, shift)
    connection.sync()


def browser_window(connection):
    for window in connection.screen().root.query_tree().children:
        try:
            geometry = window.get_geometry()
            if window.get_attributes().map_state == X.IsViewable and geometry.width >= 640 and geometry.height >= 420:
                return window
        except Exception:
            pass
    return None


def activate_first_link(connection, skip=0):
    window = browser_window(connection)
    if window is None:
        return False
    geometry = window.get_geometry()
    raw = window.get_image(0, 0, geometry.width, geometry.height, X.ZPixmap, 0xffffffff)
    frame = Image.frombytes('RGB', (geometry.width, geometry.height), raw.data, 'raw', 'BGRX')
    region = frame.crop((0, 140, geometry.width, min(500, geometry.height)))
    red, green, blue = region.split()
    mask = ImageChops.multiply(ImageChops.subtract(blue, red).point(lambda x: 255 if x >= 20 else 0),
                               ImageChops.subtract(blue, green).point(lambda x: 255 if x >= 20 else 0))
    mask = ImageChops.multiply(mask, blue.point(lambda x: 255 if x >= 100 else 0))
    # Subpixel text edges can look blue even on ordinary white text. Select a
    # continuous link underline instead of the bounds of every blue pixel.
    pixels = mask.tobytes()
    bounds = None
    last_row = -10
    for y in range(region.height):
        row = pixels[y * region.width:(y + 1) * region.width]
        run = re.search(b'\xff{12,}', row)
        if run:
            if y <= last_row + 2:
                last_row = y
                continue
            last_row = y
            if skip:
                skip -= 1
                continue
            bounds = (run.start(), y, run.end(), y + 1)
            break
    if bounds is None:
        return False
    left, top, right, bottom = bounds
    window.set_input_focus(X.RevertToParent, X.CurrentTime)
    xtest.fake_input(connection, X.MotionNotify, x=geometry.x + (left + right) // 2,
                     y=geometry.y + 140 + (top + bottom) // 2)
    xtest.fake_input(connection, X.ButtonPress, 1)
    xtest.fake_input(connection, X.ButtonRelease, 1)
    connection.sync()
    return {'x': geometry.x + (left + right) // 2, 'y': geometry.y + 140 + (top + bottom) // 2,
            'link_bounds': bounds, 'window': [geometry.x, geometry.y, geometry.width, geometry.height]}


def close_browser(connection, window):
    window.send_event(protocol.event.ClientMessage(window=window,
        client_type=connection.intern_atom('WM_PROTOCOLS'),
        data=(32, [connection.intern_atom('WM_DELETE_WINDOW'), X.CurrentTime, 0, 0, 0])), event_mask=0)
    connection.sync()


def capture_ui(connection, path):
    window = browser_window(connection)
    if window is not None:
        geometry = window.get_geometry()
        raw = window.get_image(0, 0, geometry.width, geometry.height, X.ZPixmap, 0xffffffff)
        Image.frombytes('RGB', (geometry.width, geometry.height), raw.data, 'raw', 'BGRX').save(path)


def navigate_address(connection, window, url, capture=None):
    window.set_input_focus(X.RevertToParent, X.CurrentTime)
    time.sleep(.3)
    geometry = window.get_geometry()
    xtest.fake_input(connection, X.MotionNotify, x=geometry.x + geometry.width // 2, y=geometry.y + 55)
    xtest.fake_input(connection, X.ButtonPress, 1)
    xtest.fake_input(connection, X.ButtonRelease, 1)
    ctrl = connection.keysym_to_keycode(XK.XK_Control_L)
    key = connection.keysym_to_keycode(XK.XK_a)
    xtest.fake_input(connection, X.KeyPress, ctrl)
    xtest.fake_input(connection, X.KeyPress, key)
    xtest.fake_input(connection, X.KeyRelease, key)
    xtest.fake_input(connection, X.KeyRelease, ctrl)
    connection.sync()
    type_text(connection, url)
    time.sleep(.15)
    if capture:
        capture_ui(connection, capture)
    window.set_input_focus(X.RevertToParent, X.CurrentTime)
    enter = connection.keysym_to_keycode(XK.string_to_keysym('Return'))
    xtest.fake_input(connection, X.KeyPress, enter)
    xtest.fake_input(connection, X.KeyRelease, enter)
    connection.sync()
    time.sleep(.15)


def wait_title(window, title, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if title in str(window.get_wm_name() or ''):
            return True
        time.sleep(.05)
    return False


try:
    gopher = start_server('gopher')
    nex = start_server('nex')
    finger = start_server('finger')
    probes = [
        ('gopher-menu', f'gopher://127.0.0.1:{gopher}/', 'gopher', b'\r\n'),
        ('gopher-text', f'gopher://127.0.0.1:{gopher}/0/docs/example', 'gopher', b'/docs/example\r\n'),
        ('gopher-search', f'gopher://127.0.0.1:{gopher}/7/search', 'gopher', b'/search\tlocal query\r\n'),
        ('nex-directory', f'nex://127.0.0.1:{nex}/', 'nex', b'/\r\n'),
        ('nex-text', f'nex://127.0.0.1:{nex}/about.txt', 'nex', b'/about.txt\r\n'),
        ('finger', f'finger://127.0.0.1:{finger}/alice', 'finger', b'alice\r\n'),
    ]
    if args.extended:
        canary = start_server('canary')
        spartan = start_server('spartan')
        http = start_server('http')
        next(server for server in servers if server.kind == 'spartan').canary_port = canary
        probes += [
            ('gopher-link', f'gopher://127.0.0.1:{gopher}/', 'gopher', b'/docs/example\t+\r\n'),
            ('nex-link', f'nex://127.0.0.1:{nex}/', 'nex', b'/about.txt\r\n'),
            ('spartan-document', f'spartan://127.0.0.1:{spartan}/document', 'spartan', b'127.0.0.1 /document 0\r\n'),
            ('spartan-plain', f'spartan://127.0.0.1:{spartan}/plain', 'spartan', b'127.0.0.1 /plain 0\r\n'),
            ('spartan-data', f'spartan://127.0.0.1:{spartan}/data?%00%0A%C3%A9', 'spartan', b'127.0.0.1 /data 4\r\n\x00\n\xc3\xa9'),
            ('spartan-redirect', f'spartan://127.0.0.1:{spartan}/redirect', 'spartan', b'127.0.0.1 /document 0\r\n'),
            ('spartan-input', f'spartan://127.0.0.1:{spartan}/input-page', 'spartan', b'127.0.0.1 /input-page 11\r\nlocal query'),
            ('spartan-client-error', f'spartan://127.0.0.1:{spartan}/client-error', 'spartan', b'127.0.0.1 /client-error 0\r\n'),
            ('spartan-server-error', f'spartan://127.0.0.1:{spartan}/server-error', 'spartan', b'127.0.0.1 /server-error 0\r\n'),
            ('spartan-inert-html', f'spartan://127.0.0.1:{spartan}/html', 'spartan', b'127.0.0.1 /html 0\r\n'),
        ]
    if args.gopher_plus:
        plus = start_server('gopher-plus')
        probes += [
            ('gopher-rfc-search', f'gopher://127.0.0.1:{gopher}/7/search%09local%20query', 'gopher', b'/search\tlocal query\r\n'),
            ('gopher-plus-counted', f'gopher+://127.0.0.1:{plus}/0/file', 'gopher-plus', b'/file\t+\r\n'),
            ('gopher-plus-rfc-layout', f'gopher://127.0.0.1:{plus}/0/file%09%09%2B', 'gopher-plus', b'/file\t\t+\r\n'),
            ('gopher-plus-dot', f'gopher+://127.0.0.1:{plus}/0/dot', 'gopher-plus', b'/dot\t+\r\n'),
            ('gopher-plus-eof', f'gopher+://127.0.0.1:{plus}/0/eof', 'gopher-plus', b'/eof\t+\r\n'),
            ('gopher-plus-attributes', f'gopher+://127.0.0.1:{plus}/0/file%09%09!', 'gopher-plus', b'/file\t!\r\n'),
            ('gopher-plus-raw-attributes', f'gopher+://127.0.0.1:{plus}/0/raw%09%09!', 'gopher-plus', b'/raw\t!\r\n'),
            ('gopher-plus-directory-attributes', f'gopher+://127.0.0.1:{plus}/1/dir%09%09$', 'gopher-plus', b'/dir\t$\r\n'),
            ('gopher-plus-attribute-subset', f'gopher+://127.0.0.1:{plus}/0/file%09%09!%2BVIEWS', 'gopher-plus', b'/file\t!+VIEWS\r\n'),
            ('gopher-plus-view', f'gopher+://127.0.0.1:{plus}/0/file%09%09%2Btext%2Fplain%20De_DE', 'gopher-plus', b'/file\t+text/plain De_DE\r\n'),
            ('gopher-plus-view-link', f'gopher+://127.0.0.1:{plus}/0/file%09%09!', 'gopher-plus', b'/file\t+text/plain De_DE\r\n'),
            ('gopher-plus-error', f'gopher+://127.0.0.1:{plus}/0/error', 'gopher-plus', b'/error\t+\r\n'),
            ('gopher-plus-search', f'gopher+://127.0.0.1:{plus}/7/search', 'gopher-plus', b'/search\tlocal query\t+\r\n'),
            ('gopher-plus-form', f'gopher+://127.0.0.1:{plus}/0/form%09%09%3F', 'gopher-plus', b'/form\t+\t1\r\n+13\r\nlocal query\r\n'),
        ]
    if args.cases:
        probes = [probe for probe in probes if probe[0] in args.cases]
    with tempfile.TemporaryDirectory(prefix='dome-smallnet-') as directory:
        runtime = Path(directory)
        rd, wr = os.pipe()
        xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(wr), '-screen', '0', '1440x1000x24', '-nolisten', 'tcp'], pass_fds=(wr,), stderr=subprocess.DEVNULL)
        os.close(wr)
        try:
            with os.fdopen(rd) as stream:
                screen = ':' + stream.readline().strip()
            connection = display.Display(screen)
            try:
                base_env = {**os.environ, 'DISPLAY': screen, 'FLTK_BACKEND': 'x11', 'LIBGL_ALWAYS_SOFTWARE': '1', 'XDG_RUNTIME_DIR': directory}
                for key in list(base_env):
                    if key in ['WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'MOZ_GCTIMER', 'DOME_BROWSER_GC_PROFILE'] or key.startswith(('DOME_BROWSER_LAYOUT_CAPTURE', 'DOME_BROWSER_FRAME_CAPTURE', 'DOME_BROWSER_MEMORY_CAPTURE')):
                        base_env.pop(key, None)
                for name, url, kind, expected in probes:
                    folder = out / name
                    folder.mkdir(mode=0o700)
                    profile = runtime / name
                    env = dict(base_env)
                    before = len(records)
                    with (folder / 'browser.log').open('w') as log:
                        child = subprocess.Popen([str(binary), '--software', '--no-restore', '--profile', str(profile), '--smoke-test', str(folder / 'frame.ppm'), url], cwd=root, env=env, stdout=log, stderr=log)
                        deadline = time.monotonic() + 45
                        answered = False
                        activated = False
                        try:
                            while child.poll() is None and time.monotonic() < deadline:
                                if name in ['gopher-search', 'gopher-plus-search', 'spartan-input'] and not answered:
                                    answered = answer_search(connection)
                                if name == 'gopher-plus-form' and not answered:
                                    answered = answer_simple_plus_form(connection)
                                if name in ['gopher-link', 'nex-link', 'spartan-input'] and not activated:
                                    activated = activate_first_link(connection)
                                if name == 'gopher-plus-view-link' and not activated:
                                    activated = activate_first_link(connection, skip=1)
                                time.sleep(.05)
                            check(child.poll() == 0, name + ': native load and clean shutdown')
                        finally:
                            if child.poll() is None:
                                child.kill()
                                child.wait()
                    interactions.append({'name': name, 'activated': activated, 'answered': answered})
                    check((kind, expected) in records[before:], name + ': correct local protocol request')
                    frame = Image.open(folder / 'frame.ppm').convert('RGB')
                    frame.save(folder / 'frame.png')
                    check(frame.width >= 640 and frame.height >= 420 and len(frame.getcolors(frame.width * frame.height) or []) > 8, name + ': rendered frame available for review')
                    completed.append(name)
                    print('PASS', name, flush=True)
                if args.extended and 'spartan-inert-html' in completed:
                    check(not any(kind == 'canary' for kind, request in records), 'Native document policy retains inert HTML behavior')
                if args.extended and (not args.cases or 'navigation-cancellation' in args.cases):
                    folder = out / 'navigation-cancellation'
                    folder.mkdir(mode=0o700)
                    env = dict(base_env)
                    env['DOME_BROWSER_FRAME_CAPTURE'] = str(folder / 'frame.png')
                    env['DOME_BROWSER_TRACE'] = '1'
                    env['RUST_LOG'] = 'constellation::constellation=debug'
                    scheme = args.cancellation_scheme
                    slow = next(server for server in servers if server.kind == scheme)
                    base = f'{scheme}://127.0.0.1:{slow.server_address[1]}'
                    initial = base + ('/' if args.cancellation_after_load else '/slow')
                    expected = b'GET /about.txt HTTP/1.1\r\n' if scheme == 'http' else b'/about.txt\r\n'
                    before = len(records)
                    with (folder / 'browser.log').open('w') as log:
                        child = subprocess.Popen([str(binary), '--software', '--no-restore', '--profile', str(runtime / 'cancel'), initial], cwd=root, env=env, stdout=log, stderr=log)
                        try:
                            if args.cancellation_after_load:
                                deadline = time.monotonic() + 10
                                window = browser_window(connection)
                                while window is None and time.monotonic() < deadline:
                                    time.sleep(.05)
                                    window = browser_window(connection)
                                check(window is not None and wait_title(window, 'HTTP /' if scheme == 'http' else base + '/'), 'Active document fixture is ready before starting a pending load')
                                navigate_address(connection, window, base + '/slow')
                            check(slow.slow_started.wait(10), 'Cancellation fixture begins an incomplete load')
                            window = browser_window(connection)
                            check(window is not None, 'Incomplete native load retains a usable browser window')
                            navigate_address(connection, window, base + '/about.txt', folder / 'ui-before-submit.png')
                            deadline = time.monotonic() + 5
                            while (scheme, expected) not in records[before:] and time.monotonic() < deadline:
                                time.sleep(.05)
                            cancellation_observation.update(replacement_request_seen=(scheme, expected) in records[before:],
                                old_connection_closed=slow.slow_closed.is_set(), browser_alive=child.poll() is None)
                            capture_ui(connection, folder / 'ui-before-close.png')
                            check((scheme, expected) in records[before:], 'Navigation completes a replacement protocol request')
                            if scheme == 'nex':
                                check(slow.slow_closed.wait(3) and child.poll() is None, 'Navigation releases the old connection while the browser remains running')
                            check(wait_title(window, 'HTTP /about.txt' if scheme == 'http' else base + '/about.txt'), 'Replacement document reaches the native window')
                            close_browser(connection, window)
                            check(child.wait(timeout=10) == 0, 'Cancellation follow-up closes normally')
                            cancellation_complete = True
                        finally:
                            if child.poll() is None:
                                child.kill()
                                child.wait()
            finally:
                connection.close()
        finally:
            xvfb.terminate()
            xvfb.wait(timeout=5)
finally:
    for server in servers:
        server.shutdown()
        server.server_close()
    report = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'checks': checks, 'completed': completed,
              'driver_sha256': driver_sha256,
              'interactions': interactions, 'selected_cases': args.cases,
              'cancellation_observation': cancellation_observation,
              'cancellation_scheme': args.cancellation_scheme, 'cancellation_after_load': args.cancellation_after_load,
              'extended': args.extended, 'cancellation_complete': cancellation_complete,
              'passed': bool(checks) and len(completed) == len(probes) and cancellation_complete and all(c['passed'] for c in checks),
              'scope': 'Local native loading, wire framing, input and orderly shutdown; extended mode covers link activation, Spartan status and MIME handling, native document policy and cancellation. Captures require visual review. No complete all-smallnet support claim.'}
    (out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
