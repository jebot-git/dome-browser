#!/usr/bin/env python3
"""Bounded-window UDP fixture for Guppy browser integration tests."""
import collections,select,socket,threading,time,urllib.parse

class GuppyFixture:
    def __init__(self,responder,records,mode=lambda url:'normal',chunk_size=8192):
        self.socket=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);self.socket.bind(('127.0.0.1',0));self.server_address=self.socket.getsockname()
        self.responder=responder;self.records=records;self.mode=mode;self.chunk_size=chunk_size
        self.names={};self.bodies={};self.started=threading.Event();self.closed=threading.Event();self.profile=None;self.catalog=None
        self.sessions={};self.dropped=set();self.stop=threading.Event();self.errors=[];self.last_peer=None
        self.thread=threading.Thread(target=self.run,daemon=True);self.thread.start()
    def make_context(self,*args):return None
    def shutdown(self):self.stop.set();self.thread.join(timeout=5)
    def server_close(self):self.socket.close()
    def send(self,data,peer):
        try:self.socket.sendto(data,peer)
        except OSError:pass
    def fill(self,state):
        while len(state['pending'])<state['window'] and not state['made_eof']:
            seq=state['next'];start=state['offset'];body=state['body'][start:start+state['chunk']]
            state['offset']+=len(body);state['next']+=1
            if seq==state['first']:
                wire=f"{seq} {state['mime']}\r\n".encode()+body
            else:wire=f'{seq}\r\n'.encode()+body
            eof=seq!=state['first'] and not body
            state['made_eof']=eof
            if eof:state['eof']=seq
            due=time.monotonic()+.15 if state['mode'].startswith('reorder') and seq==state['first'] else 0
            state['pending'][seq]=[wire,due]
    def run(self):
        try:
            while not self.stop.is_set():
                if select.select([self.socket],[],[],.01)[0]:
                    try:packet,peer=self.socket.recvfrom(65536)
                    except OSError:continue
                    if packet.startswith(b'guppy://'):
                        if peer in self.sessions:continue
                        if not packet.endswith(b'\r\n') or len(packet)>2048:raise AssertionError('Invalid Guppy request')
                        uri=packet[:-2].decode('ascii');url=urllib.parse.urlsplit(uri);mode=self.mode(url);self.last_peer=peer
                        record={'kind':'guppy','uri':uri,'path':url.path,'query':url.query,'metadata':False,'languages':[],'identity':False,'tls':None,'sni':None,'trusted_before_request':False,'peer_port':peer[1],'request_hex':packet.hex(),'submitted':None,'acks':[],'retransmissions':0}
                        self.records.append(record)
                        if mode=='request-retry' and peer not in self.dropped:self.dropped.add(peer);continue
                        status,meta,body=self.responder(url);self.bodies[url.path]=body
                        if mode=='slow':self.started.set();self.sessions[peer]={'control':True,'started':time.monotonic()};continue
                        if mode=='bad-header':self.send(b'5 Invalid status\r\n',peer);continue
                        if status in [1,3,4]:self.send(f'{status} {meta}\r\n'.encode(),peer);continue
                        first=41 if mode=='sequence-41' else 39
                        state={'first':first,'next':first,'mime':meta,'body':body,'offset':0,'pending':collections.OrderedDict(),'made_eof':False,'eof':None,'record':record,'started':time.monotonic(),'mode':mode,'window':64 if mode=='reorder-window' else 8,'chunk':1 if mode=='reorder' else 32 if mode=='reorder-window' else self.chunk_size,'ignored_ack':False,'control':False}
                        self.sessions[peer]=state;self.fill(state)
                    elif peer in self.sessions and packet.endswith(b'\r\n'):
                        state=self.sessions[peer]
                        if state['control']:continue
                        seq=int(packet[:-2]);state['record']['acks'].append(seq)
                        if seq==state['eof']:state['record']['eof_acks']=state['record'].get('eof_acks',0)+1
                        if (state['mode']=='ack-retry' and seq==state['first'] or state['mode']=='eof-retry' and seq==state['eof']) and not state['ignored_ack']:
                            state['ignored_ack']=True;continue
                        state['pending'].pop(seq,None);self.fill(state)
                        if state['made_eof'] and not state['pending']:
                            state['record']['complete']=True;del self.sessions[peer];continue
                now=time.monotonic()
                for peer,state in list(self.sessions.items()):
                    if now-state['started']>120:del self.sessions[peer];continue
                    if state['control']:continue
                    for seq,value in list(state['pending'].items()):
                        if seq==state['eof'] and state['mode']=='missing-eof':continue
                        if now>=value[1]:
                            if value[1]:state['record']['retransmissions']+=1
                            self.send(value[0],peer);value[1]=now+.15
        except BaseException as error:self.errors.append(repr(error))
