"""Private test adapter: inject provider role metadata into a synthetic host feed.

The fixture still supplies real native bridge framing, leases, history, poses and
pixel submission. This adapter adds classification metadata only; it neither
creates browser events nor touches the desktop's XR socket.
"""
import json, socket, socketserver, struct, threading

class RoleProxy:
 def __init__(self, path, upstream):
  self.primary=True
  self.lock=threading.Lock()
  self.errors=[]
  owner=self
  class Handler(socketserver.BaseRequestHandler):
   def handle(self):
    def read(stream,count):
     result=b''
     while len(result)<count:
      chunk=stream.recv(count-len(result))
      if not chunk:raise EOFError('Incomplete private bridge message')
      result+=chunk
     return result
    try:
     self.request.settimeout(3)
     header=read(self.request,4);size=struct.unpack('<I',header)[0]
     assert 0<size<=65536
     raw=read(self.request,size);request=json.loads(raw)
     payload_size=request.get('bytes',0) if request.get('op')=='end_frame' else 0
     assert 0<=payload_size<=128*1024*1024
     with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as upstream_socket:
      upstream_socket.settimeout(3);upstream_socket.connect(str(upstream))
      upstream_socket.sendall(header+raw)
      remaining=payload_size
      while remaining:
       chunk=read(self.request,min(65536,remaining));upstream_socket.sendall(chunk);remaining-=len(chunk)
      size=struct.unpack('<I',read(upstream_socket,4))[0];assert 0<size<=65536
      response=json.loads(read(upstream_socket,size))
     if request.get('op')=='wait_frame' and response.get('ok'):
      with owner.lock:primary=owner.primary
      for frame in [response,*[update['frame']for update in response.get('input_batch',{}).get('updates',[])]]:
       for source in frame.get('inputs',[]):source['primary_action']=primary
     raw=json.dumps(response,separators=(',',':')).encode();assert len(raw)<=65536
     self.request.sendall(struct.pack('<I',len(raw))+raw)
    except Exception as error:owner.errors.append(str(error))
  class Server(socketserver.ThreadingUnixStreamServer):
   daemon_threads=True
  self.server=Server(str(path),Handler)
  self.thread=threading.Thread(target=self.server.serve_forever,daemon=True);self.thread.start()
 def set_role(self, primary):
  with self.lock:self.primary=primary
 def close(self):
  self.server.shutdown();self.server.server_close();self.thread.join()
