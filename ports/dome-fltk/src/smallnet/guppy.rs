// SPDX-License-Identifier: GPL-3.0-or-later
//! Guppy v0.4.4: one UDP request, sequenced data and explicit acknowledged EOF.
//! Specification: https://github.com/dimkr/guppy-protocol
use super::{Error, MAX_BYTES, gemini};
use std::{collections::BTreeMap, future::Future, io, pin::Pin, sync::{Arc,Mutex}, task::{Context,Poll}, time::Duration};
use tokio::{io::{AsyncRead,AsyncReadExt,ReadBuf}, net::UdpSocket, sync::Semaphore, time::Instant};
use url::Url;

const MAX_SEQUENCE:u32=2_147_483_647;
const MAX_PACKET:usize=65536;
const REORDER_PACKETS:usize=32;
const REORDER_BYTES:usize=2*1024*1024;
const BATCH:usize=32*1024;
const RETRY:Duration=Duration::from_millis(500);
const IDLE:Duration=Duration::from_secs(30);
static LINGERS:Semaphore=Semaphore::const_new(32);
type End=Arc<Mutex<Option<(u32,u32)>>>;

pub(super) fn request_bytes(url:&Url)->Result<Vec<u8>,Error> {
    gemini::uri_request_bytes(url,"guppy",2046)
}

#[derive(Debug,PartialEq)]
enum Packet {Success(u32,String,Vec<u8>),Chunk(u32,Vec<u8>),Input(String),Redirect(String),Failure(String)}
fn packet(bytes:&[u8])->Result<Packet,Error> {
    let crlf=bytes.windows(2).position(|part|part==b"\r\n").ok_or(Error::Response)?;
    let header=std::str::from_utf8(&bytes[..crlf]).map_err(|_|Error::Response)?;
    let (number,meta)=match header.split_once(' ') {Some((number,meta))=>(number,Some(meta)),None=>(header,None)};
    if number.is_empty() || !number.bytes().all(|b|b.is_ascii_digit()) {return Err(Error::Response);}
    let seq:u32=number.parse().map_err(|_|Error::Response)?;
    let data=&bytes[crlf+2..];
    if let Some(meta)=meta {
        if meta.chars().any(char::is_control) {return Err(Error::Response);}
        if matches!(seq,1|3|4) {
            if !data.is_empty() {return Err(Error::Response);}
            return Ok(match seq {1=>Packet::Input(meta.into()),3 if !meta.is_empty()=>Packet::Redirect(meta.into()),4=>Packet::Failure(meta.into()),_=>return Err(Error::Response)});
        }
        if !(6..=MAX_SEQUENCE).contains(&seq) {return Err(Error::Response);}
        let mime:mime::Mime=meta.parse().map_err(|_|Error::Response)?;
        Ok(Packet::Success(seq,mime.to_string(),data.to_vec()))
    }else{
        if !(6..=MAX_SEQUENCE).contains(&seq) {return Err(Error::Response);}
        Ok(Packet::Chunk(seq,data.to_vec()))
    }
}
async fn send(socket:&UdpSocket,bytes:&[u8])->Result<(),Error> {
    if socket.send(bytes).await.map_err(|_|Error::Transport)?!=bytes.len() {return Err(Error::Transport);}
    Ok(())
}
async fn ack(socket:&UdpSocket,seq:u32)->Result<(),Error> {send(socket,format!("{seq}\r\n").as_bytes()).await}

