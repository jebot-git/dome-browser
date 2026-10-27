// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only FTP: RFC 959/1738, EPSV (2428), and MLSD/SIZE (3659).
use super::{Error, MAX_BYTES, normalize_address};
use std::net::SocketAddr;
use tokio::{io::{AsyncReadExt, AsyncWriteExt, BufReader}, net::TcpStream};
use url::Url;

const LINE_LIMIT: usize = 8192;
const REPLY_LIMIT: usize = 65536;

#[derive(Clone, Default)]
pub(super) struct Credentials { pub user: String, pub password: Option<String>, pub account: String }
pub(super) struct Target {
    pub url: Url,
    pub credentials: Option<Credentials>,
    directories: Vec<Vec<u8>>,
    name: Vec<u8>,
    directory: bool,
    ascii: bool,
    names: bool,
    listing_path: String,
}
fn argument(bytes: &[u8]) -> Result<(), Error> {
    if bytes.len()>LINE_LIMIT || bytes.iter().any(|b| *b<32 || *b==127) { return Err(Error::Address); }
    Ok(())
}
fn decode(text: &str) -> Result<Vec<u8>, Error> {
    let bytes: Vec<_> = percent_encoding::percent_decode_str(text).collect();argument(&bytes)?;Ok(bytes)
}
impl Target {
    pub(super) fn filename(&self) -> String {
        String::from_utf8_lossy(&self.name).rsplit(['/', '\\']).next().unwrap_or("download")
            .chars().filter(|c|!c.is_control()).take(128).collect()
    }
    pub(super) fn parse(url: &Url) -> Result<Self, Error> {
        if url.scheme()!="ftp" || url.host_str().is_none() || url.query().is_some() || url.as_str().len()>65536 { return Err(Error::Address); }
        let user=percent_encoding::percent_decode_str(url.username()).decode_utf8().map_err(|_| Error::Address)?.into_owned();
        let password=url.password().map(|value|percent_encoding::percent_decode_str(value).decode_utf8().map(|s|s.into_owned()).map_err(|_|Error::Address)).transpose()?;
        argument(user.as_bytes())?;if let Some(password)=&password {argument(password.as_bytes())?;}
        let credentials=if user.is_empty() && password.is_none() {None} else {Some(Credentials {user:if user.is_empty(){"anonymous".into()}else{user},password,account:String::new()})};
        let mut clean=url.clone();normalize_address(&mut clean)?;
        clean.set_username("").map_err(|_|Error::Address)?;clean.set_password(None).map_err(|_|Error::Address)?;
        let mut path=clean.path().to_owned();
        let kind=if path.len()>=7 && path.as_bytes()[path.len()-7..path.len()-1].eq_ignore_ascii_case(b";type=") {
            let kind=path.as_bytes()[path.len()-1].to_ascii_lowercase();path.truncate(path.len()-7);Some(kind)
        }else{None};
        if kind.is_some_and(|kind|!matches!(kind,b'a'|b'i'|b'd')) {return Err(Error::Unsupported);}
        let directory=kind==Some(b'd') || path.ends_with('/') || path.is_empty();
        let mut parts: Vec<_> = path.strip_prefix('/').ok_or(Error::Address)?.split('/').map(decode).collect::<Result<_,_>>()?;
        let names=kind==Some(b'd');
        let name=if directory && !names {parts.pop();Vec::new()}else{parts.pop().unwrap_or_default()};
        // Keep empty intermediate components: RFC 1738 assigns them an empty
        // CWD argument. Encoded slashes remain within one server pathname.
        let listing_path=if names {format!("{}/",path.rsplit_once('/').map(|(parent,_)|parent).unwrap_or(""))}
            else if directory {path}else{format!("{path}/")};
        Ok(Self {url:clean,credentials,directories:parts,name,directory,ascii:kind==Some(b'a'),names,listing_path})
    }
}

