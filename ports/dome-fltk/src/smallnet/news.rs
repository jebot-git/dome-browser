// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only NNTP (3977), news/nntp URIs (5538), and TLS (4642/8143).
use super::{Error,MAX_BYTES,normalize_address};
use std::{collections::BTreeMap,io::Read,path::PathBuf,sync::{Arc,Mutex}};
use serde::{Deserialize,Serialize};
use tokio::{io::{AsyncBufReadExt,AsyncRead,AsyncReadExt,AsyncWrite,AsyncWriteExt,BufReader},net::TcpStream};
use url::Url;
const LINE:usize=65536;
const GROUPS:usize=1024;
const PAGE:u64=50;

#[derive(Clone,Copy,Debug,Serialize,Deserialize,PartialEq)]
#[serde(rename_all="lowercase")]
pub(super) enum Transport { Plain, Starttls, Tls }
impl Transport {pub fn label(self)->&'static str {match self {Self::Plain=>"Unencrypted",Self::Starttls=>"STARTTLS",Self::Tls=>"TLS"}}}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub(super) struct Server {pub host:String,pub port:u16,pub transport:Transport,#[serde(default)] pub ca_file:Option<PathBuf>}
impl Server {
 pub fn endpoint(&self)->Result<Url,Error> {
  let mut url=Url::parse("news://example.invalid/").unwrap();url.set_host(Some(&self.host)).map_err(|_|Error::Address)?;url.set_port(Some(self.port)).map_err(|_|Error::Address)?;
  if self.port==0{return Err(Error::Address);}normalize_address(&mut url)?;Ok(url)
 }
 fn key(&self)->Result<String,Error> {Ok(self.endpoint()?.to_string())}
 pub fn for_url(url:&Url)->Result<Self,Error> {
  let server=Self {host:url.host_str().ok_or(Error::Address)?.into(),port:url.port().unwrap_or(119),transport:if url.port()==Some(563){Transport::Tls}else{Transport::Plain},ca_file:None};server.endpoint()?;Ok(server)
 }
}
#[derive(Clone,Serialize,Deserialize)]
struct Settings {version:u32,default:Option<String>,servers:BTreeMap<String,Server>}
impl Default for Settings {fn default()->Self {Self {version:1,default:None,servers:BTreeMap::new()}}}
pub(super) struct Store {path:Option<PathBuf>,memory:Mutex<Settings>}
impl Store {
 pub fn new(path:Option<PathBuf>)->Self {Self {path,memory:Mutex::new(Settings::default())}}
 fn load(&self)->Result<std::sync::MutexGuard<'_,Settings>,Error> {
  let mut settings=self.memory.lock().unwrap_or_else(|e|e.into_inner());
  if let Some(path)=&self.path {
   use std::os::unix::fs::{OpenOptionsExt,MetadataExt};
   match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path) {
    Ok(file)=>{let info=file.metadata().map_err(|_|Error::NewsSettings)?;if !info.is_file() || info.uid()!=unsafe{libc::geteuid()} || info.mode()&0o077!=0 || info.len()>1024*1024{return Err(Error::NewsSettings);}
     let mut bytes=Vec::new();file.take(1024*1024+1).read_to_end(&mut bytes).map_err(|_|Error::NewsSettings)?;if bytes.len()>1024*1024{return Err(Error::NewsSettings);}*settings=serde_json::from_slice(&bytes).map_err(|_|Error::NewsSettings)?;},
    Err(e) if e.kind()==std::io::ErrorKind::NotFound=>*settings=Settings::default(),Err(_)=>return Err(Error::NewsSettings),
   }
  }
  if settings.version!=1 || settings.servers.len()>256 || settings.servers.iter().any(|(key,s)|s.key().as_ref()!=Ok(key)) || settings.default.as_ref().is_some_and(|key|!settings.servers.contains_key(key)){return Err(Error::NewsSettings);}
  Ok(settings)
 }
 pub fn get(&self,url:&Url)->Result<Option<Server>,Error> {
  let settings=self.load()?;
  if url.host_str().is_none(){Ok(settings.default.as_ref().and_then(|key|settings.servers.get(key)).cloned())}
  else {let default=Server::for_url(url)?;Ok(Some(settings.servers.get(&default.key()?).cloned().unwrap_or(default)))}
 }
 pub fn remember(&self,server:Server,default:bool)->Result<(),Error> {
  let key=server.key()?;let mut settings=self.load()?;
  if settings.servers.len()>=256 && !settings.servers.contains_key(&key){return Err(Error::NewsSettings);}
  settings.servers.insert(key.clone(),server);if default{settings.default=Some(key);}
  if let Some(path)=&self.path {crate::storage::write(path,&(serde_json::to_string_pretty(&*settings).map_err(|_|Error::NewsSettings)?+"\n")).map_err(|_|Error::NewsSettings)?;}Ok(())
 }
}

