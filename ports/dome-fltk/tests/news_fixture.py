#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Loopback-only read-only NNTP fixture, including implicit TLS and STARTTLS."""
import socket,socketserver,ssl,threading

def article(message_id='Fixture@Example.invalid'):
    return (f'From: =?UTF-8?Q?Ren=C3=A9e?= <fixture@example.invalid>\r\n'
        'Subject: =?UTF-8?Q?News_fixture_=E4=B8=96=E7=95=8C?=\r\n'
        'Date: Mon, 26 Oct 2026 10:00:00 +0000\r\n'
        f'Message-ID: <{message_id}>\r\n'
        'References: <Parent@Example.invalid>\r\n'
        'Newsgroups: comp.test\r\nMIME-Version: 1.0\r\n'
        'Content-Type: multipart/mixed; boundary=fixture\r\n\r\n'
        '--fixture\r\nContent-Type: text/plain; charset=iso-8859-1\r\n'
        'Content-Transfer-Encoding: quoted-printable\r\n\r\n'
        'Caf=E9 and literal <b>markup</b>\r\n.dot line\r\n'
        '--fixture\r\nContent-Type: application/octet-stream\r\n'
        "Content-Disposition: attachment; filename*=utf-8''test%20file.bin\r\n"
        'Content-Transfer-Encoding: base64\r\n\r\nAAH/\r\n--fixture--\r\n').encode()

class NewsFixture(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True
    def __init__(self,records,kind='news',context=None,implicit=False):
        self.records=records;self.kind=kind;self.context=context;self.implicit=implicit
        self.sessions=[];self.errors=[];self.mode='normal';self.tls_attempts=0
        self.started=threading.Event();self.closed=threading.Event()
        super().__init__(('127.0.0.1',0),Handler)
        self.thread=threading.Thread(target=self.serve_forever,daemon=True);self.thread.start()
    def get_request(self):
        stream,peer=self.socket.accept();stream.settimeout(30)
        if self.implicit:
            self.tls_attempts+=1
            try:stream=self.context.wrap_socket(stream,server_side=True)
            except (ssl.SSLError,OSError):stream.close();raise OSError('Fixture TLS peer declined')
        return stream,peer

class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(30);mode=self.server.mode
        session={'commands':[],'quit':False,'tls':self.server.implicit,'reader':mode!='mode-switch'};self.server.sessions.append(session)
        def send(data):self.wfile.write(data if isinstance(data,bytes) else data.encode());self.wfile.flush()
        def block():
            self.server.started.set()
            try:
                if self.connection.recv(1)==b'':self.server.closed.set()
            except (OSError,ssl.SSLError):self.server.closed.set()
        def dot(data):return b''.join((b'.'+line if line.startswith(b'.') else line) for line in data.splitlines(keepends=True))+b'.\r\n'
        send('201 Local news fixture ready\r\n')
        try:
            while True:
                raw=self.rfile.readline(513)
                if not raw:return
                if len(raw)>512 or not raw.endswith(b'\r\n'):raise AssertionError('Invalid NNTP command framing')
                command=raw[:-2].decode();verb,_,argument=command.partition(' ')
                session['commands'].append(command)
                self.server.records.append({'kind':self.server.kind,'command':command,'tls':session['tls'],'peer_port':self.client_address[1]})
                if verb=='CAPABILITIES':
                    if mode=='bad-header':send('invalid status\r\n');return
                    if mode=='legacy':send('500 Old server\r\n')
                    elif not session['reader']:send('101 Capabilities\r\nVERSION 2\r\nMODE-READER\r\n.\r\n')
                    else:send('101 Capabilities\r\nVERSION 2\r\nREADER\r\nOVER\r\n'+('STARTTLS\r\n' if self.server.context and not session['tls'] else '')+'.\r\n')
                elif command=='MODE READER':
                    session['reader']=True;send('500 Old reader\r\n' if mode=='legacy' else '201 Reader ready\r\n')
                elif verb=='STARTTLS':
                    if not self.server.context or mode=='no-starttls':send('580 TLS unavailable\r\n');continue
                    send('382 Begin TLS\r\n')
                    # No buffered bytes are interpreted after the upgrade.
                    self.connection=self.server.context.wrap_socket(self.connection,server_side=True)
                    self.rfile=self.connection.makefile('rb');self.wfile=self.connection.makefile('wb');session['tls']=True
                elif command.startswith('LIST ACTIVE'):
                    send('215 Group list\r\n')
                    if mode=='catalogue-limit':
                        for n in range(1100):send(f'comp.fixture{n} 101 1 y\r\n')
                    elif argument.endswith('empty.*'):pass
                    else:send('comp.test 101 1 y\r\ncomp.empty 0 0 n\r\n')
                    send('.\r\n')
                elif verb=='GROUP':
                    if mode=='auth':send('480 Authentication required\r\n')
                    elif argument=='absent':send('411 No such group\r\n')
                    elif argument=='comp.empty':send('211 0 0 0 comp.empty\r\n')
                    elif argument=='comp.test':send('211 101 1 101 comp.test\r\n')
                    else:send('411 No such group\r\n')
                elif verb in ['OVER','XOVER']:
                    if (mode=='legacy' and verb=='OVER') or mode=='head-fallback':send('500 Overview unavailable\r\n');continue
                    lo,hi=map(int,argument.split('-'));send('224 Overview\r\n')
                    for n in [hi-1,hi]:
                        if n>=lo:send(f'{n}\t=?UTF-8?Q?News_fixture_=E4=B8=96=E7=95=8C?=\tFixture <fixture@example.invalid>\tMon, 26 Oct 2026 10:00:00 +0000\t<Article-{n}@Example.invalid>\t<Parent@Example.invalid>\t123\t5\r\n')
                    send('.\r\n')
                elif verb=='HEAD':
                    n=int(argument)
                    if n in [100,101]:send(f'221 {n} <Article-{n}@Example.invalid> Headers\r\nSubject: Fixture overview\r\nFrom: Fixture <fixture@example.invalid>\r\nMessage-ID: <Article-{n}@Example.invalid>\r\n.\r\n')
                    else:send('423 No article\r\n')
                elif verb=='ARTICLE':
                    if argument.startswith('<'):
                        ident=argument[1:-1];number=0
                    else:number=int(argument);ident=f'Article-{number}@Example.invalid'
                    if ident=='absent@id':send('430 No article\r\n');continue
                    if ident in ['slow@id','partial@id']:
                        if ident=='partial@id':send('220 0 <partial@id> Article\r\nSubject: Partial\r\n\r\nWaiting\r\n')
                        block();return
                    send(f'220 {number} <{ident if mode!="mismatch" else "wrong@id"}> Article\r\n')
                    if mode=='large-source':
                        send(f'Subject: Large fixture\r\nMessage-ID: <{ident}>\r\n\r\n')
                        remaining=128*1024*1024
                        while remaining:
                            size=min(1000,remaining);send(b'x'*(size-2)+b'\r\n');remaining-=size
                        send('.\r\n');continue
                    if mode=='truncated':send(article(ident));return
                    if mode=='oversize-line':send(b'x'*65537);return
                    send(dot(article(ident)))
                elif verb=='QUIT':
                    session['quit']=True
                    if mode=='silent-quit':block();return
                    send('205 Goodbye\r\n');return
                else:raise AssertionError('Unexpected NNTP command '+command)
        except (BrokenPipeError,ConnectionResetError,socket.timeout,ssl.SSLError):return
        except Exception as error:self.server.errors.append(repr(error))