// A socket remains owned by the request/reader: cancelling a pending load drops
// its receive future and port immediately. Only completed sessions linger.
struct Chunk {data:Vec<u8>,eof:bool}
struct Session {
    socket:Arc<UdpSocket>,first:u32,next:u32,mime:String,queue:BTreeMap<u32,Chunk>,bytes:usize,
    last_ack:u32,deadline:Instant,retry_at:Instant,buffer:Vec<u8>,end:End,finished:bool,
}
impl Session {
    async fn next_packet(&mut self)->Result<Packet,Error> {
        loop {
            let now=Instant::now();
            if now>=self.deadline {return Err(Error::Timeout);}
            if now>=self.retry_at {ack(&self.socket,self.last_ack).await?;self.retry_at=Instant::now()+RETRY;}
            tokio::select! {
                result=self.socket.recv(&mut self.buffer)=>return packet(&self.buffer[..result.map_err(|_|Error::Transport)?]),
                _=tokio::time::sleep_until(self.retry_at.min(self.deadline))=>{},
            }
        }
    }
    async fn accept(&mut self,received:Packet)->Result<(),Error> {
        let (seq,data)=match received {
            Packet::Success(seq,mime,_) if seq==self.first && mime==self.mime=>{ack(&self.socket,seq).await?;return Ok(());},
            Packet::Chunk(seq,data)=>(seq,data),
            _=>return Err(Error::Response),
        };
        if seq<self.first {return Ok(());}
        if seq<self.next || self.queue.contains_key(&seq) {ack(&self.socket,seq).await?;return Ok(());}
        // Reserve room for the missing next packet even when the reorder cache
        // is full. Uncached packets remain unacknowledged for retransmission.
        if seq!=self.next && (self.queue.len()>=REORDER_PACKETS || self.bytes+data.len()>REORDER_BYTES) {return Ok(());}
        self.bytes+=data.len();self.queue.insert(seq,Chunk {eof:data.is_empty(),data});ack(&self.socket,seq).await
    }
    async fn next_one(&mut self)->Result<Option<Vec<u8>>,Error> {
        let mut polled=0;
        loop {
            if self.finished {return Ok(None);}
            if let Some(chunk)=self.queue.remove(&self.next) {
                self.bytes-=chunk.data.len();let seq=self.next;
                if chunk.eof {
                    if !self.queue.is_empty() {return Err(Error::Response);}
                    *self.end.lock().unwrap_or_else(|e|e.into_inner())=Some((self.first,seq));
                    self.finished=true;return Ok(None);
                }
                if seq==MAX_SEQUENCE {return Err(Error::Response);}
                self.next+=1;self.last_ack=seq;self.deadline=Instant::now()+IDLE;self.retry_at=Instant::now()+RETRY;
                if !chunk.data.is_empty() {return Ok(Some(chunk.data));}
            }else{
                let received=self.next_packet().await?;self.accept(received).await?;
                polled+=1;if polled%64==0 {tokio::task::yield_now().await;}
            }
        }
    }
    async fn batch(&mut self)->Result<Option<Vec<u8>>,Error> {
        let mut bytes=Vec::new();
        while bytes.len()<BATCH {
            match self.next_one().await? {Some(chunk)=>bytes.extend_from_slice(&chunk),None=>break}
        }
        if bytes.is_empty() {Ok(None)}else{Ok(Some(bytes))}
    }
}
type Next=Pin<Box<dyn Future<Output=(Session,Result<Option<Vec<u8>>,Error>)>+Send>>;
struct Reader {session:Option<Session>,next:Option<Next>,bytes:Vec<u8>,position:usize,done:bool}
impl AsyncRead for Reader {
    fn poll_read(mut self:Pin<&mut Self>,cx:&mut Context<'_>,target:&mut ReadBuf<'_>)->Poll<io::Result<()>> {
        if target.remaining()==0 {return Poll::Ready(Ok(()));}
        if self.position<self.bytes.len() {
            let n=target.remaining().min(self.bytes.len()-self.position);
            target.put_slice(&self.bytes[self.position..self.position+n]);self.position+=n;
            return Poll::Ready(Ok(()));
        }
        if self.done {return Poll::Ready(Ok(()));}
        if self.next.is_none() {
            let mut session=self.session.take().expect("Live Guppy reader owns its session");
            self.next=Some(Box::pin(async move {let bytes=session.batch().await;(session,bytes)}));
        }
        match self.next.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Pending=>Poll::Pending,
            Poll::Ready((session,result))=>{
                self.next=None;self.session=Some(session);
                match result {
                    Ok(Some(bytes))=>{let n=target.remaining().min(bytes.len());target.put_slice(&bytes[..n]);self.bytes=bytes;self.position=n;Poll::Ready(Ok(()))},
                    Ok(None)=>{self.done=true;Poll::Ready(Ok(()))},
                    Err(error)=>{self.done=true;self.session=None;Poll::Ready(Err(io::Error::new(if error==Error::Timeout {io::ErrorKind::TimedOut}else{io::ErrorKind::InvalidData},error.message())))},
                }
            },
        }
    }
}

