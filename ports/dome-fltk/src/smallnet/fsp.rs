// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only FSP v2. Author protocol: FSP project doc/PROTOCOL, version 0.19.
use super::{Error,MAX_BYTES,normalize_address,transfer};
use std::{future::Future,io::{self,Read,Seek,SeekFrom,Write},net::SocketAddr,os::{fd::AsRawFd,unix::fs::{OpenOptionsExt,MetadataExt}},pin::Pin,sync::Arc,task::{Context,Poll},time::Duration};
use tokio::{io::{AsyncRead,AsyncReadExt,ReadBuf},net::UdpSocket,time::Instant};
use url::Url;

const BLOCK:usize=1024;
const DIRECTORY:u8=0x41;
const FILE:u8=0x42;
const FLAGS:u8=0x47;
const BYE:u8=0x4a;
const STAT:u8=0x4d;
const FAILURE:u8=0x40;
const IDLE:Duration=Duration::from_secs(20);

pub(super) struct Target {pub url:Url,path:Vec<u8>,pub password:Option<Vec<u8>>}
fn decode(text:&str)->Result<Vec<u8>,Error> {
    let raw=text.as_bytes();
    for (i,b) in raw.iter().enumerate(){if *b==b'%' && (i+2>=raw.len() || !raw[i+1..i+3].iter().all(u8::is_ascii_hexdigit)){return Err(Error::Address);}}
    let bytes:Vec<_>=percent_encoding::percent_decode_str(text).collect();
    if bytes.iter().any(|b|*b<32 || *b==127){return Err(Error::Address);}Ok(bytes)
}
impl Target {
    pub(super) fn parse(url:&Url)->Result<Self,Error> {
        if url.scheme()!="fsp" || url.host_str().is_none() || url.query().is_some() || url.fragment().is_some() || url.as_str().len()>8192 || url.password().is_some(){return Err(Error::Address);}
        let password=if url.username().is_empty(){None}else{Some(decode(url.username())?)};
        let mut clean=url.clone();clean.set_username("").map_err(|_|Error::Address)?;clean.set_password(None).map_err(|_|Error::Address)?;normalize_address(&mut clean)?;
        if clean.path().is_empty(){clean.set_path("/");}
        let path=decode(clean.path())?;
        let target=Self {url:clean,path,password};target.argument(target.password.as_deref())?;Ok(target)
    }
    fn argument(&self,password:Option<&[u8]>)->Result<Vec<u8>,Error> {
        let mut bytes=self.path.clone();
        if let Some(password)=password {if password.iter().any(|b|*b<32 || *b==127){return Err(Error::Address);}bytes.push(b'\n');bytes.extend_from_slice(password);}
        bytes.push(0);if bytes.len()+2>BLOCK{return Err(Error::Address);}Ok(bytes)
    }
    pub(super) fn filename(&self)->String {String::from_utf8_lossy(&self.path).rsplit('/').next().unwrap_or("download").chars().filter(|c|!c.is_control() && *c!='\\').take(128).collect()}
}

#[derive(Debug)]
struct Packet {command:u8,key:u16,sequence:u16,position:u32,data:Vec<u8>,extra:Vec<u8>}
fn checksum(bytes:&[u8],client:bool)->u8 {
    let sum=bytes.iter().enumerate().filter(|(i,_)|*i!=1).fold(if client{bytes.len()as u32}else{0},|sum,(_,b)|sum+u32::from(*b));
    (sum+(sum>>8))as u8
}
fn encode(command:u8,key:u16,sequence:u16,position:u32,data:&[u8],extra:&[u8])->Result<Vec<u8>,Error> {
    if data.len()+extra.len()>BLOCK{return Err(Error::Address);}
    let mut bytes=vec![command,0];bytes.extend_from_slice(&key.to_be_bytes());bytes.extend_from_slice(&sequence.to_be_bytes());bytes.extend_from_slice(&(data.len()as u16).to_be_bytes());bytes.extend_from_slice(&position.to_be_bytes());bytes.extend_from_slice(data);bytes.extend_from_slice(extra);bytes[1]=checksum(&bytes,true);Ok(bytes)
}
fn packet(bytes:&[u8])->Result<Packet,Error> {
    if bytes.len()<12 || bytes.len()>BLOCK+12 || bytes[1]!=checksum(bytes,false){return Err(Error::Response);}
    let length=u16::from_be_bytes([bytes[6],bytes[7]])as usize;if length>bytes.len()-12{return Err(Error::Response);}
    Ok(Packet {command:bytes[0],key:u16::from_be_bytes([bytes[2],bytes[3]]),sequence:u16::from_be_bytes([bytes[4],bytes[5]]),position:u32::from_be_bytes(bytes[8..12].try_into().unwrap()),data:bytes[12..12+length].to_vec(),extra:bytes[12+length..].to_vec()})
}

