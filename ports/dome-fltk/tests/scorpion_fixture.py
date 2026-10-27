#!/usr/bin/env python3
"""Synthetic read-only Scorpion endpoints for private browser integration tests."""
import hashlib,socket,socketserver,threading,urllib.parse

def block(kind,body=b'',attribute=b'',encoding=0x10):
    if isinstance(body,str):body=body.encode('cp437')
    if isinstance(attribute,str):attribute=attribute.encode('ascii')
    return bytes([kind|encoding])+len(attribute).to_bytes(2,'big')+attribute+len(body).to_bytes(3,'big')+body

def document():
    return b''.join([
        block(1,'Scorpion fixture','start'),block(0,'Caf\x82 <literal> & text'.encode('latin1')),
        block(0,b'Styles: \x12strong\x11 / \x13emphasis\x11'),
        block(13,b'left\tright\nA  B\n\x1b[31mplain\x1b[0m'),
        block(0,b'\xfe\x21\x22\x24',encoding=0),
        block(8,'Open child','/child.scorp'),block(9,'Search fixture','/input'),
        block(10,'Interactive fixture','/interactive'),block(11,'Alternate fixture','/mirror.scorp'),
    ])

class ScorpionFixture(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True
    def __init__(self,context=None,mode='normal',dual=False):
        self.context=context;self.dual=dual;self.mode=mode;self.records=[];self.errors=[];self.started=threading.Event();self.closed=threading.Event();self.stop=threading.Event()
        super().__init__(('127.0.0.1',0),Handler)
        self.thread=threading.Thread(target=self.serve_forever,daemon=True);self.thread.start()
    def close(self):
        self.stop.set();self.shutdown();self.server_close();self.thread.join(timeout=3)
    @property
    def port(self):return self.server_address[1]

class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        fixture=self.server;connection=self.request;tls=False
        try:
            connection.settimeout(3)
            if fixture.context and (not fixture.dual or connection.recv(1,socket.MSG_PEEK)==b'\x16'):
                connection=fixture.context.wrap_socket(connection,server_side=True);tls=True
            with connection:
                connection.settimeout(3);line=b''
                while not line.endswith(b'\r\n'):
                    b=connection.recv(1)
                    if not b:return
                    line+=b
                    if len(line)>4096:raise AssertionError('request limit')
                assert line.startswith(b'R ') and line.endswith(b'\r\n')
                url=line[2:-2].decode('ascii');u=urllib.parse.urlsplit(url)
                public=urllib.parse.urlunsplit((u.scheme,u.netloc.rsplit('@',1)[-1],u.path,u.query,u.fragment))
                authenticated=u.username=='fixture' and u.password=='fixture-pass'
                peer=connection.getpeercert(binary_form=True) if tls else None
                fixture.records.append({'request':public,'authenticated':authenticated,'client_certificate':bool(peer),'client_fingerprint':hashlib.sha256(peer).hexdigest() if peer else None,'transport_tls':tls})
                assert not u.fragment
                mode=fixture.mode;body=document();kind='document:scorpion';status=b''
                if mode=='identity' and not fixture.records[-1]['client_certificate']:status=b'60 * A fixture identity is required\r\n'
                elif mode=='external-redirect':status=b'30 '+fixture.redirect_target.encode('ascii')+b'\r\n'
                elif u.path=='/auth' and not authenticated:status=b'56 Login required\r\n'
                elif u.path=='/auth':body=block(1,'Authenticated fixture')+block(0,'Read-only fixture account')
                elif mode=='failure':status=b'44 1 Retry the local fixture later\r\n'
                elif mode=='bad-header':status=b'20 missing-fields\r\n'
                elif mode=='redirect-loop':status=b'30 '+u.path.encode()+b'\r\n'
                elif u.path=='/redirect':status=b'30 /child.scorp\r\n'
                elif u.path=='/input' and not u.query:status=b'10 Fixture query\r\n'
                elif u.path=='/input':body=block(1,'Input fixture')+block(0,urllib.parse.unquote(u.query).encode('cp437'))
                elif u.path in ['/child.scorp','/mirror.scorp']:body=block(1,'Child fixture')+block(8,'Return','/index.scorp#start')
                elif u.path=='/plain.txt':body='Plain UTF-8: Žluťoučký 世界\n'.encode();kind='text/plain;charset=utf-8'
                elif mode=='truncated-block':body=b'\x10\0\0\0\0\x05short'[:-2]
                elif mode=='large':
                    total=128*1024*1024;chunk=b'0123456789abcdef'*(65536//16)
                    connection.sendall(f'20 {total} application/octet-stream\r\n'.encode());fixture.started.set()
                    for _ in range(total//len(chunk)):
                        if fixture.stop.is_set():return
                        connection.sendall(chunk)
                    fixture.closed.set();return
                elif mode=='partial':
                    connection.sendall(b'20 1048576 text/plain\r\npartial');fixture.started.set()
                    while not fixture.stop.is_set():
                        try:
                            if not connection.recv(1):fixture.closed.set();return
                        except socket.timeout:continue
                    return
                if status:connection.sendall(status)
                else:
                    size='?' if mode=='unknown-length' else str(len(body)+(1 if mode=='truncated-body' else 0))
                    connection.sendall(f'20 {size} {kind}\r\n'.encode()+body)
                    if mode=='keep-open':
                        fixture.started.set();connection.settimeout(.2)
                        while not fixture.stop.is_set():
                            try:
                                if not connection.recv(1):fixture.closed.set();return
                            except socket.timeout:continue
                if tls:
                    try:connection.unwrap().close()
                    except (OSError,TimeoutError):pass
        except (ConnectionError,TimeoutError,OSError):fixture.closed.set()
        except Exception as e:fixture.errors.append(repr(e))
