// SPDX-License-Identifier: GPL-3.0-or-later
//! WHOIS (RFC 3912) and read-only RWhois 1.5 (RFC 2167).
//! Lookup/encoding query parameters are a browser address convention.
use super::{Error,MAX_BYTES,MAX_REQUEST,normalize_address,transfer};
use tokio::{io::{AsyncBufReadExt,AsyncReadExt,AsyncWriteExt,BufReader},net::TcpStream};
use url::Url;
use std::{future::Future,io,pin::Pin,task::{Context,Poll}};
use tokio::io::{AsyncRead,ReadBuf};

const LINE_LIMIT:usize=65536;
const MAX_ROWS:usize=16384;
const MAX_HOPS:u8=16;
#[derive(Clone,Debug)]
pub(super) struct Target {pub url:Url,pub query:Option<String>,pub latin1:bool,pub area:Option<String>,pub hops:u8}
fn decode(text:&str)->Result<String,Error>{
    let raw=text.as_bytes();for(i,b)in raw.iter().enumerate(){if *b==b'%' && (i+2>=raw.len() || !raw[i+1..i+3].iter().all(u8::is_ascii_hexdigit)){return Err(Error::Address);}}
    let value=percent_encoding::percent_decode_str(text).decode_utf8().map_err(|_|Error::Address)?.into_owned();if value.chars().any(char::is_control){return Err(Error::Address);}Ok(value)
}
fn area(text:&str)->bool{!text.is_empty() && !text.starts_with('/') && text.len()<=255 && text.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'.'|b'-'|b'/'))}
impl Target {
    pub(super) fn parse(url:&Url)->Result<Self,Error>{
        if !matches!(url.scheme(),"whois"|"rwhois") || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() || url.as_str().len()>16384{return Err(Error::Address);}
        let mut clean=url.clone();normalize_address(&mut clean)?;if clean.path().is_empty(){clean.set_path("/");}
        let rwhois=clean.scheme()=="rwhois";let path=decode(clean.path())?;
        let mut query=None;let mut latin1=false;let mut encoding_seen=false;let mut hops=0;let mut hop_seen=false;
        if let Some(parameters)=clean.query(){
            for pair in parameters.split('&'){
                let(key,value)=pair.split_once('=').ok_or(Error::Address)?;let value=decode(&value.replace('+'," "))?;
                match key{
                    "q" if query.is_none()=>query=Some(value),
                    "encoding" if !encoding_seen=>{encoding_seen=true;latin1=match value.as_str(){"utf-8"=>false,"latin1"|"iso-8859-1"=>true,_=>return Err(Error::Address)};},
                    "hop" if !hop_seen=>{hop_seen=true;hops=value.parse::<u8>().map_err(|_|Error::Address)?;if hops>MAX_HOPS{return Err(Error::Address);}},
                    _=>return Err(Error::Address),
                }
            }
        }
        let authority=if rwhois{
            if path=="/"{None}else{let text=path.strip_prefix("/auth-area=").filter(|s|area(s)).ok_or(Error::Address)?;Some(text.to_owned())}
        }else{
            if path!="/"{if query.is_some(){return Err(Error::Address);}query=Some(path[1..].to_owned());}None
        };
        let mut target=Self{url:clean,query,latin1,area:authority,hops};target.canonicalize();
        if target.query.as_ref().is_some_and(|q|!q.is_empty()){target.command()?;}Ok(target)
    }
    pub(super) fn label(&self)->&'static str{if self.url.scheme()=="rwhois"{"RWhois"}else{"WHOIS"}}
    fn canonicalize(&mut self){
        self.url.set_path(&self.area.as_ref().map(|a|format!("/auth-area={a}")).unwrap_or_else(||"/".into()));self.url.set_query(None);
        if let Some(query)=&self.query{self.url.query_pairs_mut().append_pair("q",query);}
        if self.latin1{self.url.query_pairs_mut().append_pair("encoding","latin1");}
        if self.hops>0{self.url.query_pairs_mut().append_pair("hop",&self.hops.to_string());}
    }
    pub(super) fn with_query(&self,query:String,latin1:bool)->Result<Self,Error>{let mut next=self.clone();next.query=Some(query);next.latin1=latin1;next.hops=0;next.command()?;next.canonicalize();Ok(next)}
    pub(super) fn command(&self)->Result<Vec<u8>,Error>{
        let text=self.query.as_ref().filter(|s|!s.is_empty()).ok_or(Error::Address)?;
        if text.len()>MAX_REQUEST || text.chars().any(char::is_control) || (self.url.scheme()=="rwhois" && text.trim_start().starts_with('-')){return Err(Error::Address);}
        let mut bytes=if self.latin1{text.chars().map(|c|u8::try_from(c as u32).map_err(|_|Error::Unsupported)).collect::<Result<Vec<_>,_>>()?}else{text.as_bytes().to_vec()};bytes.extend_from_slice(b"\r\n");Ok(bytes)
    }
    pub(super) fn referral(&self,value:&str)->Option<Self>{
        if self.hops>=MAX_HOPS{return None;}let url=Url::parse(value).ok()?;
        if url.scheme()!="rwhois" || url.query().is_some() || url.fragment().is_some(){return None;}
        let mut next=Self::parse(&url).ok()?;next.area.as_ref()?;next.query=self.query.clone();next.latin1=self.latin1;next.hops=self.hops+1;next.command().ok()?;next.canonicalize();Some(next)
    }
}
fn text(bytes:&[u8],latin1:bool)->String{if latin1{bytes.iter().copied().map(char::from).collect()}else{String::from_utf8(bytes.to_vec()).unwrap_or_else(|_|bytes.iter().copied().map(char::from).collect())}}

