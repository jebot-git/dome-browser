#!/usr/bin/env python3
"""Private TLS Titan server fixtures; never use a public upload endpoint."""
import hashlib
import socketserver
import threading
import urllib.parse


class TitanFixture(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self, context):
        self.context = context
        self.mode = 'normal'
        self.records = []
        self.errors = []
        self.started = threading.Event()
        self.closed = threading.Event()
        self.stop = threading.Event()
        super().__init__(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    @property
    def port(self):
        return self.server_address[1]

    def close(self):
        self.stop.set()
        self.shutdown()
        self.server_close()
        self.thread.join(timeout=3)


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        fixture = self.server
        try:
            self.request.settimeout(5)
            with fixture.context.wrap_socket(self.request, server_side=True) as stream:
                line = b''
                while not line.endswith(b'\r\n'):
                    byte = stream.recv(1)
                    if not byte:
                        return
                    line += byte
                    assert len(line) <= 1024
                url = urllib.parse.urlsplit(line[:-2].decode('ascii'))
                assert not url.fragment and not url.username and not url.password
                record = {'scheme': url.scheme, 'path': url.path.split(';', 1)[0],
                          'query': url.query, 'bytes': 0, 'header_bytes': len(line),
                          'client_certificate': bool(stream.getpeercert(binary_form=True))}
                fixture.records.append(record)
                if url.scheme == 'gemini':
                    response = f'30 titan://localhost:{fixture.port}/resource;size=0;token=ignored-token\r\n'.encode() if fixture.mode=='gemini-redirect' else b'20 text/gemini\r\n# Existing resource\nOriginal content\n'
                else:
                    assert url.scheme == 'titan'
                    values = url.path.split(';')[1:]
                    params = dict(urllib.parse.unquote(value).split('=', 1) for value in values)
                    assert set(params) <= {'size', 'mime', 'token'}
                    size = int(params['size'])
                    assert 0 <= size <= 256 * 1024 * 1024
                    record.update(size=size, mime=params.get('mime', 'text/gemini'),
                                  token_matches=params.get('token') == 'fixture-token',
                                  token_present=bool(params.get('token')))
                    mode = fixture.mode
                    if mode == 'early-failure':
                        response = b'50 Fixture intent declined\r\n'
                    elif mode == 'identity' and not record['client_certificate']:
                        response = b'60 Select a fixture identity\r\n'
                    elif mode in ['input', 'sensitive'] and not url.query:
                        response = b'11 Fixture query\r\n' if mode == 'sensitive' else b'10 Fixture query\r\n'
                    else:
                        digest = hashlib.sha256()
                        while record['bytes'] < size and not fixture.stop.is_set():
                            data = stream.recv(min(65536, size - record['bytes']))
                            if not data:
                                fixture.closed.set()
                                return
                            record['bytes'] += len(data)
                            digest.update(data)
                            fixture.started.set()
                            if mode == 'stalled':
                                while not fixture.stop.wait(.05):
                                    # Peek with a short timeout only after the
                                    # client has been cancelled by the driver.
                                    if getattr(fixture, 'cancel_requested', False):
                                        try:
                                            if not stream.recv(65536):
                                                fixture.closed.set()
                                                return
                                        except OSError:
                                            fixture.closed.set()
                                            return
                                return
                        record['sha256'] = digest.hexdigest()
                        if mode == 'redirect':
                            response = f'30 gemini://localhost:{fixture.port}/read\r\n'.encode()
                        elif mode == 'redirect-titan':
                            response = f'30 titan://localhost:{fixture.port}/next;size=999;token=ignored\r\n'.encode()
                        elif mode == 'failure':
                            response = b'50 Fixture did not accept the submission\r\n'
                        elif mode == 'bad-header':
                            response = b'broken\r\n'
                        else:
                            title = 'Titan deleted' if size == 0 else 'Titan uploaded'
                            response = f'20 text/gemini\r\n# {title}\nAccepted {size} bytes\n'.encode()
                stream.sendall(response)
                fixture.closed.set()
                try:
                    stream.unwrap().close()
                except OSError:
                    pass
        except (OSError, TimeoutError):
            fixture.closed.set()
        except Exception as error:
            fixture.errors.append(repr(error))