#[derive(Debug)]
struct Status { code: u16, text: String }
impl Status {
    fn failure(self) -> Reply {Reply::Failure(self.code,self.text)}
    fn unsupported(&self) -> bool {matches!(self.code,500|501|502|504)}
}
struct Session { control: BufReader<TcpStream>, peer: SocketAddr }
impl Session {
    async fn octet(&mut self) -> Result<u8, Error> {
        loop {
            let byte=self.control.read_u8().await.map_err(|_|Error::Transport)?;
            if byte!=255 {return Ok(byte);}
            let command=self.control.read_u8().await.map_err(|_|Error::Transport)?;
            match command {
                255=>return Ok(255),
                251..=254=>{
                    let option=self.control.read_u8().await.map_err(|_|Error::Transport)?;
                    // FTP control uses Telnet; decline optional negotiation.
                    if command==251 || command==253 {
                        self.control.get_mut().write_all(&[255,if command==251{254}else{252},option]).await.map_err(|_|Error::Transport)?;
                    }
                },
                _=>return Err(Error::Response),
            }
        }
    }
    async fn line(&mut self) -> Result<Vec<u8>, Error> {
        let mut line=Vec::new();
        while line.len()<LINE_LIMIT {
            line.push(self.octet().await?);
            if line.ends_with(b"\r\n") {line.truncate(line.len()-2);return Ok(line);}
        }
        Err(Error::TooLarge)
    }
    async fn status(&mut self) -> Result<Status, Error> {
        let first=self.line().await?;
        if first.len()<4 || !first[..3].iter().all(u8::is_ascii_digit) || !matches!(first[3],b' '|b'-') {return Err(Error::Response);}
        let code=std::str::from_utf8(&first[..3]).unwrap().parse::<u16>().map_err(|_|Error::Response)?;
        if !(100..600).contains(&code) {return Err(Error::Response);}
        let mut text=first[4..].to_vec();
        if first[3]==b'-' {
            let end=[first[0],first[1],first[2],b' '];
            let mut finished=false;
            for _ in 0..128 {
                let line=self.line().await?;
                if text.len()+line.len()+1>REPLY_LIMIT {return Err(Error::TooLarge);}
                text.push(b'\n');text.extend_from_slice(if line.starts_with(&end){&line[4..]}else{&line});
                if line.starts_with(&end) {finished=true;break;}
            }
            if !finished {return Err(Error::TooLarge);}
        }
        Ok(Status {code,text:String::from_utf8_lossy(&text).chars().filter(|c|!c.is_control() || matches!(c,'\n'|'\t')).collect()})
    }
    async fn command(&mut self, verb: &str, value: Option<&[u8]>) -> Result<Status, Error> {
        let mut bytes=verb.as_bytes().to_vec();
        if let Some(value)=value {
            argument(value)?;bytes.push(b' ');
            for byte in value {bytes.push(*byte);if *byte==255{bytes.push(255);}}
        }
        bytes.extend_from_slice(b"\r\n");
        self.control.get_mut().write_all(&bytes).await.map_err(|_|Error::Transport)?;
        self.status().await
    }
    async fn data(&mut self) -> Result<Result<TcpStream,Status>,Error> {
        let extended=self.command("EPSV",None).await?;
        let port=if extended.code==229 {extended_port(&extended.text)?}
        else if (extended.unsupported() || extended.code==522) && self.peer.is_ipv4() {
            let passive=self.command("PASV",None).await?;
            if passive.code!=227 {return Ok(Err(passive));}
            passive_port(&passive.text)?
        } else {return Ok(Err(extended));};
        if port==0 || servo::protocol_handler::is_bad_port(port) {return Err(Error::Address);}
        let endpoint=SocketAddr::new(self.peer.ip(),port);
        Ok(Ok(TcpStream::connect(endpoint).await.map_err(|_|Error::Transport)?))
    }
    async fn begin_transfer(&mut self,verb:&str,name:Option<&[u8]>)->Result<Result<TcpStream,Status>,Error> {
        let data=match self.data().await? {Ok(data)=>data,Err(reply)=>return Ok(Err(reply))};
        let start=self.command(verb,name).await?;
        if !matches!(start.code,125|150){return Ok(Err(start));}
        Ok(Ok(data))
    }
    async fn transfer(&mut self, verb: &str, name: Option<&[u8]>, size: Option<usize>) -> Result<Result<Vec<u8>,Status>,Error> {
        let data=match self.begin_transfer(verb,name).await? {Ok(data)=>data,Err(reply)=>return Ok(Err(reply))};
        let mut bytes=Vec::new();
        data.take((MAX_BYTES+1) as u64).read_to_end(&mut bytes).await.map_err(|_|Error::Transport)?;
        if bytes.len()>MAX_BYTES {return Err(Error::TooLarge);}
        let done=self.status().await?;
        if !matches!(done.code,226|250) {return Ok(Err(done));}
        if size.is_some_and(|size|size!=bytes.len()) {return Err(Error::Response);}
        Ok(Ok(bytes))
    }
    async fn directory(&mut self,target:&Target) -> Result<Reply,Error> {
        let listing=if target.names {
            match self.transfer("NLST",if target.name.is_empty(){None}else{Some(&target.name)},None).await? {
                Ok(bytes)=>parse_listing(&bytes,false)?,Err(reply)=>return Ok(reply.failure()),
            }
        }else{match self.transfer("MLSD",None,None).await? {
            Ok(bytes)=>parse_listing(&bytes,true)?,
            Err(reply) if reply.unsupported() || reply.code==550=>match self.transfer("NLST",None,None).await? {
                Ok(bytes)=>parse_listing(&bytes,false)?,Err(reply)=>return Ok(reply.failure()),
            },
            Err(reply)=>return Ok(reply.failure()),
        }};
        let mut base=target.url.clone();base.set_path(&target.listing_path);base.set_fragment(None);
        Ok(Reply::Directory {base,entries:listing})
    }
}
fn parenthesized(text:&str) -> Result<&str,Error> {
    text.rsplit_once('(').and_then(|(_,rest)|rest.split_once(')').map(|(value,_)|value)).ok_or(Error::Response)
}
fn extended_port(text:&str) -> Result<u16,Error> {
    let text=parenthesized(text)?;
    let delimiter=text.as_bytes().first().copied().ok_or(Error::Response)?;
    if !(33..=126).contains(&delimiter) || !text.is_ascii() {return Err(Error::Response);}
    let fields:Vec<_>=text.split(char::from(delimiter)).collect();
    if fields.len()!=5 || fields[..3].iter().any(|s|!s.is_empty()) || !fields[4].is_empty() || fields[3].is_empty() || !fields[3].bytes().all(|b|b.is_ascii_digit()) {return Err(Error::Response);}
    fields[3].parse().map_err(|_|Error::Response)
}
fn passive_port(text:&str) -> Result<u16,Error> {
    let parts=parenthesized(text)?.split(',').map(|s|s.trim().parse::<u8>().map_err(|_|Error::Response)).collect::<Result<Vec<_>,_>>()?;
    if parts.len()!=6 {return Err(Error::Response);}
    // The connected control peer determines the data host, including when a
    // server advertises an unroutable address behind NAT.
    Ok(u16::from(parts[4])*256+u16::from(parts[5]))
}