#[derive(Clone,Debug)]
pub(super) struct Attribute {pub class:String,pub name:String,pub kind:u8,pub value:String}
#[derive(Debug)]
pub(super) struct Results {pub records:Vec<Vec<Attribute>>,pub referrals:Vec<String>,pub notices:Vec<String>,pub status:Option<(u16,String)>,pub source:Vec<u8>}
pub(super) enum Reply {Text(Vec<u8>),Results(Results),Stream(transfer::Transfer),Failure(u16,String)}
struct Banner {flags:u32}
fn banner(bytes:&[u8])->Result<Banner,Error>{
    let value=std::str::from_utf8(bytes).map_err(|_|Error::Response)?;let mut words=value.splitn(4,' ');
    if words.next()!=Some("%rwhois"){return Err(Error::Response);}let versions=words.next().ok_or(Error::Response)?;if words.next().is_none_or(|s|s.is_empty()){return Err(Error::Response);}
    let selected=versions.split(',').find(|v|v.starts_with("V-1.5:")).ok_or(Error::Unsupported)?;
    let mut parts=selected.split(':');if parts.next()!=Some("V-1.5"){return Err(Error::Response);}let flags=parts.next().filter(|s|s.len()==6 && s.bytes().all(|b|b.is_ascii_hexdigit())).ok_or(Error::Response)?;
    let extra=parts.next().filter(|s|s.len()==2 && s.bytes().all(|b|b.is_ascii_hexdigit())).ok_or(Error::Response)?;let _=extra;if parts.next().is_some(){return Err(Error::Response);}Ok(Banner{flags:u32::from_str_radix(flags,16).map_err(|_|Error::Response)?})
}
fn status(bytes:&[u8],latin1:bool)->Result<Option<(u16,String)>,Error>{
    if bytes==b"%ok"{return Ok(None);}let rest=bytes.strip_prefix(b"%error ").ok_or(Error::Response)?;
    if rest.len()<5 || !rest[..3].iter().all(u8::is_ascii_digit) || rest[3]!=b' ' || rest[4..].is_empty(){return Err(Error::Response);}
    Ok(Some((std::str::from_utf8(&rest[..3]).unwrap().parse().map_err(|_|Error::Response)?,text(&rest[4..],latin1))))
}
fn atom(s:&str)->bool{!s.is_empty() && s.len()<=128 && s.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'_'|b'-'))}
fn attribute(bytes:&[u8],latin1:bool)->Result<Attribute,Error>{
    let first=bytes.iter().position(|b|*b==b':').ok_or(Error::Response)?;let second=bytes[first+1..].iter().position(|b|*b==b':').map(|i|i+first+1).ok_or(Error::Response)?;
    let class=std::str::from_utf8(&bytes[..first]).map_err(|_|Error::Response)?;let tag=std::str::from_utf8(&bytes[first+1..second]).map_err(|_|Error::Response)?;
    let(name,kind)=if let Some((name,kind))=tag.split_once(';'){(name,match kind{"T"=>b'T',"I"=>b'I',"S"=>b'S',_=>return Err(Error::Response)})}else{(tag,b'T')};
    if !atom(class) || !atom(name){return Err(Error::Response);}Ok(Attribute{class:class.into(),name:name.into(),kind,value:text(&bytes[second+1..],latin1)})
}
struct Session {io:BufReader<TcpStream>,bytes:usize,limit:usize,latin1:bool,notices:Vec<String>,wire:Vec<u8>}
impl Session {
    async fn line(&mut self)->Result<Vec<u8>,Error>{
        let mut line=Vec::new();loop{
            let bytes=self.io.fill_buf().await.map_err(|_|Error::Transport)?;if bytes.is_empty(){return Err(Error::Response);}let take=bytes.iter().position(|b|*b==b'\n').map(|i|i+1).unwrap_or(bytes.len());
            if line.len()+take>LINE_LIMIT || self.bytes.saturating_add(take)>self.limit{return Err(Error::TooLarge);}line.extend_from_slice(&bytes[..take]);self.io.consume(take);self.bytes+=take;
            if line.last()==Some(&b'\n'){
                // ARIN's reference implementation emits LF on Unix. Accept
                // that unambiguous historical delimiter; always send CRLF,
                // and retain original reply delimiters in source downloads.
                self.wire=line.clone();line.pop();if line.last()==Some(&b'\r'){line.pop();}if line.iter().any(|b|*b==0 || *b==b'\r'){return Err(Error::Response);}return Ok(line);
            }
        }
    }
    async fn meaningful(&mut self)->Result<Vec<u8>,Error>{
        loop{let line=self.line().await?;if line==b"%info on"{
            let mut notice=Vec::new();loop{let info=self.line().await?;if info==b"%info off"{break;}notice.extend_from_slice(&info);notice.push(b'\n');}if self.notices.len()>=1024{return Err(Error::TooLarge);}self.notices.push(text(&notice,self.latin1));
        }else{return Ok(line);}}
    }
    async fn send(&mut self,bytes:&[u8])->Result<(),Error>{self.io.write_all(bytes).await.map_err(|_|Error::Transport)}
    async fn directive(&mut self,command:&[u8])->Result<Option<(u16,String)>,Error>{self.send(command).await?;status(&self.meaningful().await?,self.latin1)}
    async fn finish(&mut self,flags:u32){
        if flags&0x80!=0{let _=tokio::time::timeout(std::time::Duration::from_millis(500),self.directive(b"-quit\r\n")).await;}
        let _=self.io.shutdown().await;
    }
    async fn results(&mut self)->Result<Results,Error>{
        let mut result=Results{records:Vec::new(),referrals:Vec::new(),notices:Vec::new(),status:None,source:Vec::new()};let mut record=Vec::new();let mut info=false;let mut notice=Vec::new();
        for _ in 0..MAX_ROWS{
            let line=self.line().await?;result.source.extend_from_slice(&self.wire);
            if info{if line==b"%info off"{info=false;result.notices.push(text(&notice,self.latin1));notice.clear();}else{notice.extend_from_slice(&line);notice.push(b'\n');}continue;}
            if line==b"%info on"{info=true;continue;}
            if line==b"%ok" || line.starts_with(b"%error "){
                result.status=status(&line,self.latin1)?;if !record.is_empty(){result.records.push(record);}result.notices.splice(0..0,self.notices.drain(..));return Ok(result);
            }
            if line.is_empty(){if !record.is_empty(){result.records.push(std::mem::take(&mut record));}continue;}
            if let Some(value)=line.strip_prefix(b"%referral "){
                let value=std::str::from_utf8(value).map_err(|_|Error::Response)?;if result.referrals.len()>=64{return Err(Error::TooLarge);}result.referrals.push(value.into());continue;
            }
            if line.starts_with(b"%"){return Err(Error::Response);}record.push(attribute(&line,self.latin1)?);
        }
        Err(Error::TooLarge)
    }
}
struct Source {session:Session,flags:u32,info:bool,done:bool}
impl Source {
    async fn batch(&mut self)->Result<Option<Vec<u8>>,Error>{
        if self.done{return Ok(None);}let mut bytes=Vec::new();
        while bytes.len()<64*1024{
            let line=self.session.line().await?;bytes.extend_from_slice(&self.session.wire);
            if self.info{if line==b"%info off"{self.info=false;}continue;}
            if line==b"%info on"{self.info=true;continue;}
            if line==b"%ok" || line.starts_with(b"%error "){status(&line,self.session.latin1)?;self.session.finish(self.flags).await;self.done=true;break;}
            if line.is_empty(){continue;}
            if let Some(value)=line.strip_prefix(b"%referral "){std::str::from_utf8(value).map_err(|_|Error::Response)?;continue;}
            if line.starts_with(b"%"){return Err(Error::Response);}attribute(&line,self.session.latin1)?;
        }
        Ok(Some(bytes))
    }
}
type SourceNext=Pin<Box<dyn Future<Output=(Source,Result<Option<Vec<u8>>,Error>)>+Send>>;
struct SourceReader {source:Option<Source>,next:Option<SourceNext>,bytes:Vec<u8>,offset:usize,done:bool}
impl AsyncRead for SourceReader {
    fn poll_read(mut self:Pin<&mut Self>,cx:&mut Context<'_>,target:&mut ReadBuf<'_>)->Poll<io::Result<()>>{
        if target.remaining()==0{return Poll::Ready(Ok(()));}
        if self.offset<self.bytes.len(){let n=target.remaining().min(self.bytes.len()-self.offset);target.put_slice(&self.bytes[self.offset..self.offset+n]);self.offset+=n;return Poll::Ready(Ok(()));}
        if self.done{return Poll::Ready(Ok(()));}
        if self.next.is_none(){let mut source=self.source.take().expect("RWhois source owns its session");self.next=Some(Box::pin(async move{let next=source.batch().await;(source,next)}));}
        match self.next.as_mut().unwrap().as_mut().poll(cx){
            Poll::Pending=>Poll::Pending,
            Poll::Ready((source,next))=>{
                self.next=None;self.source=Some(source);match next{
                    Ok(Some(bytes))=>{let n=target.remaining().min(bytes.len());target.put_slice(&bytes[..n]);self.bytes=bytes;self.offset=n;Poll::Ready(Ok(()))},
                    Ok(None)=>{self.done=true;self.source=None;Poll::Ready(Ok(()))},
                    Err(error)=>{self.done=true;self.source=None;Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData,error.message())))},
                }
            },
        }
    }
}
async fn rwhois(target:&Target,download:bool)->Result<Reply,Error>{
    let stream=TcpStream::connect((target.url.host_str().ok_or(Error::Address)?.trim_matches(['[',']']),target.url.port().unwrap_or(4321))).await.map_err(|_|Error::Transport)?;
    let mut session=Session{io:BufReader::new(stream),bytes:0,limit:MAX_BYTES,latin1:target.latin1,notices:Vec::new(),wire:Vec::new()};
    let initial=session.meaningful().await?;if initial.starts_with(b"%error "){let(code,message)=status(&initial,target.latin1)?.ok_or(Error::Response)?;return Ok(Reply::Failure(code,message));}banner(&initial)?;
    session.send(b"-rwhois V-1.5 DomeBrowser/0.1\r\n").await?;let negotiated=session.meaningful().await?;
    if negotiated.starts_with(b"%error "){let(code,message)=status(&negotiated,target.latin1)?.ok_or(Error::Response)?;return Ok(Reply::Failure(code,message));}
    let flags=banner(&negotiated)?.flags;if let Some((code,message))=status(&session.meaningful().await?,target.latin1)?{return Ok(Reply::Failure(code,message));}
    // Ask only for declared read/session capabilities. Unsupported optional
    // controls leave server defaults in place; a fatal result ends the load.
    for(bit,command)in [(0x04,b"-display dump\r\n".as_slice()),(0x20,b"-limit 100\r\n".as_slice()),(0x08,b"-forward off\r\n".as_slice())]{
        if flags&bit!=0{if let Some((code,message))=session.directive(command).await?{if code>=500{session.finish(flags).await;return Ok(Reply::Failure(code,message));}}}
    }
    session.send(&target.command()?).await?;
    if download{
        session.limit=usize::MAX;let reader=SourceReader{source:Some(Source{session,flags,info:false,done:false}),next:None,bytes:Vec::new(),offset:0,done:false};
        return Ok(Reply::Stream(transfer::Transfer::new("text/plain".into(),None,reader)));
    }
    let result=session.results().await?;session.finish(flags).await;Ok(Reply::Results(result))
}
pub(super) async fn open(target:&Target,download:bool)->Result<Reply,Error>{
    target.command()?;if target.url.scheme()=="rwhois"{return rwhois(target,download).await;}
    let host=target.url.host_str().ok_or(Error::Address)?.trim_matches(['[',']']);let mut socket=TcpStream::connect((host,target.url.port().unwrap_or(43))).await.map_err(|_|Error::Transport)?;socket.write_all(&target.command()?).await.map_err(|_|Error::Transport)?;
    if download{return Ok(Reply::Stream(transfer::Transfer::new("text/plain".into(),None,socket)));}
    let mut bytes=Vec::new();(&mut socket).take((MAX_BYTES+1)as u64).read_to_end(&mut bytes).await.map_err(|_|Error::Transport)?;
    if bytes.len()>MAX_BYTES{let reader=std::io::Cursor::new(bytes).chain(socket);return Ok(Reply::Stream(transfer::Transfer::new("text/plain".into(),None,reader)));}
    Ok(Reply::Text(bytes))
}

