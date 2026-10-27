# SPDX-License-Identifier: GPL-3.0-or-later
"""Unmodified Fedora/ARIN rwhoisd over accepted loopback inetd sockets."""
from pathlib import Path
import hashlib,json,os,pwd,shutil,socket,socketserver,subprocess,threading

class _Handler(socketserver.BaseRequestHandler):
    def handle(self):
        server=self.server;server.active.add(self.request)
        try:
            with (server.folder/f'session-{len(server.children)}.log').open('w') as log:
                child=subprocess.Popen([str(server.binary),'-i','-r','-c',str(server.config)],stdin=self.request,stdout=self.request,stderr=log,cwd=server.data)
                server.children.append(child)
                try:child.wait(timeout=30)
                except subprocess.TimeoutExpired:child.terminate();child.wait(timeout=5)
        finally:server.active.discard(self.request)

class RwhoisReference(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True;block_on_close=False
    def __init__(self,prefix,folder):
        prefix=Path(prefix).resolve();self.folder=Path(folder).resolve();self.folder.mkdir(mode=0o700,parents=True,exist_ok=False);self.data=self.folder/'data';self.binary=prefix/'usr/bin/rwhoisd';self.children=[];self.active=set()
        shutil.copytree(prefix/'var/rwhoisd/samples',self.data)
        (self.data/'rwhoisd.auth_area').write_text('type:master\nname:a.com\ndata-dir:a.com/data\nschema-file:a.com/schema\nsoa-file:a.com/soa\n')
        (self.data/'rwhoisd.dir').write_text('display:yes\nlimit:yes\nquit:yes\nclass:no\ndirective:no\nforward:no\nholdconnect:no\nnotify:no\nregister:no\nschema:no\nsecurity:no\nsoa:no\nstatus:no\nxfer:no\n')
        (self.data/'rwhoisd.x.dir').write_text('');(self.data/'hosts.allow').write_text('ALL: ALL\n');(self.data/'hosts.deny').write_text('')
        super().__init__(('127.0.0.1',0),_Handler)
        self.config=self.folder/'rwhoisd.conf';self.config.write_text(f'root-dir:{self.data}\nauth-area-file:rwhoisd.auth_area\ndirective-file:rwhoisd.dir\nx-directive-file:rwhoisd.x.dir\nlocal-host:reference.example.invalid\nlocal-port:{self.server_address[1]}\nserver-type:inetd\nchrooted:no\nuserid:{pwd.getpwuid(os.getuid()).pw_name}\nuse-syslog:no\nverbosity:-1\ndefault-log-file:{self.folder}/server.log\npid-file:{self.folder}/server.pid\nserver-contact:fixture@example.invalid\n')
        with (self.folder/'index.log').open('w') as log:subprocess.run([str(prefix/'usr/bin/rwhois_indexer'),'-c',str(self.config),'-i','-s','txt'],stdout=log,stderr=log,check=True)
        self.thread=threading.Thread(target=self.serve_forever,daemon=True);self.thread.start()
        self.expected=self.probe()
        (self.folder/'provenance.json').write_text(json.dumps({'binary':str(self.binary),'binary_sha256':hashlib.sha256(self.binary.read_bytes()).hexdigest(),'unmodified_package_binary':True,'indexed_packaged_author_sample':True,'loopback_only':True,'administrative_directives_disabled':True,'system_installed':False},indent=2)+'\n')
    def probe(self):
        with socket.create_connection(self.server_address,timeout=5) as client:
            stream=client.makefile('rwb',buffering=0);assert stream.readline().startswith(b'%rwhois V-1.5:')
            for command in [b'-rwhois V-1.5 DomeBrowser/0.1\r\n',b'-display dump\r\n',b'-limit 100\r\n']:
                stream.write(command)
                while True:
                    line=stream.readline();assert line and not line.startswith(b'%error ')
                    if line.rstrip(b'\r\n')==b'%ok':break
            stream.write(b'domain a.com\r\n');result=b''
            while True:
                line=stream.readline();assert line;result+=line
                if line.rstrip(b'\r\n')==b'%ok' or line.startswith(b'%error '):break
            assert b'Domain-Name:a.com' in result or b'Domain-Name: a.com' in result
            stream.close();return result
    def shutdown(self):
        for client in list(self.active):
            try:client.shutdown(socket.SHUT_RDWR)
            except OSError:pass
        for child in self.children:
            if child.poll() is None:child.terminate()
        super().shutdown()
        for child in self.children:
            if child.poll() is None:child.wait(timeout=5)