pub(super) struct Entry {pub name:Vec<u8>,pub directory:bool,pub size:Option<u64>,pub modified:Option<String>}
fn parse_listing(bytes:&[u8],facts:bool) -> Result<Vec<Entry>,Error> {
    let mut entries=Vec::new();
    for raw in bytes.split(|b|*b==b'\n') {
        let line=raw.strip_suffix(b"\r").unwrap_or(raw);
        if line.is_empty() {continue;}
        let mut directory=false;let mut size=None;let mut modified=None;
        let name=if facts {
            let split=line.iter().position(|b|*b==b' ').ok_or(Error::Response)?;
            let metadata=std::str::from_utf8(&line[..split]).map_err(|_|Error::Response)?;
            if !metadata.is_empty() && !metadata.ends_with(';') {return Err(Error::Response);}
            let mut skip=false;
            for fact in metadata.split(';').filter(|s|!s.is_empty()) {
                let (key,value)=fact.split_once('=').ok_or(Error::Response)?;
                if key.eq_ignore_ascii_case("type") {
                    directory=value.eq_ignore_ascii_case("dir");skip=value.eq_ignore_ascii_case("cdir") || value.eq_ignore_ascii_case("pdir");
                }else if key.eq_ignore_ascii_case("size") {size=value.parse::<u64>().ok();}
                else if key.eq_ignore_ascii_case("modify") && value.len()<=32 && value.bytes().all(|b|b.is_ascii_digit() || b==b'.') {modified=Some(value.to_owned());}
            }
            if skip {continue;}
            &line[split+1..]
        }else{line};
        if name.is_empty() || matches!(name,b"."|b"..") {continue;}
        argument(name)?;
        if entries.len()>=10000 {return Err(Error::TooLarge);}
        entries.push(Entry {name:name.to_vec(),directory,size,modified});
    }
    Ok(entries)
}
pub(super) enum Reply {Stream(super::transfer::Transfer),File {mime:String,bytes:Vec<u8>},Directory {base:Url,entries:Vec<Entry>},Authenticate {account:bool},Failure(u16,String)}