pub(super) fn document(target:&Target,reply:Option<&Reply>,action:Option<&Url>,notice:Option<&str>)->String{
    use super::document::{escape,page_with_metadata,link_target};
    let title=if reply.is_none(){format!("{} lookup",target.label())}else{format!("{} results",target.label())};let mut body=format!("<h1>{}</h1>",escape(&title));
    if let Some(query)=&target.query{body.push_str(&format!("<p>Lookup: <strong>{}</strong></p>",escape(query)));}
    if let Some(area)=&target.area{body.push_str(&format!("<p>Authority area: {}</p>",escape(area)));}
    if let Some(action)=action{body.push_str(&format!("<p><a href=\"{}\" rel=noreferrer>{}</a></p>",escape(action.as_str()),if target.query.is_some(){"Change lookup"}else{"Look up a directory entry"}));}
    if let Some(notice)=notice{body.push_str(&format!("<p>{}</p>",escape(notice)));}
    match reply{
        None=>{if notice.is_none(){body.push_str("<p>Choose a lookup to send to this directory server.</p>");}},
        Some(Reply::Text(bytes))=>body.push_str(&format!("<pre>{}</pre>",escape(&text(bytes,target.latin1)))),
        Some(Reply::Failure(code,message))=>body.push_str(&format!("<h2>Directory server response</h2><p>{code}: {}</p>",escape(message))),
        Some(Reply::Results(result))=>{
            for notice in &result.notices{body.push_str(&format!("<aside><pre>{}</pre></aside>",escape(notice)));}
            for record in &result.records{
                body.push_str("<section><dl>");for attribute in record{
                    body.push_str(&format!("<dt>{}: {}</dt><dd>",escape(&attribute.class),escape(&attribute.name)));
                    let linked=match attribute.kind{
                        b'I'=>target.with_query(format!("ID={}",attribute.value),target.latin1).ok().map(|t|t.url),
                        b'S'=>link_target(&target.url,&attribute.value),_=>None,
                    };
                    if let Some(url)=linked{body.push_str(&format!("<a href=\"{}\" rel=noreferrer>{}</a>",escape(url.as_str()),escape(&attribute.value)));}else{body.push_str(&escape(&attribute.value));}body.push_str("</dd>");
                }body.push_str("</dl></section>");
            }
            if !result.referrals.is_empty(){body.push_str("<h2>Directory referrals</h2><p>Select a directory to continue this lookup.</p><ul>");for referral in &result.referrals{
                if let Some(next)=target.referral(referral){body.push_str(&format!("<li><a href=\"{}\" rel=noreferrer>{}</a></li>",escape(next.url.as_str()),escape(referral)));}else{body.push_str(&format!("<li>{} (referral unavailable or navigation limit reached)</li>",escape(referral)));}
            }body.push_str("</ul>");}
            if let Some((code,message))=&result.status{body.push_str(&format!("<p>RWhois {code}: {}</p>",escape(message)));}
            if result.records.is_empty() && result.referrals.is_empty() && result.status.is_none(){body.push_str("<p>No directory entries were returned.</p>");}
        },Some(Reply::Stream(_))=>{},
    }
    page_with_metadata(&target.url,&body,None,Some(&title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{net::TcpListener,io::AsyncBufReadExt};
    fn target(value:&str)->Target{Target::parse(&Url::parse(value).unwrap()).unwrap()}
    #[test]
    fn lookup_addresses_decode_once_and_keep_queries_separate_from_directives(){
        let lookup=target("whois://example.org/Smith%20Jones#part");assert_eq!(lookup.url.as_str(),"whois://example.org/?q=Smith+Jones#part");assert_eq!(lookup.command().unwrap(),b"Smith Jones\r\n");
        assert_eq!(target("whois://example.org/?q=literal%2520value").command().unwrap(),b"literal%20value\r\n");
        assert_eq!(target("rwhois://example.org/auth-area=10.0.0.0/8?q=Ren%C3%A9e&encoding=latin1").command().unwrap(),b"Ren\xe9e\r\n");
        for uri in ["rwhois://example.org/?q=-register+on","rwhois://example.org/?q=+-quit","whois://example.org/?q=a%0db","whois://example.org/?q=a%00b","whois://example.org/?q=%zz","whois://example.org/?q=a&q=b","whois://name@example.org/?q=a","rwhois://example.org/auth-area=/bad?q=a","rwhois://example.org/arbitrary?q=a"]{assert!(Target::parse(&Url::parse(uri).unwrap()).is_err(),"{uri}");}
        assert!(target("whois://example.org/?q=%E4%B8%96%E7%95%8C").with_query("世界".into(),true).is_err());
    }
    #[test]
    fn referrals_preserve_the_query_and_authority_with_a_navigation_bound(){
        let lookup=target("rwhois://example.org/auth-area=.?q=domain+example.org&encoding=latin1");let next=lookup.referral("rwhois://other.example:4321/auth-area=example.org").unwrap();assert_eq!(next.query,lookup.query);assert_eq!(next.area.as_deref(),Some("example.org"));assert!(next.latin1);assert_eq!(next.hops,1);
        assert!(lookup.referral("https://example.org/").is_none());assert!(lookup.referral("rwhois://other.example/auth-area=.?q=other").is_none());
        assert!(target("rwhois://example.org/?q=a&hop=16").referral("rwhois://other.example/auth-area=.").is_none());
    }
    #[test]
    fn rwhois_banners_and_result_statuses_are_structured(){
        assert_eq!(banner(b"%rwhois V-1.0,V-1.5:0000a4:00 reference.example (Reference)").unwrap().flags,0xa4);
        assert!(matches!(banner(b"%rwhois V-1.0 reference.example"),Err(Error::Unsupported)));assert!(banner(b"%rwhois V-1.5:bad:00 reference.example").is_err());assert!(status(b"%ok trailing",false).is_err());assert_eq!(status(b"%error 330 Maximum objects limit",false).unwrap(),Some((330,"Maximum objects limit".into())));
    }
    async fn connection(bytes:Vec<u8>)->(TcpStream,tokio::task::JoinHandle<()>){
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let server=tokio::spawn(async move{let(mut socket,_)=listener.accept().await.unwrap();socket.write_all(&bytes).await.unwrap();});(TcpStream::connect(address).await.unwrap(),server)
    }
    fn session(socket:TcpStream)->Session{Session{io:BufReader::new(socket),bytes:0,limit:MAX_BYTES,latin1:false,notices:Vec::new(),wire:Vec::new()}}
    #[tokio::test(flavor="current_thread")]
    async fn rwhois_info_blocks_cannot_end_results_and_original_delimiters_survive(){
        for separator in [b"\r\n".as_slice(),b"\n".as_slice()]{
            let lines=[b"%info on".as_slice(),b"%ok",b"Service information <&>",b"%info off",b"domain:ID;I:one.example",b"domain:Description:<script>plain</script>",b"",b"%referral rwhois://other.example/auth-area=example",b"%error 330 Limit reached"];
            let mut bytes=Vec::new();for line in lines{bytes.extend_from_slice(line);bytes.extend_from_slice(separator);}let expected=bytes.clone();let(socket,server)=connection(bytes).await;let result=session(socket).results().await.unwrap();server.await.unwrap();assert_eq!(result.source,expected);assert_eq!(result.records.len(),1);assert_eq!(result.referrals.len(),1);assert!(result.notices[0].starts_with("%ok\n"));
            let html=document(&target("rwhois://example.org/?q=domain+example"),Some(&Reply::Results(result)),None,None);assert!(html.contains("&lt;script&gt;plain&lt;/script&gt;"));assert!(html.contains("ID%3Done.example"));
        }
        for bytes in [b"domain:ID:one\r\n".as_slice(),b"%info on\r\n%ok\r\n",b"malformed record\r\n%ok\r\n"]{let(socket,server)=connection(bytes.to_vec()).await;assert!(session(socket).results().await.is_err());server.await.unwrap();}
    }
    #[tokio::test(flavor="current_thread")]
    async fn whois_preserves_multiline_bytes_and_waits_for_connection_completion(){
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let expected=b"First line\r\nSecond line\nRaw Latin-1: \xe9\r\n";
        let server=tokio::spawn(async move{let(socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);let mut command=String::new();socket.read_line(&mut command).await.unwrap();assert_eq!(command,"Smith\r\n");socket.write_all(&expected[..12]).await.unwrap();tokio::time::sleep(std::time::Duration::from_millis(30)).await;socket.write_all(&expected[12..]).await.unwrap();});
        let Reply::Text(bytes)=open(&target(&format!("whois://{address}/?q=Smith")),false).await.unwrap()else{panic!("Expected WHOIS text")};assert_eq!(bytes,expected);server.await.unwrap();
    }
    #[tokio::test(flavor="current_thread")]
    async fn rwhois_negotiates_only_read_controls_and_completes_before_peer_eof(){
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let server=tokio::spawn(async move{
            let(socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);socket.write_all(b"%rwhois V-1.5:0000a4:00 reference.example\r\n").await.unwrap();let mut command=String::new();
            for expected in ["-rwhois V-1.5 DomeBrowser/0.1\r\n","-display dump\r\n","-limit 100\r\n","domain example\r\n","-quit\r\n"]{
                command.clear();socket.read_line(&mut command).await.unwrap();assert_eq!(command,expected);
                if expected.starts_with("-rwhois"){socket.write_all(b"%rwhois V-1.5:0000a4:00 reference.example\r\n").await.unwrap();}
                if expected.starts_with("domain"){socket.write_all(b"domain:ID:one.example\r\n\r\n").await.unwrap();}socket.write_all(b"%ok\r\n").await.unwrap();
            }
        });
        let reply=tokio::time::timeout(std::time::Duration::from_secs(3),open(&target(&format!("rwhois://{address}/?q=domain+example")),false)).await.unwrap().unwrap();let Reply::Results(result)=reply else{panic!("Expected RWhois records")};assert_eq!(result.records.len(),1);server.await.unwrap();
    }
    #[tokio::test(flavor="current_thread")]
    async fn rwhois_source_streams_beyond_preview_and_requires_terminal_status(){
        for complete in [true,false]{
            let line=[b"domain:Description:".as_slice(),&vec![b'x';4096],b"\r\n"].concat();let mut expected=line.repeat(MAX_BYTES/line.len()+1);if complete{expected.extend_from_slice(b"%ok\r\n");}let(socket,server)=connection(expected.clone()).await;let mut s=session(socket);s.limit=usize::MAX;
            let mut reader=SourceReader{source:Some(Source{session:s,flags:0,info:false,done:false}),next:None,bytes:Vec::new(),offset:0,done:false};let mut bytes=Vec::new();let result=reader.read_to_end(&mut bytes).await;
            if complete{assert_eq!(result.unwrap(),expected.len());assert_eq!(bytes,expected);}else{assert!(result.is_err());}server.await.unwrap();
        }
    }
}
