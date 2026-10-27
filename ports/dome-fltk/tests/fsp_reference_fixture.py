# SPDX-License-Identifier: GPL-3.0-or-later
"""Unmodified author FSP daemon, private archive, transparent loopback observer."""
import hashlib,json,socket,struct,subprocess,threading,time,zlib
from pathlib import Path

class FspReference:
    def __init__(self,binary,folder,records):
        self.folder=Path(folder);self.folder.mkdir(parents=True,exist_ok=False)
        self.archive=self.folder/'archive';self.archive.mkdir();self.records=records
        self.text='FSP reference text 世界\r\n.literal line\r\n'.encode()
        self.data=bytes(range(256))*257+b'\x00exact ending\xff'
        self.large=bytes(range(256))*(8*1024*1024//256)+b'large exact ending'
        (self.archive/'read me.txt').write_bytes(self.text)
        (self.archive/'file.bin').write_bytes(self.data);(self.archive/'large.bin').write_bytes(self.large)
        (self.archive/'.README').write_text('Local FSP archive <&>\n')
        (self.archive/'subfolder').mkdir();(self.archive/'subfolder'/'nested.txt').write_text('Nested FSP resource\n')
        (self.archive/'protected').mkdir();(self.archive/'protected'/'.FSP_OK_PASSWORD').write_text('fixture-password\n');(self.archive/'protected'/'private.txt').write_text('Protected reference resource\n')
        def chunk(kind,data):return struct.pack('!I',len(data))+kind+data+struct.pack('!I',zlib.crc32(kind+data)&0xffffffff)
        png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('!IIBBBBB',320,160,8,2,0,0,0))+chunk(b'IDAT',zlib.compress((b'\0'+b'\x17\xc5\x65'*320)*160))+chunk(b'IEND',b'')
        (self.archive/'pixel.png').write_bytes(png)
        for i in range(90):(self.archive/f'entry-{i:03}.txt').write_text(f'FSP entry {i}\n')
        with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as reserve:reserve.bind(('127.0.0.1',0));port=reserve.getsockname()[1]
        config=self.folder/'fspd.conf';config.write_text(f'port {port}\nListenAddress 127.0.0.1\nhomedir {self.archive.resolve()}\nread_only yes\nuse_prebuild_dirlists no\npermit_passwordless_owners off\npidlogname {self.folder.resolve()}/fspd.pid\n')
        self.log=(self.folder/'server.log').open('w');self.child=subprocess.Popen([str(Path(binary).resolve()),'-F','-f',str(config.resolve())],stdout=self.log,stderr=self.log)
        self.proxy=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);self.proxy.bind(('127.0.0.1',0));self.proxy.settimeout(.1);self.port=self.proxy.getsockname()[1]
        self.upstream=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);self.upstream.connect(('127.0.0.1',port));self.upstream.settimeout(.1)
        self.active=None;self.stop=threading.Event();self.threads=[]
        def requests():
            while not self.stop.is_set():
                try:data,peer=self.proxy.recvfrom(65536)
                except socket.timeout:continue
                except OSError:break
                self.active=peer
                if len(data)>=12:
                    command,checksum,key,seq,length,position=struct.unpack('!BBHHHI',data[:12]);body=data[12:12+length].removesuffix(b'\0');name,_,password=body.partition(b'\n')
                    self.records.append({'command':command,'sequence':seq,'position':position,'path':name.decode('utf-8','replace'),'password_supplied':bool(password),'fixture_password_matches':password==b'fixture-password','wire_bytes':len(data),'client_port':peer[1]})
                self.upstream.send(data)
        def responses():
            while not self.stop.is_set():
                try:data=self.upstream.recv(65536)
                except (socket.timeout,ConnectionRefusedError):continue
                except OSError:break
                if self.active:
                    try:self.proxy.sendto(data,self.active)
                    except OSError:break
        for fn in [requests,responses]:
            t=threading.Thread(target=fn,daemon=True);t.start();self.threads.append(t)
        time.sleep(.15)
        assert self.child.poll() is None,'Author FSP daemon did not stay running'
        self.probe();self.records.clear()
        (self.folder/'provenance.json').write_text(json.dumps({'binary':str(Path(binary).resolve()),'binary_sha256':hashlib.sha256(Path(binary).read_bytes()).hexdigest(),'server_read_only':True,'loopback_only':True,'wire_proxy':'Unmodified datagrams in both directions; observe read commands and redact passwords','system_installation':False},indent=2)+'\n')
    def probe(self):
        with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as client:
            client.connect(('127.0.0.1',self.port));client.settimeout(2)
            key=0
            for sequence,command,body in [(1,0x4d,b'/\0'),(2,0x4a,b'')]:
                data=bytearray(struct.pack('!BBHHHI',command,0,key,sequence,len(body),0)+body);total=sum(data)+len(data);data[1]=(total+(total>>8))&255;client.send(data);reply=client.recv(65536);assert reply[0]==command;key=struct.unpack('!H',reply[2:4])[0]
    def shutdown(self):self.close()
    def server_close(self):pass
    def close(self):
        self.stop.set();self.proxy.close();self.upstream.close()
        for t in self.threads:t.join(timeout=1)
        if self.child.poll() is None:self.child.terminate();self.child.wait(timeout=5)
        self.log.close()