#[cfg(test)]
pub(super) async fn fetch(target:&Target,credentials:Option<&Credentials>)->Result<Reply,Error> {
    fetch_inner(target,credentials,None).await
}
pub(super) async fn open(target:&Target,credentials:Option<&Credentials>,download:bool)->Result<Reply,Error> {
    fetch_inner(target,credentials,Some(download)).await
}
async fn fetch_inner(target:&Target,credentials:Option<&Credentials>,download:Option<bool>) -> Result<Reply,Error> {
    let host=target.url.host_str().ok_or(Error::Address)?.trim_matches(['[',']']);
    let stream=TcpStream::connect((host,target.url.port().unwrap_or(21))).await.map_err(|_|Error::Transport)?;
    let peer=stream.peer_addr().map_err(|_|Error::Transport)?;
    let mut session=Session {control:BufReader::new(stream),peer};
    let mut greeting=session.status().await?;
    if greeting.code==120 {greeting=session.status().await?;}
    if greeting.code!=220 {return Ok(greeting.failure());}
    let anonymous=Credentials {user:"anonymous".into(),password:Some("anonymous@".into()),account:String::new()};
    let credentials=credentials.unwrap_or(&anonymous);
    let mut login=session.command("USER",Some(credentials.user.as_bytes())).await?;
    if login.code==331 {
        let Some(password)=&credentials.password else {return Ok(Reply::Authenticate {account:false});};
        login=session.command("PASS",Some(password.as_bytes())).await?;
    }
    if login.code==332 {
        if credentials.account.is_empty() {return Ok(Reply::Authenticate {account:true});}
        login=session.command("ACCT",Some(credentials.account.as_bytes())).await?;
    }
    if login.code==530 {return Ok(Reply::Authenticate {account:false});}
    if login.code!=230 {return Ok(login.failure());}
    let features=session.command("FEAT",None).await?;
    if features.code==211 && features.text.lines().any(|line|line.trim().eq_ignore_ascii_case("UTF8")) {
        let options=session.command("OPTS",Some(b"UTF8 ON")).await?;
        if !(200..300).contains(&options.code) && !options.unsupported() {return Ok(options.failure());}
    }
    let transfer_type=session.command("TYPE",Some(if target.ascii{b"A"}else{b"I"})).await?;
    if transfer_type.code!=200 {return Ok(transfer_type.failure());}
    for directory in &target.directories {
        let result=session.command("CWD",Some(directory)).await?;
        if result.code!=250 {return Ok(result.failure());}
    }
    let result=if target.directory {session.directory(target).await?}else{
        let mime=if target.ascii {"text/plain".into()}else{mime_guess::from_path(String::from_utf8_lossy(&target.name).as_ref()).first_or_octet_stream().to_string()};
        let kind:mime::Mime=mime.parse().map_err(|_|Error::Response)?;
        let stream_file=download.is_some_and(|download|download || !(kind.type_()==mime::TEXT || kind.type_()==mime::IMAGE || kind.essence_str()=="application/xhtml+xml"));
        let mut size=None;
        if !target.ascii {
            let response=session.command("SIZE",Some(&target.name)).await?;
            if response.code==213 {
                let length=response.text.trim().parse::<u64>().map_err(|_|Error::Response)?;
                if !stream_file && length>MAX_BYTES as u64 {return Err(Error::TooLarge);}
                size=Some(length);
            }
        }
        if stream_file {
            match session.begin_transfer("RETR",Some(&target.name)).await? {
                Ok(data)=>{
                    let completion=async move {
                        let status=session.status().await?;
                        if !matches!(status.code,226|250){return Err(Error::Response);}
                        let _=tokio::time::timeout(std::time::Duration::from_millis(500),session.command("QUIT",None)).await;
                        Ok(())
                    };
                    return Ok(Reply::Stream(super::transfer::Transfer::new(mime,size,data).with_completion(completion)));
                },
                Err(reply) if reply.code==550=>{
                    let cwd=session.command("CWD",Some(&target.name)).await?;
                    if cwd.code==250 {session.directory(target).await?}else{reply.failure()}
                },
                Err(reply)=>reply.failure(),
            }
        }else{match session.transfer("RETR",Some(&target.name),size.map(|size|size as usize)).await? {
            Ok(bytes)=>Reply::File {mime,bytes},
            Err(reply) if reply.code==550=>{
                let cwd=session.command("CWD",Some(&target.name)).await?;
                if cwd.code==250 {session.directory(target).await?}else{reply.failure()}
            },
            Err(reply)=>reply.failure(),
        }}
    };
    // A server that ignores QUIT must not turn a completed read into a failure.
    let _=tokio::time::timeout(std::time::Duration::from_millis(500),session.command("QUIT",None)).await;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ftp_paths_follow_url_component_and_type_rules() {
        let t=Target::parse(&Url::parse("ftp://user:password@host/%2Fpub/a%20b.txt;type=a#fragment").unwrap()).unwrap();
        assert_eq!(t.directories,[b"/pub".to_vec()]);assert_eq!(t.name,b"a b.txt");assert!(t.ascii);assert!(!t.directory);
        assert_eq!(t.url.username(),"");assert!(t.url.password().is_none());assert_eq!(t.credentials.unwrap().user,"user");
        for value in ["ftp://host/", "ftp://host/pub/", "ftp://host/pub;type=d"] {assert!(Target::parse(&Url::parse(value).unwrap()).unwrap().directory);}
        assert_eq!(Target::parse(&Url::parse("ftp://host/a%2Fb%3Btype%3Di").unwrap()).unwrap().name,b"a/b;type=i");
        let d=Target::parse(&Url::parse("ftp://host/parent/pattern;type=d").unwrap()).unwrap();
        assert_eq!(d.directories,[b"parent".to_vec()]);assert_eq!(d.name,b"pattern");assert!(d.names);assert_eq!(d.listing_path,"/parent/");
        let d=Target::parse(&Url::parse("ftp://host//etc/motd").unwrap()).unwrap();assert_eq!(d.directories,[b"".to_vec(),b"etc".to_vec()]);
        for value in ["ftp://host/a?query", "ftp://host/a;type=z", "ftp://host/a%00b"] {assert!(Target::parse(&Url::parse(value).unwrap()).is_err());}
    }
    #[test]
    fn passive_endpoints_and_machine_listings_are_parsed_losslessly() {
        assert_eq!(extended_port("Entering passive mode (|||45678|)").unwrap(),45678);
        assert_eq!(extended_port("Entering passive mode (!!!45678!)").unwrap(),45678);
        assert_eq!(passive_port("Passive (192,0,2,1,178,110)").unwrap(),45678);
        assert!(extended_port("(|||70000|)").is_err());assert!(passive_port("(1,2,3,4,5)").is_err());
        let e=parse_listing(b"Type=cdir; .\r\nType=pdir; ..\r\nType=dir; child\r\nType=file;Size=3;Modify=20261025120000; caf\xc3\xa9.txt\r\n",true).unwrap();
        assert_eq!(e.len(),2);assert!(e[0].directory);assert_eq!(e[1].name,"café.txt".as_bytes());assert_eq!(e[1].size,Some(3));
        let e=parse_listing(b"a file.txt\r\nraw\xffname\r\n",false).unwrap();assert_eq!(e[0].name,b"a file.txt");assert_eq!(e[1].name,b"raw\xffname");
    }
    #[tokio::test(flavor="current_thread")]
    async fn binary_retrieval_requires_successful_completion_and_the_announced_size() {
        use tokio::io::AsyncBufReadExt;
        for mismatch in [false,true] {
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let addr=listener.local_addr().unwrap();
            let server=tokio::spawn(async move {
                let (stream,_)=listener.accept().await.unwrap();let mut control=BufReader::new(stream);let mut data=None;
                control.get_mut().write_all(b"220 Fixture\r\n").await.unwrap();
                loop {
                    let mut line=String::new();if control.read_line(&mut line).await.unwrap()==0 {break;}
                    let reply=match line.trim_end() {
                        "USER anonymous"=>"331 Password\r\n".to_owned(),
                        "PASS anonymous@"=>"230 Ready\r\n".into(),
                        "FEAT"=>"500 Optional command unavailable\r\n".into(),
                        "TYPE I"=>"200 Binary\r\n".into(),
                        "SIZE file.bin"=>format!("213 {}\r\n",if mismatch{5}else{4}),
                        "EPSV"=>{
                            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();data=Some(listener);
                            format!("229 Passive (|||{port}|)\r\n")
                        },
                        "RETR file.bin"=>{
                            control.get_mut().write_all(b"150 Data\r\n").await.unwrap();let (mut stream,_)=data.take().unwrap().accept().await.unwrap();
                            stream.write_all(b"a\0\xffz").await.unwrap();stream.shutdown().await.unwrap();"226 Complete\r\n".into()
                        },
                        "QUIT"=>{control.get_mut().write_all(b"221 Closed\r\n").await.unwrap();break;},
                        other=>panic!("Unexpected fixture command: {other}"),
                    };
                    control.get_mut().write_all(reply.as_bytes()).await.unwrap();
                }
            });
            let target=Target::parse(&Url::parse(&format!("ftp://{addr}/file.bin")).unwrap()).unwrap();
            let result=tokio::time::timeout(std::time::Duration::from_secs(3),fetch(&target,None)).await.unwrap();
            if mismatch {assert!(matches!(result,Err(Error::Response)));}
            else {match result.unwrap(){Reply::File {bytes,..}=>assert_eq!(bytes,b"a\0\xffz"),_=>panic!("Expected complete file")}}
            server.await.unwrap();
        }
    }
    #[tokio::test(flavor="current_thread")]
    async fn ftp_control_replies_accept_multiline_and_telnet_escaping() {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let addr=listener.local_addr().unwrap();
        let server=tokio::spawn(async move {
            let (mut s,_)=listener.accept().await.unwrap();s.write_all(b"\xff\xfb\x01" ).await.unwrap();
            s.write_all(b"220-First\r\nUTF8\r\n220 Last\r\n").await.unwrap();
            let mut negotiation=[0;3];s.read_exact(&mut negotiation).await.unwrap();assert_eq!(negotiation,[255,254,1]);
            let mut command=[0;11];s.read_exact(&mut command).await.unwrap();assert_eq!(&command,b"RETR x\xff\xffy\r\n");
            s.write_all(b"200 Accepted\r\n").await.unwrap();
        });
        let stream=TcpStream::connect(addr).await.unwrap();let mut s=Session {peer:addr,control:BufReader::new(stream)};
        let r=s.status().await.unwrap();assert_eq!(r.code,220);assert_eq!(r.text,"First\nUTF8\nLast");
        assert_eq!(s.command("RETR",Some(b"x\xffy")).await.unwrap().code,200);server.await.unwrap();
    }
}
