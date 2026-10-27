#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Loopback DICT fixture shared by native browser and mixed-profile tests."""
import shlex,socket,socketserver,threading

class DictFixture(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True
    def __init__(self,records):
        self.records=records;self.sessions=[];self.errors=[];self.started=threading.Event();self.closed=threading.Event()
        super().__init__(('127.0.0.1',0),Handler)
        self.thread=threading.Thread(target=self.serve_forever,daemon=True);self.thread.start()

class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(35);session={'commands':[],'quit':False};self.server.sessions.append(session)
        def send(data):self.wfile.write(data if isinstance(data,bytes) else data.encode());self.wfile.flush()
        def block():
            self.server.started.set()
            try:
                if self.connection.recv(1)==b'':self.server.closed.set()
            except OSError:pass
        send('220 Local DICT fixture <mime> <one@localhost>\r\n')
        try:
            while True:
                line=self.rfile.readline(6145)
                if not line:return
                if not line.endswith(b'\r\n') or len(line)>6144 or len(line.decode())>1024:raise AssertionError('Invalid command framing')
                command=line.decode()[:-2];session['commands'].append(command)
                fields=shlex.split(command);verb=fields[0].upper()
                if verb=='CLIENT':send('250 Client accepted\r\n')
                elif verb=='QUIT':session['quit']=True;send('221 Closing connection\r\n');return
                elif verb=='SHOW' and fields[1].upper()=='DB':send('110 2 databases\r\nwn "Local dictionary 世界"\r\nother "Second & <literal> dictionary"\r\n.\r\n250 Complete\r\n')
                elif verb=='SHOW' and fields[1].upper()=='STRAT':send('111 2 strategies\r\nexact "Exact match"\r\nprefix "Prefix match"\r\n.\r\n250 Complete\r\n')
                elif verb in ['DEFINE','MATCH']:
                    assert len(fields)==(3 if verb=='DEFINE' else 4)
                    database=fields[1];word=fields[-1];strategy=fields[2] if verb=='MATCH' else None
                    self.server.records.append({'kind':'dict','verb':verb,'database':database,'strategy':strategy,'word':word,'peer_port':self.client_address[1]})
                    if word in ['slow','partial-hang']:
                        if word=='partial-hang':send('150 1 definition\r\n151 word wn "Dictionary"\r\nPartial definition\r\n')
                        block();return
                    if word=='missing':send('552 No matching word\r\n');continue
                    if database=='unknown':send('550 Unknown database\r\n');continue
                    if strategy=='unknown':send('551 Unknown strategy\r\n');continue
                    if word=='bad-header':send('invalid response\r\n');return
                    if word=='bad-count':send('152 2 matches\r\nwn fixture\r\n.\r\n250 Complete\r\n');return
                    if word=='oversize-line':send(b'150 1 definition\r\n151 word wn "Dictionary"\r\n'+b'a'*6145);return
                    if word=='document-limit':
                        send('150 1 definition\r\n151 word wn "Dictionary"\r\n')
                        for _ in range(8193):send(b'a'*1022+b'\r\n')
                        send('.\r\n250 Complete\r\n');return
                    if word=='overlong-unicode':send('150 1 definition\r\n151 word wn "Dictionary"\r\n'+'界'*1023+'\r\n.\r\n250 Complete\r\n');return
                    if verb=='MATCH':send('152 2 matches\r\nwn "fixture"\r\nother Wide SCSI\r\n.\r\n250 Complete\r\n');continue
                    if word in ['truncated','missing-final']:
                        send('150 1 definition\r\n151 word wn "Dictionary"\r\nIncomplete definition\r\n'+('.\r\n' if word=='missing-final' else ''));return
                    if word=='controls':send('150 1 definition\r\n151 word wn \"Dictionary\"\r\nPage one\fpage two\u0085next\x7fline\r\n.\r\n250 Complete\r\n');continue
                    if word=='wide':send('150 1 definition\r\n151 word wn "Unicode dictionary"\r\n'+'界'*1022+'\r\n.\r\n250 Complete\r\n');continue
                    quoted='"'+word.replace('\\','\\\\').replace('"','\\"')+'"'
                    send(f'150 2 definitions\r\n151 {quoted} wn "First dictionary"\r\nFirst definition 世界\r\n..dot text\r\n<b>literal markup</b>\r\n.\r\n151 {quoted} other "Second dictionary"\r\nSecond definition only\r\n.\r\n250 Complete\r\n')
                else:raise AssertionError('Unexpected command '+command)
        except (BrokenPipeError,ConnectionResetError,socket.timeout):return
        except Exception as error:self.server.errors.append(repr(error))