async fn finish(socket:Arc<UdpSocket>,end:End)->Result<(),Error> {
    let (first,last)=end.lock().unwrap_or_else(|e|e.into_inner()).ok_or(Error::Response)?;
    // Re-acknowledge a lost final ACK without delaying page presentation.
    // The deadline and permit bound completed-session resources.
    if let Ok(permit)=LINGERS.try_acquire() {
        tokio::spawn(async move {
            let _permit=permit;let deadline=Instant::now()+Duration::from_millis(2200);let mut buffer=vec![0;MAX_PACKET];let mut received=0;
            while Instant::now()<deadline {
                let n=match tokio::time::timeout_at(deadline,socket.recv(&mut buffer)).await {Ok(Ok(n))=>n,_=>break};
                received+=1;if received%64==0 {tokio::task::yield_now().await;}
                let seq=match packet(&buffer[..n]) {Ok(Packet::Success(seq,_,_)|Packet::Chunk(seq,_)) if (first..=last).contains(&seq)=>seq,_=>continue};
                if ack(&socket,seq).await.is_err() {break;}
            }
        });
    }
    Ok(())
}

enum Start {Data(String,Session),Control(gemini::Reply)}
async fn start(socket:Arc<UdpSocket>,request:&[u8])->Result<Start,(Error,bool)> {
    let deadline=Instant::now()+IDLE;let mut retry_at=Instant::now();let mut buffer=vec![0;MAX_PACKET];
    let mut queue=BTreeMap::new();let mut size=0;let mut seen=false;let mut received=0;
    loop {
        let now=Instant::now();if now>=deadline {return Err((Error::Timeout,false));}
        if now>=retry_at {send(&socket,request).await.map_err(|e|(e,false))?;retry_at=Instant::now()+RETRY;}
        let n=tokio::select! {
            result=socket.recv(&mut buffer)=>result.map_err(|e|(Error::Transport,!seen && e.kind()==io::ErrorKind::ConnectionRefused))?,
            _=tokio::time::sleep_until(retry_at.min(deadline))=>continue,
        };
        seen=true;received+=1;if received%64==0 {tokio::task::yield_now().await;}
        match packet(&buffer[..n]).map_err(|e|(e,false))? {
            Packet::Input(prompt)=>return Ok(Start::Control(gemini::Reply::Input {sensitive:false,prompt})),
            Packet::Failure(message)=>return Ok(Start::Control(gemini::Reply::Failure(4,message))),
            Packet::Redirect(meta)=>{
                // Resolve relative references with the URL parser; never turn a
                // server redirect into an executable/external application URI.
                let url=Url::parse(std::str::from_utf8(&request[..request.len()-2]).map_err(|_|(Error::Address,false))?).map_err(|_|(Error::Address,false))?;
                let target=url.join(&meta).map_err(|_|(Error::Response,false))?;
                if !matches!(target.scheme(),"guppy"|"gemini"|"mercury"|"gopher"|"gopher+"|"gophers"|"scroll"|"nex"|"spartan"|"finger"|"ftp"|"http"|"https") || !target.username().is_empty() || target.password().is_some() {return Err((Error::Response,false));}
                return Ok(Start::Control(gemini::Reply::Redirect(target)));
            },
            Packet::Success(first,mime,data)=>{
                queue.retain(|seq,_|*seq>first);size=queue.values().map(|c:&Chunk|c.data.len()).sum();
                size+=data.len();queue.insert(first,Chunk {data,eof:false});ack(&socket,first).await.map_err(|e|(e,false))?;
                return Ok(Start::Data(mime.clone(),Session {socket,first,next:first,mime,queue,bytes:size,last_ack:first,deadline:Instant::now()+IDLE,retry_at:Instant::now()+RETRY,buffer,end:Arc::new(Mutex::new(None)),finished:false}));
            },
            Packet::Chunk(seq,data)=>{
                if queue.contains_key(&seq) {ack(&socket,seq).await.map_err(|e|(e,false))?;continue;}
                if queue.len()<REORDER_PACKETS && size+data.len()<=REORDER_BYTES {
                    size+=data.len();queue.insert(seq,Chunk {eof:data.is_empty(),data});ack(&socket,seq).await.map_err(|e|(e,false))?;
                }
            },
        }
    }
}

