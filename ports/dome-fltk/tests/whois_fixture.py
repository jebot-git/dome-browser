# SPDX-License-Identifier: GPL-3.0-or-later
"""Loopback-only WHOIS/RWhois fixtures derived from RFC 3912/2167 transcripts."""
import socket,socketserver,threading,time

WHOIS_TEXT='Info about Smith\r\nMore info about Smith\r\nUnicode directory text 世界\r\n'.encode()
RWHOIS_RESULT=b'domain:ID;I:one.example\r\ndomain:Domain-Name:example.invalid\r\ndomain:Description:Directory text <&>\r\n\r\n%ok\r\n'

class _Handler(socketserver.StreamRequestHandler):
    def handle(self):
        server=self.server;server.active.add(self.connection)
        try:
            self.connection.settimeout(25)
            if server.kind=='rwhois':
                self.wfile.write(b'%rwhois V-1.5:0000ac:00 fixture.example\r\n');self.wfile.flush()
                while True:
                    command=self.rfile.readline(8192)
                    if not command:return
                    server.records.append({'kind':server.kind,'endpoint':server.tag,'command':command.decode('latin1').rstrip('\r\n'),'wire_hex':command.hex()})
                    if command.startswith(b'-rwhois '):self.wfile.write(b'%rwhois V-1.5:0000ac:00 fixture.example\r\n%ok\r\n')
                    elif command in [b'-display dump\r\n',b'-limit 100\r\n',b'-forward off\r\n']:self.wfile.write(b'%ok\r\n')
                    elif command==b'-quit\r\n':self.wfile.write(b'%ok\r\n');self.wfile.flush();return
                    else:break
                    self.wfile.flush()
            else:
                command=self.rfile.readline(8192)
                if not command:return
                server.records.append({'kind':server.kind,'endpoint':server.tag,'command':command.decode('latin1').rstrip('\r\n'),'wire_hex':command.hex()})
            if not command.endswith(b'\r\n'):return
            query=command[:-2]
            if query==b'slow':
                self.wfile.write(b'domain:Description:Pending result\r\n' if server.kind=='rwhois' else b'Pending response\r\n');self.wfile.flush();server.started.set()
                if self.connection.recv(1)==b'':server.closed.set()
                return
            if server.kind=='whois':
                if query==b'large':self.wfile.write(server.large)
                elif query==b'Ren\xe9e':self.wfile.write(b'Latin-1 lookup: Ren\xe9e\r\n')
                else:
                    self.wfile.write(WHOIS_TEXT[:18]);self.wfile.flush();time.sleep(.03);self.wfile.write(WHOIS_TEXT[18:])
                self.wfile.flush();return
            if query==b'missing':self.wfile.write(b'%error 230 No objects found\r\n')
            elif query==b'partial':self.wfile.write(RWHOIS_RESULT.removesuffix(b'%ok\r\n')+b'%error 330 Maximum objects limit\r\n')
            elif query==b'incomplete':self.wfile.write(RWHOIS_RESULT.removesuffix(b'%ok\r\n'));self.wfile.flush();return
            elif query==b'info':self.wfile.write(b'%info on\r\n%ok\r\nDirectory information\r\n%info off\r\n'+RWHOIS_RESULT)
            elif query==b'referral' and server.referral_port:
                self.wfile.write(f'%referral rwhois://127.0.0.1:{server.referral_port}/auth-area=example.invalid\r\n%ok\r\n'.encode())
            elif query==b'large':self.wfile.write(server.large+b'%ok\r\n')
            else:self.wfile.write(RWHOIS_RESULT)
            self.wfile.flush()
            # Keep the result connection open until explicit quit, proving that
            # the browser recognizes the result terminator rather than TCP EOF.
            command=self.rfile.readline(8192)
            if command:
                server.records.append({'kind':server.kind,'endpoint':server.tag,'command':command.decode('latin1').rstrip('\r\n'),'wire_hex':command.hex()})
                if command==b'-quit\r\n':self.wfile.write(b'%ok\r\n');self.wfile.flush()
        except (OSError,BrokenPipeError):pass
        finally:server.active.discard(self.connection)

class WhoisFixture(socketserver.ThreadingTCPServer):
    allow_reuse_address=True;daemon_threads=True;block_on_close=False
    def __init__(self,records,kind='whois'):
        self.kind=kind;self.tag=kind;self.records=records;self.referral_port=None;self.active=set();self.started=threading.Event();self.closed=threading.Event()
        line=b'domain:Description:'+b'x'*4096+b'\r\n' if kind=='rwhois' else b'WHOIS directory line '+b'x'*4096+b'\r\n'
        self.large=line*(8*1024*1024//len(line)+1)
        super().__init__(('127.0.0.1',0),_Handler);self.thread=threading.Thread(target=self.serve_forever,daemon=True);self.thread.start()
    def shutdown(self):
        for client in list(self.active):
            try:client.shutdown(socket.SHUT_RDWR)
            except OSError:pass
        super().shutdown()