#[derive(Clone,Debug,PartialEq)]
pub(super) enum Kind {Groups(String),Group(String,Option<u64>),Article(String)}
#[derive(Clone,Debug)]
pub(super) struct Target {pub url:Url,pub kind:Kind,pub before:Option<u64>,pub part:Option<usize>}
fn decode(s:&str)->Result<String,Error> {
 let b=s.as_bytes();for (i,c) in b.iter().enumerate(){if *c==b'%' && (i+2>=b.len() || !b[i+1..i+3].iter().all(u8::is_ascii_hexdigit)){return Err(Error::Address);}}
 percent_encoding::percent_decode_str(s).decode_utf8().map(|s|s.into_owned()).map_err(|_|Error::Address)
}
fn number(s:&str)->Result<u64,Error> {if s.is_empty() || s.len()>16 || !s.bytes().all(|b|b.is_ascii_digit()){return Err(Error::Address);}s.parse().map_err(|_|Error::Address)}
pub(super) fn group(s:&str,wild:bool)->bool {
 !s.is_empty() && s.len()<=480 && !matches!(s,"."|"..") && !s.chars().any(|c|c.is_control() || c.is_whitespace() || matches!(c,'!'|','|'['|']'|'\\') || (!wild && matches!(c,'*'|'?')))
}
pub(super) fn message_id(s:&str)->bool {s.len()<=248 && s.contains('@') && s.bytes().all(|b|(33..127).contains(&b) && !matches!(b,b'<'|b'>'))}
impl Target {
 pub fn parse(url:&Url)->Result<Self,Error> {
  if !matches!(url.scheme(),"news"|"nntp") || url.as_str().len()>8192 || !url.username().is_empty() || url.password().is_some(){return Err(Error::Address);}
  let mut url=url.clone();if url.host_str().is_some(){normalize_address(&mut url)?;if url.port()==Some(0){return Err(Error::Address);}}
  else if url.scheme()!="news" || !url.cannot_be_a_base(){return Err(Error::Address);}
  let path=url.path().strip_prefix('/').unwrap_or(url.path());
  let kind=if url.scheme()=="nntp" {
   let mut parts=path.split('/');let name=decode(parts.next().unwrap_or(""))?;if !group(&name,false){return Err(Error::Address);}
   let article=parts.next().map(number).transpose()?;if parts.next().is_some() || article==Some(0){return Err(Error::Address);}Kind::Group(name,article)
  }else {
   let resource=decode(path)?;
   if resource.contains('@'){if !message_id(&resource){return Err(Error::Address);}Kind::Article(resource)}
   else if resource.is_empty(){Kind::Groups("*".into())}
   else {if !group(&resource,true){return Err(Error::Address);}if resource.contains(['*','?']){Kind::Groups(resource)}else{Kind::Group(resource,None)}}
  };
  let mut before=None;let mut part=None;
  if let Some(query)=url.query(){for value in query.split('&') {
   let (key,value)=value.split_once('=').ok_or(Error::Address)?;let n=number(value)?;if n==0{return Err(Error::Address);}
   match key {"before" if before.is_none()=>before=Some(n),"part" if part.is_none() && n<=512=>part=Some(n as usize),_=>return Err(Error::Address),}
  }}
  let article=matches!(kind,Kind::Article(_)|Kind::Group(_,Some(_)));
  if (article && before.is_some()) || (!article && part.is_some()) || (before.is_some() && !matches!(kind,Kind::Group(_,None))){return Err(Error::Address);}
  Ok(Self {url,kind,before,part})
 }
 pub fn with_server(&self,server:&Server)->Result<Url,Error> {
  let mut url=server.endpoint()?;url.set_scheme(self.url.scheme()).map_err(|_|Error::Address)?;url.set_path(&format!("/{}",self.url.path().trim_start_matches('/')));url.set_query(self.url.query());url.set_fragment(self.url.fragment());Ok(url)
 }
 pub fn root(&self)->Url {let mut url=self.url.clone();url.set_scheme("news").ok();url.set_path("/");url.set_query(None);url.set_fragment(None);url}
 pub fn group_url(&self,name:&str)->Url {let mut url=self.root();url.set_path(&format!("/{}",percent_encoding::utf8_percent_encode(name,percent_encoding::NON_ALPHANUMERIC)));url}
 pub fn article_url(&self,name:&str,n:u64)->Url {let mut url=self.group_url(name);url.set_scheme("nntp").ok();url.set_path(&format!("{}/{n}",url.path()));url}
 pub fn id_url(&self,id:&str)->Option<Url> {let id=id.trim_matches(['<','>']);if !message_id(id){return None;}let mut url=self.root();url.set_path(&format!("/{}",percent_encoding::utf8_percent_encode(id,percent_encoding::NON_ALPHANUMERIC)));Some(url)}
}