// FSP keys belong to a peer IP/port, rather than an individual UDP socket.
// A per-user advisory lock coordinates tabs and browser processes. It remains
// held through completion/cancellation cleanup; keys never enter browser state.
struct Lease {file:std::fs::File,key:u16}
impl Lease {
    async fn acquire(peer:SocketAddr)->Result<Self,Error> {
        let uid=unsafe{libc::geteuid()};let parent=std::path::PathBuf::from(format!("/run/user/{uid}"));
        let parent=if parent.is_dir(){parent}else{std::env::temp_dir()};
        let directory=parent.join(format!("dome-fsp-{uid}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&directory){Ok(())=>{},Err(e)if e.kind()==io::ErrorKind::AlreadyExists=>{},Err(_)=>return Err(Error::Transport)}
        let info=std::fs::symlink_metadata(&directory).map_err(|_|Error::Transport)?;
        if !info.is_dir() || info.uid()!=uid || info.mode()&0o077!=0{return Err(Error::Transport);}
        let path=directory.join(peer.to_string().replace([':', '[', ']'],"_"));
        let mut file=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).custom_flags(libc::O_NOFOLLOW|libc::O_NONBLOCK).open(path).map_err(|_|Error::Transport)?;
        let info=file.metadata().map_err(|_|Error::Transport)?;if !info.is_file() || info.uid()!=uid || info.mode()&0o077!=0{return Err(Error::Transport);}
        loop {if unsafe{libc::flock(file.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)}==0{break;}let e=io::Error::last_os_error();if e.kind()!=io::ErrorKind::WouldBlock{return Err(Error::Transport);}tokio::time::sleep(Duration::from_millis(50)).await;}
        let mut bytes=[0;2];let key=if file.read_exact(&mut bytes).is_ok(){u16::from_be_bytes(bytes)}else{0};Ok(Self{file,key})
    }
    fn update(&mut self,key:u16)->Result<(),Error> {self.file.seek(SeekFrom::Start(0)).map_err(|_|Error::Transport)?;self.file.write_all(&key.to_be_bytes()).map_err(|_|Error::Transport)?;self.key=key;Ok(())}
}
use std::os::unix::fs::DirBuilderExt;
struct Session {socket:Arc<UdpSocket>,lease:Option<Lease>,sequence:u16,closed:bool}
impl Session {
    async fn connect(target:&Target,argument:&[u8])->Result<(Self,Packet),Error> {
        let host=target.url.host_str().ok_or(Error::Address)?.trim_matches(['[',']']);
        let peers:Vec<_>=tokio::net::lookup_host((host,target.url.port().unwrap_or(21))).await.map_err(|_|Error::Transport)?.take(8).collect();
        let mut error=Error::Transport;
        for peer in peers {
            let mut session=match Self::peer(peer).await{Ok(s)=>s,Err(e)=>{error=e;continue;}};
            match tokio::time::timeout(Duration::from_secs(4),session.exchange(STAT,0,argument,&[])).await {
                Ok(Ok(reply))=>return Ok((session,reply)),
                Ok(Err(e))=>error=e,Err(_)=>error=Error::Timeout,
            }
        }
        Err(error)
    }
    async fn peer(peer:SocketAddr)->Result<Self,Error> {
        let lease=Lease::acquire(peer).await?;
        let socket=UdpSocket::bind(if peer.is_ipv4(){"0.0.0.0:0"}else{"[::]:0"}).await.map_err(|_|Error::Transport)?;socket.connect(peer).await.map_err(|_|Error::Transport)?;
        let random=uuid::Uuid::new_v4();let sequence=u16::from_be_bytes(random.as_bytes()[..2].try_into().unwrap());
        Ok(Self {socket:Arc::new(socket),lease:Some(lease),sequence,closed:false})
    }
    async fn exchange(&mut self,command:u8,position:u32,data:&[u8],extra:&[u8])->Result<Packet,Error> {
        self.sequence=self.sequence.wrapping_add(1);let sequence=self.sequence;
        let bytes=encode(command,self.lease.as_ref().ok_or(Error::Transport)?.key,sequence,position,data,extra)?;
        let deadline=Instant::now()+IDLE;let mut delay=Duration::from_millis(1340);let mut buffer=vec![0;65536];let mut invalid=0usize;
        loop {
            if Instant::now()>=deadline{return Err(Error::Timeout);}
            if self.socket.send(&bytes).await.map_err(|_|Error::Transport)?!=bytes.len(){return Err(Error::Transport);}
            let until=(Instant::now()+delay).min(deadline);
            loop {
                if Instant::now()>=until {break;}
                let count=match tokio::time::timeout_at(until,self.socket.recv(&mut buffer)).await{Ok(Ok(n))=>n,Ok(Err(_))=>return Err(Error::Transport),Err(_)=>break};
                let reply=match packet(&buffer[..count]){Ok(p)=>p,Err(_)=>{invalid+=1;if invalid%64==0{tokio::task::yield_now().await;}continue;}};
                if reply.sequence!=sequence || !matches!(reply.command,c if c==command || c==FAILURE) || (matches!(command,DIRECTORY|FILE) && reply.command!=FAILURE && reply.position!=position){continue;}
                self.lease.as_mut().unwrap().update(reply.key)?;return Ok(reply);
            }
            delay=(delay.mul_f64(1.5)).min(Duration::from_secs(5));
        }
    }
    async fn finish(&mut self) {
        let _=tokio::time::timeout(Duration::from_secs(5),self.exchange(BYE,0,&[],&[])).await;self.closed=true;
    }
}
impl Drop for Session {
    fn drop(&mut self){
        if self.closed{return;}
        if let (Some(lease),Ok(runtime))=(self.lease.take(),tokio::runtime::Handle::try_current()){
            let mut session=Self{socket:self.socket.clone(),lease:Some(lease),sequence:self.sequence,closed:true};
            runtime.spawn(async move {session.finish().await;});
        }
    }
}

#[derive(Clone,Debug,PartialEq)]
struct Stat {time:u32,size:u32,kind:u8}
fn stat(bytes:&[u8])->Result<Stat,Error>{if bytes.len()!=9{return Err(Error::Response);}let kind=bytes[8];if !matches!(kind,0..=2){return Err(Error::Response);}Ok(Stat{time:u32::from_be_bytes(bytes[..4].try_into().unwrap()),size:u32::from_be_bytes(bytes[4..8].try_into().unwrap()),kind})}
#[derive(Debug)]
pub(super) struct Entry {pub name:Vec<u8>,pub target:Option<String>,pub size:u32,pub time:u32,pub directory:bool}
fn directory(bytes:&[u8],entries:&mut Vec<Entry>)->Result<bool,Error>{
    let mut offset=0;
    while offset+9<=bytes.len(){
        let info=stat(&bytes[offset..offset+9]);let kind=bytes[offset+8];if kind==0{return Ok(true);}if kind==0x2a{return Ok(false);}let info=info?;
        let tail=&bytes[offset+9..];let n=tail.iter().position(|b|*b==0).ok_or(Error::Response)?;
        let (name,target)=match tail[..n].iter().position(|b|*b==b'\n'){Some(i)=>(&tail[..i],Some(String::from_utf8_lossy(&tail[i+1..n]).into_owned())),None=>(&tail[..n],None)};
        if name.is_empty() || name.iter().any(|b|*b<32 || *b==127 || *b==b'/'){return Err(Error::Response);}
        if name!=b"." && name!=b".." {if entries.len()>=16384{return Err(Error::TooLarge);}entries.push(Entry{name:name.to_vec(),target,size:info.size,time:info.time,directory:info.kind==2});}
        offset=(offset+9+n+1+3)&!3;if offset>bytes.len(){return Err(Error::Response);}
    }
    Ok(false)
}
pub(super) enum Reply {Directory{url:Url,entries:Vec<Entry>,readme:String,flags:u8},File{mime:String,bytes:Vec<u8>},Stream(transfer::Transfer),Failure(String)}
fn failure(reply:&Packet)->Option<String>{(reply.command==FAILURE).then(||String::from_utf8_lossy(reply.data.strip_suffix(&[0]).unwrap_or(&reply.data)).into_owned())}

struct FileReader {session:Session,argument:Vec<u8>,expected:Stat,position:u64}
impl FileReader {
    async fn batch(&mut self)->Result<Option<Vec<u8>>,Error>{
        if self.position==u64::from(self.expected.size){
            let reply=self.session.exchange(STAT,0,&self.argument,&[]).await?;
            if reply.command!=STAT || !reply.extra.is_empty() || stat(&reply.data)?!=self.expected{return Err(Error::Response);}
            self.session.finish().await;return Ok(None);
        }
        let mut bytes=Vec::new();
        while bytes.len()<64*1024 && self.position<u64::from(self.expected.size){
            let reply=self.session.exchange(FILE,self.position.try_into().map_err(|_|Error::TooLarge)?,&self.argument,&(BLOCK as u16).to_be_bytes()).await?;
            if reply.command!=FILE || !reply.extra.is_empty() || reply.data.is_empty() || self.position+reply.data.len()as u64>u64::from(self.expected.size){return Err(Error::Response);}
            self.position+=reply.data.len()as u64;bytes.extend_from_slice(&reply.data);
        }
        Ok(Some(bytes))
    }
}
type Next=Pin<Box<dyn Future<Output=(FileReader,Result<Option<Vec<u8>>,Error>)>+Send>>;
struct Reader {file:Option<FileReader>,next:Option<Next>,bytes:Vec<u8>,offset:usize,done:bool}
impl AsyncRead for Reader {
    fn poll_read(mut self:Pin<&mut Self>,cx:&mut Context<'_>,target:&mut ReadBuf<'_>)->Poll<io::Result<()>>{
        if target.remaining()==0{return Poll::Ready(Ok(()));}
        if self.offset<self.bytes.len(){let n=target.remaining().min(self.bytes.len()-self.offset);target.put_slice(&self.bytes[self.offset..self.offset+n]);self.offset+=n;return Poll::Ready(Ok(()));}
        if self.done{return Poll::Ready(Ok(()));}
        if self.next.is_none(){let mut file=self.file.take().expect("FSP reader owns its session");self.next=Some(Box::pin(async move{let next=file.batch().await;(file,next)}));}
        match self.next.as_mut().unwrap().as_mut().poll(cx){
            Poll::Pending=>Poll::Pending,
            Poll::Ready((file,result))=>{
                self.next=None;self.file=Some(file);
                match result{
                    Ok(Some(bytes))=>{let n=target.remaining().min(bytes.len());target.put_slice(&bytes[..n]);self.bytes=bytes;self.offset=n;Poll::Ready(Ok(()))},
                    Ok(None)=>{self.done=true;self.file=None;Poll::Ready(Ok(()))},
                    Err(error)=>{self.done=true;self.file=None;Poll::Ready(Err(io::Error::new(if error==Error::Timeout{io::ErrorKind::TimedOut}else{io::ErrorKind::InvalidData},error.message())))},
                }
            },
        }
    }
}

pub(super) async fn open(target:&Target,password:Option<&[u8]>,download:bool)->Result<Reply,Error>{
    let argument=target.argument(password)?;let (mut session,reply)=Session::connect(target,&argument).await?;
    if let Some(message)=failure(&reply){session.finish().await;return Ok(Reply::Failure(message));}
    if !reply.extra.is_empty(){return Err(Error::Response);}let info=stat(&reply.data)?;
    if info.kind==0{session.finish().await;return Ok(Reply::Failure("The resource is unavailable or requires a password.".into()));}
    if info.kind==2{
        let protection=session.exchange(FLAGS,0,&argument,&(BLOCK as u16).to_be_bytes()).await?;
        if let Some(message)=failure(&protection){session.finish().await;return Ok(Reply::Failure(message));}
        if protection.position!=1 || protection.extra.len()!=1{return Err(Error::Response);}
        let readme=String::from_utf8_lossy(protection.data.strip_suffix(&[0]).unwrap_or(&protection.data)).into_owned();let flags=protection.extra[0];
        let mut entries=Vec::new();let mut position=0u32;let mut block=None;
        loop{
            let reply=session.exchange(DIRECTORY,position,&argument,&(BLOCK as u16).to_be_bytes()).await?;
            if let Some(message)=failure(&reply){session.finish().await;return Ok(Reply::Failure(message));}
            if !reply.extra.is_empty() || reply.data.is_empty(){return Err(Error::Response);}
            let size=*block.get_or_insert(reply.data.len());if reply.data.len()>size{return Err(Error::Response);}
            if directory(&reply.data,&mut entries)?{break;}
            if reply.data.len()<size{return Err(Error::Response);}
            position=position.checked_add(reply.data.len()as u32).ok_or(Error::TooLarge)?;if position as usize>MAX_BYTES{return Err(Error::TooLarge);}
        }
        session.finish().await;let mut url=target.url.clone();if !url.path().ends_with('/'){url.set_path(&format!("{}/",url.path()));}
        return Ok(Reply::Directory{url,entries,readme,flags});
    }
    // STAT may expose metadata even when reading requires an account. Query
    // the parent directory before opening a file body or destination review.
    let end=target.path.iter().rposition(|b|*b==b'/').ok_or(Error::Address)?+1;
    let parent=Target {url:target.url.clone(),path:target.path[..end].to_vec(),password:None};
    let protection=session.exchange(FLAGS,0,&parent.argument(password)?,&(BLOCK as u16).to_be_bytes()).await?;
    if let Some(message)=failure(&protection){session.finish().await;return Ok(Reply::Failure(message));}
    if protection.position!=1 || protection.extra.len()!=1{return Err(Error::Response);}
    if protection.extra[0]&0x11==0x10{session.finish().await;return Ok(Reply::Failure("File reading is restricted in this directory.".into()));}
    let mime=mime_guess::from_path(target.filename()).first_or_octet_stream().to_string();let kind:mime::Mime=mime.parse().map_err(|_|Error::Response)?;
    let reader=Reader{file:Some(FileReader{session,argument,expected:info.clone(),position:0}),next:None,bytes:Vec::new(),offset:0,done:false};
    if download || info.size as usize>MAX_BYTES || !(kind.type_()==mime::TEXT || kind.type_()==mime::IMAGE || kind.essence_str()=="application/xhtml+xml"){
        return Ok(Reply::Stream(transfer::Transfer::new(mime,Some(u64::from(info.size)),reader)));
    }
    let reader=reader;let mut bytes=Vec::new();reader.take((MAX_BYTES+1)as u64).read_to_end(&mut bytes).await.map_err(|_|Error::Transport)?;if bytes.len()>MAX_BYTES{return Err(Error::TooLarge);}Ok(Reply::File{mime,bytes})
}

pub(super) fn document(url:&Url,base:&Url,entries:&[Entry],readme:&str,flags:u8,account:Option<&Url>)->String{
    use super::document::{escape,page_with_metadata};
    let mut body="<h1>FSP archive</h1><nav>".to_owned();if base.path()!="/"{if let Ok(parent)=base.join("../"){body.push_str(&format!("<a href=\"{}\" rel=noreferrer>Parent directory</a> · ",escape(parent.as_str())));}}
    if let Some(account)=account{body.push_str(&format!("<a href=\"{}\" rel=noreferrer>Use password for this load</a>",escape(account.as_str())));}body.push_str("</nav>");
    if !readme.is_empty(){body.push_str(&format!("<pre>{}</pre>",escape(readme)));}body.push_str("<p>Browser access is read only.</p>");if flags&0x10!=0{body.push_str("<p>This directory restricts file reading.</p>");}body.push_str("<ul>");
    for entry in entries{
        let name=String::from_utf8_lossy(&entry.name);let component=percent_encoding::percent_encode(&entry.name,percent_encoding::NON_ALPHANUMERIC).to_string();let mut target=base.clone();target.set_path(&format!("{}{}{}",base.path(),component,if entry.directory{"/"}else{""}));
        body.push_str(&format!("<li><a href=\"{}\" rel=noreferrer>{}{}</a> · {} bytes · modified {}",escape(target.as_str()),escape(&name),if entry.directory{"/"}else{""},entry.size,entry.time));
        if let Some(target)=&entry.target{body.push_str(&format!(" · → {}",escape(target)));}body.push_str("</li>");
    }
    body.push_str("</ul>");page_with_metadata(url,&body,None,Some("FSP archive"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fsp_uri_uses_transient_password_userinfo_and_has_no_query_or_anchor(){
        let target=Target::parse(&Url::parse("fsp://p%3Aass@example.org:21/folder/read%20me.txt").unwrap()).unwrap();
        assert_eq!(target.password.as_deref(),Some(b"p:ass".as_slice()));assert_eq!(target.url.as_str(),"fsp://example.org:21/folder/read%20me.txt");assert_eq!(target.argument(target.password.as_deref()).unwrap(),b"/folder/read me.txt\np:ass\0");
        for uri in ["fsp://example.org/file?query","fsp://example.org/file#anchor","fsp://name:password@example.org/","fsp://example.org/%00file","fsp://example.org/%0afile","fsp://example.org/%zz"]{assert!(Target::parse(&Url::parse(uri).unwrap()).is_err(),"{uri}");}
        assert_eq!(super::super::persistent_url(&Url::parse("fsp://password@example.org/").unwrap()).unwrap().as_str(),"fsp://example.org/");
    }
    #[test]
    fn fsp_headers_validate_checksum_length_and_extra_data(){
        let request=encode(STAT,0x1234,0xabcd,0,b"/\0",&[]).unwrap();assert_eq!(request.len(),14);assert_eq!(request[1],0x4c);
        let mut response=vec![FLAGS,0,0x12,0x34,0xab,0xcd,0,1,0,0,0,1,0,0x40];response[1]=checksum(&response,false);
        let parsed=packet(&response).unwrap();assert_eq!(parsed.extra,[0x40]);assert_eq!(parsed.data,[0]);assert_eq!(parsed.position,1);
        response[7]=3;response[1]=checksum(&response,false);assert!(packet(&response).is_err());response.truncate(11);assert!(packet(&response).is_err());
    }
    fn entry(kind:u8,name:&[u8])->Vec<u8>{let mut bytes=vec![0,0,0,1,0,0,0,7,kind];bytes.extend_from_slice(name);bytes.push(0);while bytes.len()%4!=0{bytes.push(0);}bytes}
    #[test]
    fn fsp_directory_keeps_names_links_and_complete_blocks(){
        let bytes=[entry(1,b"a <&>.txt"),entry(2,b"alias\ntarget"),vec![0;9]].concat();let mut entries=Vec::new();assert!(directory(&bytes,&mut entries).unwrap());assert_eq!(entries.len(),2);assert_eq!(entries[1].target.as_deref(),Some("target"));
        let root=Url::parse("fsp://example.org/folder/").unwrap();let html=document(&root,&root,&entries,"<readme>",0x40,None);assert!(html.contains("a &lt;&amp;&gt;.txt"));assert!(html.contains("&lt;readme&gt;"));assert!(html.contains("fsp://example.org/folder/a%20%3C%26%3E%2Etxt"));
        let mut skip=vec![0;9];skip[8]=0x2a;assert!(!directory(&skip,&mut Vec::new()).unwrap());assert!(directory(&entry(1,b"bad/name"),&mut Vec::new()).is_err());assert!(directory(&[0,0,0,1,0,0,0,7,1,b'x'],&mut Vec::new()).is_err());
    }
    async fn fixture(payload:Vec<u8>,changed:bool,retry:bool)->(u16,tokio::task::JoinHandle<Vec<u8>>){
        let socket=UdpSocket::bind("127.0.0.1:0").await.unwrap();let port=socket.local_addr().unwrap().port();
        let task=tokio::spawn(async move{
            let mut commands=Vec::new();let mut bytes=vec![0;65536];let mut key=None;let mut stats=0;let mut lost=None;
            loop{
                let(n,peer)=tokio::time::timeout(Duration::from_secs(10),socket.recv_from(&mut bytes)).await.unwrap().unwrap();let wire=&bytes[..n];assert_eq!(wire[1],checksum(wire,true));
                let command=wire[0];let sequence=u16::from_be_bytes([wire[4],wire[5]]);let position=u32::from_be_bytes(wire[8..12].try_into().unwrap())as usize;let length=u16::from_be_bytes([wire[6],wire[7]])as usize;
                if retry && lost.is_none(){lost=Some(Instant::now());continue;}if retry && commands.is_empty(){assert!(lost.unwrap().elapsed()>=Duration::from_secs(1));}
                if let Some(key)=key{assert_eq!(u16::from_be_bytes([wire[2],wire[3]]),key);}commands.push(command);
                if command!=BYE{assert_eq!(&wire[12..12+length],if command==FLAGS{b"/\nfixture\0".as_slice()}else{b"/file.txt\nfixture\0".as_slice()});}
                let data=match command{
                    STAT=>{stats+=1;let mut data=vec![0,0,0,if changed && stats>1{2}else{1}];data.extend_from_slice(&(payload.len()as u32).to_be_bytes());data.push(1);data},
                    FILE=>payload[position..(position+BLOCK).min(payload.len())].to_vec(),
                    FLAGS=>vec![0],
                    BYE=>Vec::new(),_=>panic!("Unexpected browser command"),
                };
                let next_key=0x2100u16.wrapping_add(commands.len()as u16);key=Some(next_key);
                let mut response=vec![command,0];response.extend_from_slice(&next_key.to_be_bytes());response.extend_from_slice(&sequence.to_be_bytes());response.extend_from_slice(&(data.len()as u16).to_be_bytes());response.extend_from_slice(&(if command==FLAGS{1}else{position as u32}).to_be_bytes());response.extend_from_slice(&data);if command==FLAGS{response.push(0x40);}response[1]=checksum(&response,false);
                // A stale reply precedes the valid response; it must not advance
                // the current operation or its rotating session key.
                if command==FILE{let mut stale=response.clone();stale[5]=stale[5].wrapping_sub(1);stale[1]=checksum(&stale,false);socket.send_to(&stale,peer).await.unwrap();}
                socket.send_to(&response,peer).await.unwrap();if command==BYE{break;}
            }
            commands
        });(port,task)
    }
    #[tokio::test(flavor="current_thread")]
    async fn fsp_file_transport_rotates_keys_retries_and_verifies_final_stat(){
        for changed in [false,true]{
            let payload=[b"FSP exact source\r\n".as_slice(),&vec![b'x';128*1024+7],b"\r\n"].concat();let expected=payload.clone();let(port,server)=fixture(payload,changed,!changed).await;
            let target=Target::parse(&Url::parse(&format!("fsp://localhost:{port}/file.txt")).unwrap()).unwrap();
            let result=open(&target,Some(b"fixture"),false).await;
            if changed{assert!(result.is_err());}else{let Reply::File{bytes,..}=result.unwrap()else{panic!("Expected preview")};assert_eq!(bytes,expected);}
            let commands=server.await.unwrap();assert_eq!(commands.first(),Some(&STAT));assert_eq!(commands.last(),Some(&BYE));assert!(commands.iter().all(|c|matches!(*c,STAT|FLAGS|FILE|BYE)));
        }
    }
    #[tokio::test(flavor="current_thread")]
    async fn fsp_cancelled_file_sends_bye_and_releases_the_peer_lease(){
        let socket=UdpSocket::bind("127.0.0.1:0").await.unwrap();let address=socket.local_addr().unwrap();let(started,receive)=tokio::sync::oneshot::channel();
        let server=tokio::spawn(async move{
            let mut started=Some(started);let mut buffer=vec![0;65536];let mut commands=Vec::new();
            loop{
                let(n,peer)=tokio::time::timeout(Duration::from_secs(6),socket.recv_from(&mut buffer)).await.unwrap().unwrap();let bytes=&buffer[..n];let command=bytes[0];commands.push(command);
                if command==FILE{let _=started.take().unwrap().send(());continue;}
                assert!(matches!(command,STAT|FLAGS|BYE));let data=if command==STAT{vec![0,0,0,1,0,0,16,0,1]}else if command==FLAGS{vec![0]}else{Vec::new()};
                let mut reply=encode(command,0x3456,u16::from_be_bytes([bytes[4],bytes[5]]),if command==FLAGS{1}else{0},&data,if command==FLAGS{&[0x40]}else{&[]}).unwrap();reply[1]=checksum(&reply,false);socket.send_to(&reply,peer).await.unwrap();if command==BYE{break;}
            }
            commands
        });
        let target=Target::parse(&Url::parse(&format!("fsp://{address}/file.txt")).unwrap()).unwrap();let load=tokio::spawn(async move{open(&target,None,false).await});
        tokio::time::timeout(Duration::from_secs(5),receive).await.unwrap().unwrap();load.abort();match load.await{Err(e)=>assert!(e.is_cancelled()),Ok(_)=>panic!("Pending FSP load was not cancelled")};
        assert_eq!(server.await.unwrap(),[STAT,FLAGS,FILE,BYE]);
        let lease=tokio::time::timeout(Duration::from_secs(1),Lease::acquire(address)).await.unwrap().unwrap();assert_eq!(lease.key,0x3456);
    }
}