pub(super) async fn open(url:&Url,download:bool)->Result<gemini::Reply,Error> {
    let request=request_bytes(url)?;
    let host=url.host_str().ok_or(Error::Address)?.trim_matches(['[',']']);let port=url.port().unwrap_or(6775);
    if port==0 {return Err(Error::Address);}
    let addresses=tokio::net::lookup_host((host,port)).await.map_err(|_|Error::Transport)?;
    let mut started=None;
    for peer in addresses {
        let socket=UdpSocket::bind(if peer.is_ipv6() {"[::]:0"}else{"0.0.0.0:0"}).await.map_err(|_|Error::Transport)?;
        if socket.connect(peer).await.is_err() {continue;}
        match start(Arc::new(socket),&request).await {
            Ok(value)=>{started=Some(value);break;},
            Err((_,true))=>continue,
            Err((error,false))=>return Err(error),
        }
    }
    let (mime,session)=match started.ok_or(Error::Transport)? {Start::Control(reply)=>return Ok(reply),Start::Data(mime,session)=>(mime,session)};
    let socket=session.socket.clone();let end=session.end.clone();
    let reader=Reader {session:Some(session),next:None,bytes:Vec::new(),position:0,done:false};
    let kind:mime::Mime=mime.parse().map_err(|_|Error::Response)?;
    let preview=kind.type_()==mime::TEXT || kind.type_()==mime::IMAGE || kind.essence_str()=="application/xhtml+xml";
    if download || !preview {
        return Ok(gemini::Reply::Stream(super::transfer::Transfer::new(mime,None,reader).with_completion(finish(socket,end))));
    }
    let mut bytes=Vec::new();reader.take((MAX_BYTES+1)as u64).read_to_end(&mut bytes).await.map_err(|_|Error::Transport)?;
    if bytes.len()>MAX_BYTES {return Err(Error::TooLarge);}
    finish(socket,end).await?;
    Ok(gemini::Reply::Success(mime,bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UdpSocket;

    #[test]
    fn packet_numbers_do_not_confuse_success_with_control_codes() {
        for number in [6,39,41,999,MAX_SEQUENCE] {
            assert!(matches!(packet(format!("{number} text/plain\r\ndata").as_bytes()),Ok(Packet::Success(n,_,data)) if n==number && data==b"data"));
        }
        assert_eq!(packet(b"1 Name\r\n").unwrap(),Packet::Input("Name".into()));
        assert_eq!(packet(b"3 ../next\r\n").unwrap(),Packet::Redirect("../next".into()));
        assert_eq!(packet(b"4 Missing\r\n").unwrap(),Packet::Failure("Missing".into()));
        assert_eq!(packet(b"41\r\n").unwrap(),Packet::Chunk(41,Vec::new()));
        for bytes in [b"0 text/plain\r\nx".as_slice(),b"5 text/plain\r\nx",b"-6 text/plain\r\nx",b"2147483648 text/plain\r\nx",b"6 \r\nx",b"6 text/plain\nx",b"1 Name\r\nextra",b"6 text/plain\0\r\nx",b"+6\r\nx"] {assert!(packet(bytes).is_err(),"{bytes:?}");}
    }
    #[test]
    fn request_uri_limit_counts_crlf_and_keeps_only_wire_resource_components() {
        assert_eq!(request_bytes(&Url::parse("guppy://EXAMPLE.com?two%20words#part").unwrap()).unwrap(),b"guppy://example.com/?two%20words\r\n");
        let prefix="guppy://host/";
        let valid=Url::parse(&format!("{prefix}{}","x".repeat(2046-prefix.len()))).unwrap();
        assert_eq!(request_bytes(&valid).unwrap().len(),2048);
        assert!(request_bytes(&Url::parse(&(valid.to_string()+"x")).unwrap()).is_err());
        for text in ["guppy://user@host/","guppy://host:0/","guppy://host:25/","mercury://host/"] {assert!(request_bytes(&Url::parse(text).unwrap()).is_err());}
    }
    async fn client(port:u16)->Arc<UdpSocket> {
        let socket=UdpSocket::bind("127.0.0.1:0").await.unwrap();socket.connect(("127.0.0.1",port)).await.unwrap();Arc::new(socket)
    }
    async fn receive(socket:&UdpSocket)->(Vec<u8>,std::net::SocketAddr) {
        let mut bytes=vec![0;MAX_PACKET];let (n,peer)=tokio::time::timeout(Duration::from_secs(5),socket.recv_from(&mut bytes)).await.unwrap().unwrap();bytes.truncate(n);(bytes,peer)
    }
    #[tokio::test(flavor="current_thread")]
    async fn retries_reorders_and_duplicate_final_acks_preserve_one_port_and_utf8_bytes() {
        let socket=UdpSocket::bind("127.0.0.1:0").await.unwrap();let port=socket.local_addr().unwrap().port();
        let server=tokio::spawn(async move {
            let (first,peer)=receive(&socket).await;let (second,same)=receive(&socket).await;
            assert_eq!(peer,same);assert_eq!(first,second);assert!(first.starts_with(b"guppy://"));
            socket.send_to(b"41\r\nfinal\n",peer).await.unwrap();assert_eq!(receive(&socket).await.0,b"41\r\n");
            socket.send_to(b"39 text/gemini\r\n# \xe4",peer).await.unwrap();assert_eq!(receive(&socket).await.0,b"39\r\n");
            socket.send_to(b"39 text/gemini\r\n# \xe4",peer).await.unwrap();assert_eq!(receive(&socket).await.0,b"39\r\n");
            socket.send_to(b"40\r\n\xb8\x96\xe7\x95\x8c\n",peer).await.unwrap();assert_eq!(receive(&socket).await.0,b"40\r\n");
            socket.send_to(b"42\r\n",peer).await.unwrap();assert_eq!(receive(&socket).await.0,b"42\r\n");
            tokio::time::sleep(Duration::from_millis(30)).await;
            socket.send_to(b"42\r\n",peer).await.unwrap();assert_eq!(receive(&socket).await,(b"42\r\n".to_vec(),peer));
        });
        let url=Url::parse(&format!("guppy://127.0.0.1:{port}/document#part")).unwrap();
        let reply=tokio::time::timeout(Duration::from_secs(5),open(&url,false)).await.unwrap().unwrap();
        assert!(matches!(reply,gemini::Reply::Success(mime,bytes) if mime=="text/gemini" && bytes=="# 世界\nfinal\n".as_bytes()));
        server.await.unwrap();
    }
    #[tokio::test(flavor="current_thread")]
    async fn missing_eof_is_an_error_and_dropping_the_reader_releases_its_socket() {
        let socket=UdpSocket::bind("127.0.0.1:0").await.unwrap();let port=socket.local_addr().unwrap().port();
        let server=tokio::spawn(async move {let (_,peer)=receive(&socket).await;socket.send_to(b"6 text/plain\r\npartial",peer).await.unwrap();let _=receive(&socket).await;tokio::time::sleep(Duration::from_millis(150)).await;});
        let source=client(port).await;let weak=Arc::downgrade(&source);
        let mut session=match start(source,b"guppy://localhost/file\r\n").await {Ok(Start::Data(_,session))=>session,_=>panic!("Expected data")};
        session.deadline=Instant::now()+Duration::from_millis(30);
        let mut reader=Reader {session:Some(session),next:None,bytes:Vec::new(),position:0,done:false};
        assert!(reader.read_to_end(&mut Vec::new()).await.is_err());drop(reader);assert!(weak.upgrade().is_none());server.await.unwrap();
    }
    #[tokio::test(flavor="current_thread")]
    async fn a_full_reorder_cache_always_leaves_room_for_the_next_packet() {
        let server=UdpSocket::bind("127.0.0.1:0").await.unwrap();let socket=client(server.local_addr().unwrap().port()).await;
        let mut session=Session {socket,first:6,next:6,mime:"text/plain".into(),queue:BTreeMap::new(),bytes:0,last_ack:6,deadline:Instant::now()+IDLE,retry_at:Instant::now()+RETRY,buffer:vec![0;MAX_PACKET],end:Arc::new(Mutex::new(None)),finished:false};
        for seq in 7..39 {session.accept(Packet::Chunk(seq,vec![b'x';65000])).await.unwrap();}
        session.accept(Packet::Chunk(39,vec![b'x';65000])).await.unwrap();assert_eq!(session.queue.len(),REORDER_PACKETS);
        session.accept(Packet::Chunk(6,b"first".to_vec())).await.unwrap();assert_eq!(session.queue.len(),REORDER_PACKETS+1);
        assert!(session.bytes<=REORDER_BYTES+MAX_PACKET);
        assert_eq!(session.next_one().await.unwrap().unwrap(),b"first");
        assert_eq!(session.queue.len(),REORDER_PACKETS);
    }
}