trait Io:AsyncRead+AsyncWrite+Unpin+Send {}
impl<T:AsyncRead+AsyncWrite+Unpin+Send> Io for T {}
type Wire=Box<dyn Io>;
struct Session {io:BufReader<Wire>,bytes:usize}
#[derive(Debug)]
struct Status {code:u16,text:String}
impl Session {
 async fn line(&mut self,limit:usize)->Result<Vec<u8>,Error> {
  let mut line=Vec::new();loop {
   let bytes=self.io.fill_buf().await.map_err(|_|Error::Transport)?;if bytes.is_empty(){return Err(Error::Response);}
   let take=bytes.iter().position(|b|*b==b'\n').map(|n|n+1).unwrap_or(bytes.len());
   if line.len()+take>limit || self.bytes+take>MAX_BYTES{return Err(Error::TooLarge);}
   line.extend_from_slice(&bytes[..take]);self.bytes+=take;self.io.consume(take);if line.last()==Some(&b'\n'){break;}
  }
  if !line.ends_with(b"\r\n"){return Err(Error::Response);}line.truncate(line.len()-2);Ok(line)
 }
 async fn status(&mut self)->Result<Status,Error> {
  let line=self.line(512).await?;
  if line.len()<3 || !line[..3].iter().all(u8::is_ascii_digit) || (line.len()>3 && line[3]!=b' '){return Err(Error::Response);}
  let code=std::str::from_utf8(&line[..3]).unwrap().parse().map_err(|_|Error::Response)?;
  if !(100..=599).contains(&code){return Err(Error::Response);}
  let text=std::str::from_utf8(line.get(4..).unwrap_or_default()).map_err(|_|Error::Response)?;
  if text.chars().any(|c|c.is_control() && c!='\t'){return Err(Error::Response);}Ok(Status {code,text:text.into()})
 }
 async fn command(&mut self,text:&str)->Result<Status,Error> {
  if text.len()+2>512 || text.chars().any(char::is_control){return Err(Error::Address);}
  self.io.get_mut().write_all(format!("{text}\r\n").as_bytes()).await.map_err(|_|Error::Transport)?;self.status().await
 }
 async fn block(&mut self,limit:usize)->Result<Vec<Vec<u8>>,Error> {
  let mut rows=Vec::new();loop {
   let mut row=self.line(LINE).await?;if row==b"."{return Ok(rows);}if row.starts_with(b".."){row.remove(0);}
   if rows.len()>=limit{return Err(Error::TooLarge);}rows.push(row);
  }
 }
 async fn capabilities(&mut self)->Result<Result<Option<Vec<String>>,Status>,Error> {
  let status=self.command("CAPABILITIES").await?;
  if matches!(status.code,500|501){return Ok(Ok(None));}if status.code!=101{return Ok(Err(status));}
  let rows=self.block(1024).await?.into_iter().map(|r|String::from_utf8(r).map_err(|_|Error::Response)).collect::<Result<Vec<_>,_>>()?;Ok(Ok(Some(rows)))
 }
 async fn reader(&mut self,caps:&mut Option<Vec<String>>)->Result<Option<Status>,Error> {
  let has=|word:&str,caps:&Option<Vec<String>>|caps.as_ref().is_some_and(|c|c.iter().any(|s|s.split_whitespace().next()==Some(word)));
  if has("READER",caps){return Ok(None);}
  if caps.is_some() && !has("MODE-READER",caps){return Ok(Some(Status {code:502,text:"This server does not offer news reading".into()}));}
  let status=self.command("MODE READER").await?;
  if matches!(status.code,200|201){match self.capabilities().await? {Ok(new)=>*caps=new,Err(status)=>return Ok(Some(status))};}
  else if caps.is_some() || !matches!(status.code,500|501){return Ok(Some(status));}Ok(None)
 }
 async fn finish(&mut self) {let _=tokio::time::timeout(std::time::Duration::from_millis(500),self.command("QUIT")).await;let _=self.io.get_mut().shutdown().await;}
}
fn tls_config(server:&Server)->Result<Arc<rustls::ClientConfig>,Error> {
 use rustls::pki_types::pem::PemObject;
 let mut roots=rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
 if let Some(path)=&server.ca_file {
  let file=std::fs::File::open(path).map_err(|_|Error::Certificate)?;let meta=file.metadata().map_err(|_|Error::Certificate)?;
  if !meta.is_file() || meta.len()>1024*1024{return Err(Error::Certificate);}
  let mut bytes=Vec::new();file.take(1024*1024+1).read_to_end(&mut bytes).map_err(|_|Error::Certificate)?;if bytes.len()>1024*1024{return Err(Error::Certificate);}
  let mut count=0;for cert in rustls::pki_types::CertificateDer::pem_slice_iter(&bytes){roots.add(cert.map_err(|_|Error::Certificate)?).map_err(|_|Error::Certificate)?;count+=1;}if count==0{return Err(Error::Certificate);}
 }
 let provider=Arc::new(rustls::crypto::aws_lc_rs::default_provider());
 let config=rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().map_err(|_|Error::Certificate)?.with_root_certificates(roots).with_no_client_auth();Ok(Arc::new(config))
}
async fn encrypt(wire:Wire,server:&Server)->Result<Wire,Error> {
 let endpoint=server.endpoint()?;let name=rustls::pki_types::ServerName::try_from(endpoint.host_str().ok_or(Error::Address)?.trim_matches(['[',']']).to_owned()).map_err(|_|Error::Address)?;
 let tls=tokio_rustls::TlsConnector::from(tls_config(server)?).connect(name,wire).await.map_err(|_|Error::Certificate)?;Ok(Box::new(tls))
}

