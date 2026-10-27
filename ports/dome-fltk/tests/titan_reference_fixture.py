#!/usr/bin/env python3
"""Private unmodified App-Phoebe 4.07 interoperability server."""
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import ssl
import subprocess
import threading
import time
import urllib.parse


class PhoebeFixture:
    def __init__(self, source, prefix, root, output, certificate, key):
        source, prefix, root, output = map(Path, [source, prefix, root, output])
        self.data = root / 'phoebe-data'
        self.data.mkdir(mode=0o700)
        self._records = []
        self.lock = threading.Lock()
        self.errors = []
        self.mode = 'normal'
        self.started = threading.Event()
        self.closed = threading.Event()
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1', 0))
            self.port = reserve.getsockname()[1]
        env = {**os.environ, 'HOME': str(root),
               'XDG_CONFIG_HOME': str(root / 'phoebe-config'),
               'PERL5LIB': ':'.join(str(p) for p in [source/'lib', prefix/'usr/share/perl5/vendor_perl', prefix/'usr/lib64/perl5/vendor_perl'])}
        command = ['perl', str(source/'script/phoebe'), '--host=localhost',
                   '--port='+str(self.port), '--cert_file='+str(certificate),
                   '--key_file='+str(key), '--wiki_dir='+str(self.data),
                   '--wiki_token=fixture-token', '--log_level=info',
                   '--wiki_mime_type=text/gemini', '--wiki_mime_type=text/plain',
                   '--wiki_mime_type=application/octet-stream',
                   '--wiki_page_size_limit=268435456']
        self.log = (output/'phoebe.log').open('w')
        self.process = subprocess.Popen(command, env=env, cwd=root,
                                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                        text=True, encoding='utf-8', errors='replace')
        self.reader = threading.Thread(target=self.read_log, daemon=True)
        self.reader.start()
        deadline = time.monotonic()+15
        ready = False
        while time.monotonic()<deadline and self.process.poll() is None:
            try:
                with socket.create_connection(('127.0.0.1', self.port), timeout=.2):
                    ready = True
                    break
            except OSError:
                time.sleep(.05)
        if not ready:
            self.close()
            raise RuntimeError('Private Phoebe did not open its loopback listener')
        # Classify this dependency stack's TLS EOF behavior independently of
        # the browser. The probe changes only a temporary fixture resource.
        probe=self.data/'file/transport-probe'
        probe.parent.mkdir(parents=True,exist_ok=True)
        probe.write_bytes(b'private transport probe')
        context=ssl.create_default_context(cafile=str(certificate))
        data=b'';ending='orderly TLS EOF'
        with socket.create_connection(('localhost',self.port),timeout=5) as tcp:
            with context.wrap_socket(tcp,server_hostname='localhost',suppress_ragged_eofs=False) as tls:
                tls.sendall(f'titan://localhost:{self.port}/transport-probe;size=0;mime=application%2Foctet%2Dstream;token=fixture-token\r\n'.encode())
                try:
                    while True:
                        block=tls.recv(65536)
                        if not block:break
                        data+=block
                except ssl.SSLEOFError:ending='TLS EOF without close_notify'
        if not data.startswith(b'20 ') or probe.exists():
            self.close()
            raise RuntimeError('Private reference transport probe did not complete deletion')
        self.ragged_delete_receipt=ending!='orderly TLS EOF'
        deadline=time.monotonic()+3
        while time.monotonic()<deadline:
            with self.lock:
                if self._records:
                    self._records.clear()
                    break
            time.sleep(.01)
        (output/'reference-transport-probe.json').write_text(json.dumps({
            'independent_client':'Python ssl with CA and hostname verification',
            'suppress_ragged_eofs':False,'status':'20','ending':ending,
            'file_deleted':not probe.exists(),'setup_only':True},indent=2)+'\n')
        source_hashes = {name: hashlib.sha256((source/name).read_bytes()).hexdigest()
                         for name in ['script/phoebe', 'lib/App/Phoebe.pm', 'META.json']}
        (output/'reference-provenance.json').write_text(json.dumps({
            'distribution': 'App-Phoebe-4.07',
            'source': 'https://cpan.metacpan.org/authors/id/S/SC/SCHROEDER/App-Phoebe-4.07.tar.gz',
            'archive_sha256': 'd37725da63e2090e6fa748090b36c7bef35c244e0cf607bba3f35e4f36b9a074',
            'source_sha256': source_hashes, 'source_modified': False,
            'system_installation': False, 'loopback_only': True,
            'binary_delete_receipt_eof':ending,
            'arguments': [arg if not arg.startswith('--wiki_token=') else '--wiki_token=<fixture token>' for arg in command],
            'scope': 'Normal author TLS server and private wiki; request logs are observed without changing protocol bytes. Saved bytes come from the actual wiki files.'
        }, indent=2)+'\n')

    def read_log(self):
        for line in self.process.stdout:
            match = re.search(r'Looking at ((?:titan|gemini)://[^\s]+)', line)
            if match:
                url = urllib.parse.urlsplit(match[1])
                path, _, parameters = url.path.partition(';')
                params = dict(urllib.parse.unquote(value).split('=', 1) for value in parameters.split(';') if '=' in value)
                record = {'scheme': url.scheme, 'path': path, 'query': url.query,
                          'client_certificate': False, 'bytes': 0}
                if url.scheme=='titan':
                    record.update(size=int(params['size']), mime=params.get('mime','text/gemini'),
                                  token_matches=params.get('token')=='fixture-token', token_present=bool(params.get('token')))
                with self.lock:
                    self._records.append(record)
                self.log.write('Received '+url.scheme+' resource request\n')
            elif 'Token:' not in line and 'titan://' not in line and 'gemini://' not in line:
                self.log.write(line)
            self.log.flush()

    @property
    def records(self):
        with self.lock:
            records = [dict(item) for item in self._records]
        for record in records:
            if record['scheme']!='titan':
                continue
            name = urllib.parse.unquote(record['path']).lstrip('/')
            text = record['mime'] in ['text/plain','text/gemini']
            file = self.data / ('page' if text else 'file') / (name+'.gmi' if text else name)
            if file.exists():
                data = file.read_bytes()
                record.update(bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
            elif record['size']==0:
                remaining=[path for path in [self.data/'page'/(name+'.gmi'),self.data/'file'/name] if path.exists()]
                record.update(bytes=sum(path.stat().st_size for path in remaining), sha256=hashlib.sha256(b'').hexdigest(),deleted=not remaining)
        return records

    def prepare_case(self, name):
        if name=='delete':
            file=self.data/'page/resource-delete.gmi'
            file.parent.mkdir(parents=True,exist_ok=True)
            file.write_text('# Private resource to delete\n')
        if name=='delete-file':
            file=self.data/'file/resource-delete-file'
            file.parent.mkdir(parents=True,exist_ok=True)
            file.write_bytes(b'Private binary resource\x00\xff')

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.reader.join(timeout=3)
        self.log.close()
