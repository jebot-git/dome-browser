#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Loopback read-only CCSO fixtures; no external directory lookups."""
import json,re,socket,socketserver,threading
class CsoFixture(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True
    def __init__(self,records,kind='cso'):
        self.records=records;self.kind=kind;self.mode='normal';self.sessions=[];self.errors=[];self.started=threading.Event();self.closed=threading.Event()
        super().__init__(('127.0.0.1',0),Handler);threading.Thread(target=self.serve_forever,daemon=True).start()
class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(35);mode=self.server.mode;session={'commands':[],'quit':False};self.server.sessions.append(session)
        def send(data):self.wfile.write(data if isinstance(data,bytes) else data.encode('latin-1'));self.wfile.flush()
        def block():
            self.server.started.set()
            try:
                if self.connection.recv(1)==b'':self.server.closed.set()
            except (ConnectionResetError,ConnectionAbortedError):self.server.closed.set()
        try:
            while True:
                line=self.rfile.readline(4097)
                if not line:return
                if not line.endswith(b'\r\n') or len(line)>4096:raise AssertionError('Invalid directory command framing')
                command=line[:-2].decode('latin-1');session['commands'].append(command)
                if command=='set charset=iso-8859-1':
                    send('513:Unknown option\r\n' if mode=='fallback' else '506:Authentication required\r\n' if mode=='denied' else '200:Done\r\n');continue
                if command=='quit':
                    session['quit']=True
                    if mode=='quit-silent':block();return
                    send('200:Bye\r\n');return
                if command=='fields':
                    self.server.records.append({'kind':self.server.kind,'verb':'fields'})
                    send('100:Directory fields\r\n-200:2: email: max 128 Lookup Public Default\r\n-200:2: email: Email address\r\n-200:3: name: max 64 Indexed Lookup Public Default\r\n-200:3: name: Full name\r\n-200:6: alias: max 32 Indexed Lookup Public Default\r\n-200:6: alias: Directory alias\r\n200:Ok\r\n');continue
                if command=='siteinfo':send('-200:1:version:Local directory fixture\r\n-200:2:maildomain:example.invalid\r\n200:Ok\r\n');continue
                if not command.startswith('query '):raise AssertionError('Unexpected directory command')
                rest=command[6:];field='';match=re.match(r'([A-Za-z0-9_-]+)=',rest)
                if match:field=match[1];rest=rest[match.end():]
                term,end=json.JSONDecoder().raw_decode(rest);tail=rest[end:].strip();returns=[]
                if tail:
                    assert tail.startswith('return ');returns=tail[7:].split()
                self.server.records.append({'kind':self.server.kind,'verb':'query','field':field,'term':term,'returns':returns,'wire_hex':line.hex()})
                if term in ['slow','partial-hang']:
                    if term=='partial-hang':send('102:Progress\r\n-200:1:name: Partial directory result\r\n')
                    block();return
                if term=='missing':send('501:No matching entries\r\n');continue
                if term=='truncated':send('102:One match\r\n-200:1:name: Incomplete result\r\n');return
                if term=='bad-header':send('not a directory response\r\n');return
                if term=='oversize-line':send(b'-200:1:name: '+b'a'*65537);return
                if term=='document-limit':
                    for _ in range(257):send(b'-200:1:other: '+b'a'*32760+b'\r\n')
                    send('200:Complete\r\n');continue
                if term=='record-limit':
                    for n in range(1025):send(f'-200:{n+1}:name: Entry\r\n')
                    send('200:Complete\r\n');continue
                value='Ren=E9e' if mode=='fallback' else 'Renée';finish='699:Fixture terminal response' if term=='unknown-terminal' else '201:Directory is read only' if term=='readonly' else '200:Ok'
                send(f'100:Searching\r\n-199:Search in progress\r\n102:Two matching entries\r\n-200:1:name: {value}\r\n-200:1:alias: fixture\r\n-200:1:address: Room 1: West\r\n-503:1:phone: Not public\r\n-200:2:name: Second entry\r\n-200:2:other: <b>literal text</b>\r\n{finish}\r\n')
        except (BrokenPipeError,ConnectionResetError,ConnectionAbortedError,socket.timeout):return
        except Exception as error:self.server.errors.append(repr(error))
class GopherFixture(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True
    def __init__(self,port):
        self.directory_port=port;self.requests=[]
        super().__init__(('127.0.0.1',0),GopherHandler);threading.Thread(target=self.serve_forever,daemon=True).start()
class GopherHandler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(10);self.server.requests.append(self.rfile.readline(4097));self.wfile.write(f'2Local phonebook\tignored selector\tlocalhost\t{self.server.directory_port}\r\n.\r\n'.encode())