#[derive(Debug)]
pub(super) struct Group {pub name:String,pub high:u64,pub low:u64,pub status:String}
#[derive(Debug)]
pub(super) struct Overview {pub number:u64,pub subject:String,pub author:String,pub date:String,pub id:String,pub references:String}
pub(super) enum Reply {
 Groups {groups:Vec<Group>,truncated:bool},
 Overview {name:String,low:u64,high:u64,start:u64,end:u64,items:Vec<Overview>},
 Article(Vec<u8>),Stream(super::transfer::Transfer),Failure(u16,String),
}
fn failure(status:Status)->Reply {Reply::Failure(status.code,status.text)}
fn header_text(name:&str,value:&str)->String {
 let raw=format!("{name}: {value}\r\n\r\n");
 let Some(message)=mail_parser::MessageParser::new().parse_headers(&raw) else{return value.into();};
 if name=="Subject" {message.subject().unwrap_or(value).into()}
 else if name=="From" {message.from().map(|a|a.mailboxes().map(|m|match (m.name(),m.address()){(Some(n),Some(a))=>format!("{n} <{a}>"),(Some(n),None)=>n.into(),(_,Some(a))=>a.into(),_=>String::new()}).collect::<Vec<_>>().join(", ")).unwrap_or_else(||value.into())}
 else {value.into()}
}
fn overview(row:&[u8],start:u64,end:u64)->Result<Overview,Error> {
 let text=std::str::from_utf8(row).map_err(|_|Error::Response)?;let cols=text.split('\t').collect::<Vec<_>>();if cols.len()<8{return Err(Error::Response);}
 let n=number(cols[0]).map_err(|_|Error::Response)?;if !(start..=end).contains(&n){return Err(Error::Response);}
 let id=cols[4].trim_matches(['<','>']);if !message_id(id){return Err(Error::Response);}
 Ok(Overview {number:n,subject:header_text("Subject",cols[1]),author:header_text("From",cols[2]),date:cols[3].into(),id:id.into(),references:cols[5].into()})
}
fn article_header(bytes:&[u8])->Result<(String,String,String,String,String),Error> {
 let message=mail_parser::MessageParser::new().parse_headers(bytes).ok_or(Error::Response)?;
 Ok((message.subject().unwrap_or("(no subject)").into(),message.from().map(|a|a.mailboxes().map(|m|match (m.name(),m.address()){(Some(n),Some(a))=>format!("{n} <{a}>"),(Some(n),None)=>n.into(),(_,Some(a))=>a.into(),_=>String::new()}).collect::<Vec<_>>().join(", ")).unwrap_or_default(),message.date().map(|d|d.to_rfc822()).unwrap_or_default(),message.message_id().unwrap_or("").into(),message.references().map(|r|r.iter().collect::<Vec<_>>().join(" ")).unwrap_or_default()))
}
pub(super) struct Attachment {pub name:String,pub mime:String,pub size:usize}
pub(super) struct Article {pub subject:String,pub author:String,pub date:String,pub id:String,pub references:String,pub body:String,pub attachments:Vec<Attachment>}
fn parser()->mail_parser::MessageParser {mail_parser::MessageParser::new().max_parts(512).max_depth(32).max_encoded_nesting(3)}
pub(super) fn article(bytes:&[u8])->Result<Article,Error> {
 let message=parser().parse(bytes).ok_or(Error::Response)?;
 if message.parts().any(|p|p.flags().contains(mail_parser::PartFlags::LIMIT_REACHED)){return Err(Error::TooLarge);}
 let (subject,author,date,id,references)=article_header(bytes)?;let mut body=String::new();
 for text in message.text_bodies() {if body.len()+text.len()>MAX_BYTES{return Err(Error::TooLarge);}if !body.is_empty(){body.push_str("\n\n");}body.push_str(&text);}
 // MessageParser supplies text from HTML-only messages without evaluating it.
 let attachments=message.attachments().enumerate().map(|(i,p)|Attachment {name:p.attachment_name().map(str::to_owned).unwrap_or_else(||format!("attachment-{}",i+1)),mime:p.content_type().map(|t|format!("{}/{}",t.ctype(),t.subtype().unwrap_or("octet-stream"))).filter(|s|s.parse::<mime::Mime>().is_ok()).unwrap_or_else(||"application/octet-stream".into()),size:p.decoded_len()}).collect();
 Ok(Article {subject,author,date,id,references,body,attachments})
}
pub(super) fn attachment(bytes:&[u8],n:usize)->Result<(String,String,Vec<u8>),Error> {
 let message=parser().parse(bytes).ok_or(Error::Response)?;
 if message.parts().any(|p|p.flags().contains(mail_parser::PartFlags::LIMIT_REACHED)){return Err(Error::TooLarge);}
 let part=message.attachments().nth(n.checked_sub(1).ok_or(Error::Address)?).ok_or(Error::Address)?;
 let (decoded,problems)=part.decoded_checked();if !problems.is_empty(){return Err(Error::Response);}
 let name=part.attachment_name().unwrap_or("attachment").rsplit(['/', '\\']).next().unwrap_or("attachment").chars().filter(|c|!c.is_control()).take(128).collect::<String>();
 let mime=part.content_type().map(|t|format!("{}/{}",t.ctype(),t.subtype().unwrap_or("octet-stream"))).filter(|s|s.parse::<mime::Mime>().is_ok()).unwrap_or_else(||"application/octet-stream".into());
 Ok((if name.is_empty(){"attachment".into()}else{name},mime,decoded.into_owned()))
}
pub(super) async fn open(target:&Target,server:&Server,download:bool)->Result<Reply,Error> {
 let endpoint=server.endpoint()?;
 let socket=TcpStream::connect((endpoint.host_str().ok_or(Error::Address)?.trim_matches(['[',']']),server.port)).await.map_err(|_|Error::Transport)?;
 let mut wire:Wire=Box::new(socket);if server.transport==Transport::Tls {wire=encrypt(wire,server).await?;}
 let mut session=Session {io:BufReader::new(wire),bytes:0};let greeting=session.status().await?;
 if !matches!(greeting.code,200|201){return Ok(failure(greeting));}
 let mut caps=match session.capabilities().await? {Ok(caps)=>caps,Err(status)=>{session.finish().await;return Ok(failure(status));}};
 if let Some(status)=session.reader(&mut caps).await?{session.finish().await;return Ok(failure(status));}
 if server.transport==Transport::Starttls {
  let status=session.command("STARTTLS").await?;
  if status.code!=382{session.finish().await;return Ok(failure(status));}
  if !session.io.buffer().is_empty(){return Err(Error::Response);}
  session.io=BufReader::new(encrypt(session.io.into_inner(),server).await?);
  caps=match session.capabilities().await? {Ok(caps)=>caps,Err(status)=>{session.finish().await;return Ok(failure(status));}};
  if let Some(status)=session.reader(&mut caps).await?{session.finish().await;return Ok(failure(status));}
 }
 let reply=match &target.kind {
  Kind::Groups(pattern)=>{
   let status=session.command(&format!("LIST ACTIVE {pattern}")).await?;
   if status.code!=215{failure(status)}else {
    let mut groups=Vec::new();let mut truncated=false;
    loop {let mut line=session.line(LINE).await?;if line==b"."{break;}if line.starts_with(b".."){line.remove(0);}
     if groups.len()==GROUPS{truncated=true;break;}
     let text=std::str::from_utf8(&line).map_err(|_|Error::Response)?;let fields=text.split_whitespace().collect::<Vec<_>>();if fields.len()!=4 || !group(fields[0],false){return Err(Error::Response);}
     groups.push(Group {name:fields[0].into(),high:number(fields[1]).map_err(|_|Error::Response)?,low:number(fields[2]).map_err(|_|Error::Response)?,status:fields[3].into()});
    }
    // This explicitly labelled catalogue preview stops an unfinished block by
    // closing it; another command cannot be issued until its dot terminator.
    if truncated{return Ok(Reply::Groups {groups,truncated});}Reply::Groups {groups,truncated}
   }
  },
  Kind::Group(name,article)=>{
   let status=session.command(&format!("GROUP {name}")).await?;
   if status.code!=211{failure(status)}else if let Some(n)=article {return read_article(session,&n.to_string(),download && target.part.is_none()).await;}
   else {
    let fields=status.text.split_whitespace().collect::<Vec<_>>();if fields.len()<4 || !group(fields[3],false){return Err(Error::Response);}
    let count=number(fields[0]).map_err(|_|Error::Response)?;let low=number(fields[1]).map_err(|_|Error::Response)?;let high=number(fields[2]).map_err(|_|Error::Response)?;
    let end=target.before.map(|n|n.saturating_sub(1)).unwrap_or(high).min(high);let start=end.saturating_sub(PAGE-1).max(low).max(1);let mut items=Vec::new();
    if count>0 && end>=start {
     let mut status=session.command(&format!("OVER {start}-{end}")).await?;
     if matches!(status.code,500|501){status=session.command(&format!("XOVER {start}-{end}")).await?;}
     if status.code==224 {
      let rows=session.block(PAGE as usize).await?;let mut ordered=BTreeMap::new();
      for row in rows {let item=overview(&row,start,end)?;let n=item.number;if ordered.insert(n,item).is_some(){return Err(Error::Response);}}items=ordered.into_values().rev().collect();
     }else if matches!(status.code,500|501) {
      for n in (start..=end).rev(){let status=session.command(&format!("HEAD {n}")).await?;
       if matches!(status.code,423|430){continue;}if status.code!=221{session.finish().await;return Ok(failure(status));}
       let bytes=session.block(2048).await?.into_iter().flat_map(|r|[r,b"\r\n".to_vec()].concat()).collect::<Vec<_>>();
       let (subject,author,date,id,references)=article_header(&bytes)?;items.push(Overview {number:n,subject,author,date,id,references});
      }
     }else if status.code!=423 {session.finish().await;return Ok(failure(status));}
    }
    Reply::Overview {name:fields[3].into(),low,high,start,end,items}
   }
  },
  Kind::Article(id)=>return read_article(session,&format!("<{id}>"),download && target.part.is_none()).await,
 };
 session.finish().await;Ok(reply)
}
async fn read_article(mut session:Session,selector:&str,download:bool)->Result<Reply,Error> {
 let status=session.command(&format!("ARTICLE {selector}")).await?;
 if status.code!=220{session.finish().await;return Ok(failure(status));}
 let mut fields=status.text.split_whitespace();let n=number(fields.next().ok_or(Error::Response)?).map_err(|_|Error::Response)?;
 let id=fields.next().ok_or(Error::Response)?;
 if !id.starts_with('<') || !id.ends_with('>') || !message_id(&id[1..id.len()-1]) || (selector.starts_with('<') && id!=selector) || (!selector.starts_with('<') && number(selector)?!=n){return Err(Error::Response);}
 if download {
  let (send,receive)=tokio::sync::oneshot::channel();
  let reader=ArticleReader {inner:Some(super::gopher::DotReader::new(session.io)),send:Some(send)};
  let stream=super::transfer::Transfer::new("message/rfc822".into(),None,reader).with_completion(async move {let io=receive.await.map_err(|_|Error::Transport)?;let mut session=Session {io,bytes:0};session.finish().await;Ok(())});
  return Ok(Reply::Stream(stream));
 }
 let rows=session.block(65536).await?;let mut bytes=Vec::new();for row in rows {bytes.extend_from_slice(&row);bytes.extend_from_slice(b"\r\n");}
 session.finish().await;Ok(Reply::Article(bytes))
}
struct ArticleReader {inner:Option<super::gopher::DotReader<BufReader<Wire>>>,send:Option<tokio::sync::oneshot::Sender<BufReader<Wire>>>}
impl AsyncRead for ArticleReader {
 fn poll_read(self:std::pin::Pin<&mut Self>,cx:&mut std::task::Context<'_>,out:&mut tokio::io::ReadBuf<'_>)->std::task::Poll<std::io::Result<()>> {
  let this=self.get_mut();if out.remaining()==0{return std::task::Poll::Ready(Ok(()));}let Some(reader)=this.inner.as_mut() else{return std::task::Poll::Ready(Ok(()));};let before=out.filled().len();
  let result=std::pin::Pin::new(reader).poll_read(cx,out);
  if matches!(result,std::task::Poll::Ready(Ok(()))) && out.filled().len()==before {let io=this.inner.take().unwrap().into_inner();if let Some(send)=this.send.take(){let _=send.send(io);}}result
 }
}

impl super::Handler {
 pub(super) async fn configure_news(&self,owner:String,url:&Url,context:&servo::protocol_handler::FetchContext)->Result<Url,Error> {
  let target=Target::parse(url)?;
  let selected=self.0.news.get(&target.url)?.unwrap_or(Server {host:String::new(),port:563,transport:Transport::Tls,ca_file:None});
  let mut modes=vec![selected.transport];for mode in [Transport::Tls,Transport::Starttls,Transport::Plain]{if !modes.contains(&mode){modes.push(mode);}}
  let spec=serde_json::json!({"title":"News server","origin":url.as_str(),"submit_label":"Use server","description":"TLS verifies the server certificate. STARTTLS requires a successful upgrade. Unencrypted connections send news requests in plain text.","fields":[
   {"kind":"text","label":"Server host name","value":selected.host,"required":true,"options":[]},
   {"kind":"text","label":"Port (TLS usually 563; NNTP usually 119)","value":selected.port.to_string(),"required":true,"options":[]},
   {"kind":"choice","label":"Connection","value":"","options":modes.iter().map(|m|m.label()).collect::<Vec<_>>()},
   {"kind":"text","label":"Additional trusted CA PEM file (optional)","value":selected.ca_file.as_ref().map(|p|p.to_string_lossy().into_owned()).unwrap_or_default(),"options":[]},
   {"kind":"check","label":"Use as default for news links without a server","value":"1","options":[]}]});
  let answer=self.prompt_with_form(owner,String::new(),Some(spec.to_string()),context).await?;
  let fields:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;let fields=fields.as_array().filter(|f|f.len()==5).ok_or(Error::Address)?;
  let value=|i:usize|fields[i]["value"].as_str().ok_or(Error::Address);
  let host=url::Host::parse(value(0)?.trim()).map_err(|_|Error::Address)?.to_string();let port=value(1)?.trim().parse().map_err(|_|Error::Address)?;
  let transport=*modes.get(fields[2]["choice"].as_u64().ok_or(Error::Address)? as usize).ok_or(Error::Address)?;
  let ca=value(3)?.trim();let ca_file=if ca.is_empty(){None}else{let p=PathBuf::from(ca);if !p.is_absolute() || ca.len()>8192 || ca.chars().any(char::is_control){return Err(Error::Address);}Some(p)};
  let server=Server {host,port,transport,ca_file};let next=target.with_server(&server)?;
  self.0.news.remember(server,fields[4]["checked"].as_bool().ok_or(Error::Address)?)?;Ok(next)
 }
 pub(super) async fn filter_news(&self,owner:String,url:&Url,context:&servo::protocol_handler::FetchContext)->Result<Url,Error> {
  let target=Target::parse(url)?;let pattern=match &target.kind {Kind::Groups(p)|Kind::Group(p,_)=>p.as_str(),_=>"*"};
  let spec=serde_json::json!({"title":"News group filter","origin":url.as_str(),"submit_label":"Show groups","description":"Use a group name, * for any text, or ? for one character.","fields":[{"kind":"text","label":"Group name or pattern","value":pattern,"required":true,"options":[]}]});
  let answer=self.prompt_with_form(owner,String::new(),Some(spec.to_string()),context).await?;let fields:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;
  let pattern=fields.as_array().filter(|f|f.len()==1).and_then(|f|f[0]["value"].as_str()).ok_or(Error::Address)?;if !group(pattern,true){return Err(Error::Address);}Ok(target.group_url(pattern))
 }
 pub(super) async fn retrieve_news(&self,url:&mut Url,request:&servo::protocol_handler::Request,context:&servo::protocol_handler::FetchContext)->Result<super::Resource,Error> {
  use super::{Content,Resource,ActionKind};
  let target=Target::parse(url)?;let owner=request.target_webview_id.as_ref().map(|id|format!("{id:?}")).ok_or(Error::Cancelled)?;
  let Some(server)=self.0.news.get(&target.url)? else {let next=self.configure_news(owner,url,context).await?;return Ok(Resource::Redirect(next));};
  if url.host_str().is_none(){return Ok(Resource::Redirect(target.with_server(&server)?));}
  let download=request.initiator==servo::protocol_handler::Initiator::Download;
  let reply=super::cancellable(open(&target,&server,download),context,20).await?;
  match reply {
   Reply::Stream(stream)=>Ok(Resource::Stream(stream)),
   Reply::Article(bytes) if target.part.is_some()=>{
    let (name,mime,bytes)=attachment(&bytes,target.part.unwrap())?;
    if download {let mut content=Content::raw(bytes,mime);content.filename=Some(name);Ok(Resource::Document(content))}
    else {self.save_resource(url,&mime,bytes,name,owner,context).await}
   },
   reply=>{
    let settings=self.action_link(url,"News server",Some(&owner),ActionKind::NewsServer);
    let filter=self.action_link(url,"Filter groups",Some(&owner),ActionKind::NewsFilter);
    let html=super::document::news(&target,&server,&reply,settings.as_ref(),filter.as_ref())?;
    let status=if let Reply::Failure(code,_) = &reply {match code {411|423|430=>404,480..=483|502=>401,400|403=>503,_=>400}}else{200};
    let mut content=if download {match reply {Reply::Article(bytes)=>Content::raw(bytes,"message/rfc822".into()),_=>Content::raw(mail_parser::html_to_text(&html).into_bytes(),"text/plain; charset=utf-8".into())}}else{Content::html(html)};
    content.status=status;Ok(Resource::Document(content))
   },
  }
 }
}

#[cfg(test)]
mod tests {
 use super::*;
 #[test]
 fn rfc_5538_resources_preserve_case_escaped_segments_and_local_page_state() {
  let id=Target::parse(&Url::parse("news:AbC%2Fdef%3Fghi%23j%25x@Example.invalid").unwrap()).unwrap();
  assert_eq!(id.kind,Kind::Article("AbC/def?ghi#j%x@Example.invalid".into()));
  let server=Server {host:"localhost".into(),port:563,transport:Transport::Tls,ca_file:None};let resolved=id.with_server(&server).unwrap();assert_eq!(Target::parse(&resolved).unwrap().kind,id.kind);
  let t=Target::parse(&Url::parse("nntp://host/example.group.n%2Fa/0007?part=1#section").unwrap()).unwrap();assert_eq!(t.kind,Kind::Group("example.group.n/a".into(),Some(7)));assert_eq!(t.part,Some(1));
  assert_eq!(Target::parse(&t.with_server(&server).unwrap()).unwrap().kind,t.kind);
  let groups=Target::parse(&Url::parse("news:example.th%3Fse.*").unwrap()).unwrap();assert_eq!(groups.kind,Kind::Groups("example.th?se.*".into()));
  let t=Target::parse(&Url::parse("news://host/comp.test?before=101").unwrap()).unwrap();assert_eq!(t.before,Some(101));
  assert_eq!(Target::parse(&t.article_url("comp.test",7)).unwrap().kind,Kind::Group("comp.test".into(),Some(7)));
 }
 #[test]
 fn malformed_uris_do_not_become_commands_or_different_resources() {
  for text in ["nntp:comp.test","nntp://host/","nntp://host/comp/0","nntp://host/comp/1/2","news://host/comp?before=0","news://host/*?before=1","news://host/comp?part=1","news://host/x@id?before=1","news://host/x%0D%0AQUIT@id","news://host/%3Cx@id%3E","news://host/%ff","news://host/%zz","news://host/comp?before=1&before=2","news://host/comp?unknown=1","news://host:25/comp","news://host:0/comp"] {assert!(Target::parse(&Url::parse(text).unwrap()).is_err(),"{text}");}
  assert!(Target::parse(&Url::parse("news://host:563/comp").unwrap()).is_ok());
 }
 #[test]
 fn settings_save_an_explicit_default_and_keep_corrupt_edits() {
  let path=std::env::temp_dir().join(format!("dome-news-settings-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&path).unwrap();let file=path.join("news-servers.json");let store=Store::new(Some(file.clone()));
  assert!(store.get(&Url::parse("news:comp.test").unwrap()).unwrap().is_none());
  let s=Server {host:"localhost".into(),port:563,transport:Transport::Tls,ca_file:None};store.remember(s,true).unwrap();
  let got=Store::new(Some(file.clone())).get(&Url::parse("news:comp.test").unwrap()).unwrap().unwrap();assert_eq!(got.transport,Transport::Tls);
  use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode()&0o777,0o600);
  std::fs::write(&file,"{bad").unwrap();assert!(matches!(store.get(&Url::parse("news:comp.test").unwrap()),Err(Error::NewsSettings)));assert_eq!(std::fs::read_to_string(&file).unwrap(),"{bad");std::fs::remove_dir_all(path).unwrap();
 }
 #[test]
 fn mime_headers_bodies_and_attachments_decode_without_evaluating_html() {
  let bytes=b"From: =?UTF-8?Q?Ren=C3=A9e?= <fixture@example.invalid>\r\nSubject: =?UTF-8?B?SGVsbG8g5LiW55WM?=\r\nMessage-ID: <Case@Example.invalid>\r\nReferences: <parent@example.invalid>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=fixture\r\n\r\n--fixture\r\nContent-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nCaf=E9\r\n--fixture\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename*=utf-8''test%20file.bin\r\nContent-Transfer-Encoding: base64\r\n\r\nAAH/\r\n--fixture--\r\n";
  let a=article(bytes).unwrap();assert_eq!(a.subject,"Hello 世界");assert!(a.author.contains("Renée"));assert!(a.body.contains("Café"));assert_eq!(a.id,"Case@Example.invalid");assert_eq!(a.attachments[0].name,"test file.bin");assert_eq!(attachment(bytes,1).unwrap(),("test file.bin".into(),"application/octet-stream".into(),vec![0,1,255]));
  let html=article(b"Subject: HTML\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hello <b>world</b></p><script>throw Error('fixture')</script>").unwrap();assert!(html.body.contains("Hello"));assert!(!html.body.contains("<script>"));
 }
 #[test]
 fn excessive_mime_structure_is_reported_instead_of_partial_attachment_selection() {
  let mut raw=b"Content-Type: multipart/mixed; boundary=x\r\n\r\n".to_vec();for _ in 0..520 {raw.extend_from_slice(b"--x\r\nContent-Type: text/plain\r\n\r\nx\r\n");}raw.extend_from_slice(b"--x--\r\n");assert!(matches!(article(&raw),Err(Error::TooLarge)));
 }
 #[test]
 fn overview_fields_decode_encoded_words_and_keep_message_ids_exact() {
  let item=overview(b"7\t=?UTF-8?Q?Caf=C3=A9?=\tFixture <f@example.invalid>\tThu, 1 Jan 2026 00:00:00 +0000\t<Case@Example.invalid>\t<parent@id>\t123\t5",1,50).unwrap();assert_eq!(item.subject,"Café");assert_eq!(item.id,"Case@Example.invalid");assert!(overview(b"51\ta\tb\tc\t<case@id>\t\t1\t1",1,50).is_err());
 }
 async fn fixture(mode:&'static str)->(Target,Server,tokio::task::JoinHandle<Vec<String>>) {
  let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
  let task=tokio::spawn(async move {
   let (io,_)=listener.accept().await.unwrap();let mut io=BufReader::new(io);io.write_all(b"201 Fixture ready\r\n").await.unwrap();let mut commands=Vec::new();let mut switched=false;
   loop {
    let mut line=String::new();if io.read_line(&mut line).await.unwrap_or(0)==0{break;}
    assert!(line.ends_with("\r\n"));let cmd=line[..line.len()-2].to_owned();commands.push(cmd.clone());
    let response=if cmd=="CAPABILITIES" {if mode=="legacy"{b"500 No capabilities\r\n".to_vec()}else if mode=="switch" && !switched {b"101 Caps\r\nVERSION 2\r\nMODE-READER\r\n.\r\n".to_vec()}else{b"101 Caps\r\nVERSION 2\r\nREADER\r\nOVER\r\n.\r\n".to_vec()}}
     else if cmd=="MODE READER" {switched=true;if mode=="legacy"{b"500 Old reader\r\n".to_vec()}else{b"201 Reader\r\n".to_vec()}}
     else if cmd=="STARTTLS" {b"580 TLS unavailable\r\n".to_vec()}
     else if cmd.starts_with("LIST ACTIVE "){b"215 Groups\r\ncomp.test 101 1 y\r\n.\r\n".to_vec()}
     else if cmd=="GROUP comp.test" {match mode {"empty0"=>b"211 0 0 0 comp.test\r\n".to_vec(),"empty-low"=>b"211 0 2 1 comp.test\r\n".to_vec(),"empty-equal"=>b"211 0 1 1 comp.test\r\n".to_vec(),_=>b"211 101 1 101 comp.test\r\n".to_vec()}}
     else if cmd.starts_with("OVER ") {if matches!(mode,"legacy"|"head"){b"500 No overview\r\n".to_vec()}else{b"224 Overview\r\n101\tFixture\tAuthor\tDate\t<fixture@id>\t\t12\t1\r\n.\r\n".to_vec()}}
     else if cmd.starts_with("XOVER "){if mode=="head"{b"500 No old overview\r\n".to_vec()}else{b"224 Overview\r\n101\tFixture\tAuthor\tDate\t<fixture@id>\t\t12\t1\r\n.\r\n".to_vec()}}
     else if cmd.starts_with("HEAD "){let n=cmd[5..].parse::<u64>().unwrap();if n==101{b"221 101 <fixture@id> Headers\r\nSubject: Fixture\r\nFrom: Author <f@id>\r\nMessage-ID: <fixture@id>\r\n.\r\n".to_vec()}else{b"423 No article\r\n".to_vec()}}
     else if cmd.starts_with("ARTICLE "){if mode=="truncated"{io.write_all(b"220 101 <fixture@id> Article\r\nSubject: Fixture\r\n\r\nPartial\r\n").await.unwrap();break;}else{b"220 101 <fixture@id> Article\r\nSubject: Fixture\r\n\r\n..dot\r\nBody\r\n.\r\n".to_vec()}}
     else if cmd=="QUIT" {if mode=="silent-quit"{let mut end=Vec::new();assert_eq!(io.read_to_end(&mut end).await.unwrap(),0);break;}else{io.write_all(b"205 Bye\r\n").await.unwrap();break;}}
     else{panic!("Unexpected command {cmd}")};
    if io.write_all(&response).await.is_err(){break;}
   }commands
  });
  let target=Target::parse(&Url::parse(&format!("news://127.0.0.1:{port}/comp.test")).unwrap()).unwrap();let server=Server::for_url(&target.url).unwrap();(target,server,task)
 }
 #[tokio::test(flavor="current_thread")]
 async fn modern_legacy_and_mode_switching_readers_retrieve_bounded_overviews() {
  for mode in ["modern","legacy","switch","head"] {
   let (t,s,task)=fixture(mode).await;let reply=open(&t,&s,false).await.unwrap();let commands=task.await.unwrap();
   let Reply::Overview {start,end,items,..}=reply else{panic!("Overview expected")};assert_eq!((start,end),(52,101));assert_eq!(items.len(),1);assert_eq!(items[0].subject,"Fixture");assert_eq!(commands.last().unwrap(),"QUIT");assert!(commands.contains(&"OVER 52-101".into()));
   assert_eq!(commands.iter().filter(|s|s.as_str()=="MODE READER").count(),usize::from(matches!(mode,"legacy"|"switch")));
   if mode=="head" {assert_eq!(commands.iter().filter(|s|s.starts_with("HEAD ")).count(),50);}
  }
 }
 #[tokio::test(flavor="current_thread")]
 async fn all_standard_empty_group_forms_do_not_request_nonexistent_articles() {
  for mode in ["empty0","empty-low","empty-equal"] {let (t,s,task)=fixture(mode).await;let reply=open(&t,&s,false).await.unwrap();assert!(matches!(reply,Reply::Overview {items,..} if items.is_empty()));let commands=task.await.unwrap();assert!(!commands.iter().any(|s|s.starts_with("OVER ")));}
 }
 #[tokio::test(flavor="current_thread")]
 async fn article_dot_completion_and_quit_do_not_wait_for_peer_eof() {
  for mode in ["modern","silent-quit"] {let (mut t,s,task)=fixture(mode).await;t.kind=Kind::Article("fixture@id".into());let reply=tokio::time::timeout(std::time::Duration::from_secs(2),open(&t,&s,false)).await.unwrap().unwrap();let Reply::Article(bytes)=reply else{panic!("Article expected")};assert!(bytes.ends_with(b".dot\r\nBody\r\n"));assert_eq!(task.await.unwrap().last().unwrap(),"QUIT");}
 }
 #[tokio::test(flavor="current_thread")]
 async fn incomplete_article_is_not_a_successful_resource() {
  let (mut t,s,task)=fixture("truncated").await;t.kind=Kind::Article("fixture@id".into());assert!(matches!(open(&t,&s,false).await,Err(Error::Response)));task.await.unwrap();
 }
 #[tokio::test(flavor="current_thread")]
 async fn configured_starttls_never_falls_back_to_plain_retrieval() {
  let (t,mut s,task)=fixture("modern").await;s.transport=Transport::Starttls;assert!(matches!(open(&t,&s,false).await.unwrap(),Reply::Failure(580,_)));let commands=task.await.unwrap();assert!(commands.contains(&"STARTTLS".into()));assert!(!commands.iter().any(|s|s.starts_with("GROUP ") || s.starts_with("ARTICLE ")));
 }
 #[tokio::test(flavor="current_thread")]
 async fn cancelling_an_incomplete_article_releases_its_live_connection() {
  let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();let (started,ready)=tokio::sync::oneshot::channel();
  let server=tokio::spawn(async move {
   let (io,_)=listener.accept().await.unwrap();let mut io=BufReader::new(io);io.write_all(b"201 Fixture\r\n").await.unwrap();let mut line=String::new();io.read_line(&mut line).await.unwrap();assert_eq!(line,"CAPABILITIES\r\n");io.write_all(b"101 Caps\r\nVERSION 2\r\nREADER\r\n.\r\n").await.unwrap();
   line.clear();io.read_line(&mut line).await.unwrap();assert_eq!(line,"ARTICLE <slow@id>\r\n");io.write_all(b"220 0 <slow@id> Article\r\nSubject: Pending\r\n\r\nWaiting\r\n").await.unwrap();let _=started.send(());
   let result=io.read_u8().await;assert!(result.is_err());
  });
  let target=Target::parse(&Url::parse(&format!("news://127.0.0.1:{port}/slow@id")).unwrap()).unwrap();let settings=Server::for_url(&target.url).unwrap();let load=tokio::spawn(async move {open(&target,&settings,false).await});
  ready.await.unwrap();load.abort();assert!(load.await.is_err());tokio::time::timeout(std::time::Duration::from_secs(1),server).await.unwrap().unwrap();
 }
}
