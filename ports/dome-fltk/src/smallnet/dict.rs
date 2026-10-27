// SPDX-License-Identifier: GPL-3.0-or-later
//! Anonymous, read-only DICT browsing (RFC 2229).
use super::{Error,MAX_BYTES,normalize_address};
use tokio::{io::{AsyncBufReadExt,AsyncWriteExt,BufReader},net::TcpStream};
use url::Url;

const MAX_LINE:usize=6144;
const MAX_ITEMS:usize=1024;
#[derive(Clone,Copy,Debug,PartialEq)]
pub(super) enum Kind { Define, Match }
#[derive(Clone,Debug,PartialEq)]
pub(super) struct Query {pub kind:Kind,pub word:String,pub database:String,pub strategy:String,pub nth:Option<usize>}
pub(super) struct Target {pub url:Url,pub query:Option<Query>}
#[derive(Debug)]
pub(super) struct Definition {pub word:String,pub database:String,pub description:String,pub text:String}
#[derive(Debug)]
pub(super) enum Reply {Index {databases:Vec<(String,String)>,strategies:Vec<(String,String)>},Definitions(Vec<Definition>),Matches(Vec<(String,String)>),Failure(u16,String)}
impl Reply {
 pub(super) fn plain_text(&self)->String {
  match self {
   Self::Index {databases,strategies}=>format!("Databases\n{}\nMatching strategies\n{}",databases.iter().map(|(id,text)|format!("{id}: {text}\n")).collect::<String>(),strategies.iter().map(|(id,text)|format!("{id}: {text}\n")).collect::<String>()),
   Self::Definitions(defs)=>defs.iter().map(|d|format!("{} — {} ({})\n{}\n",d.word,d.description,d.database,d.text)).collect(),
   Self::Matches(rows)=>rows.iter().map(|(db,word)|format!("{db}: {word}\n")).collect(),
   Self::Failure(code,text)=>format!("DICT {code}: {text}\n"),
  }
 }
}

fn atom(text:&str)->bool {!text.is_empty() && !text.chars().any(|c|c.is_control() || matches!(c,' '|'\''|'"'|'\\'))}
fn decode(text:&str)->Result<String,Error> {
 let bytes=text.as_bytes();
 for (i,b) in bytes.iter().enumerate() {if *b==b'%' && (i+2>=bytes.len() || !bytes[i+1..i+3].iter().all(u8::is_ascii_hexdigit)){return Err(Error::Address);}}
 let text=percent_encoding::percent_decode_str(text).decode_utf8().map_err(|_|Error::Address)?.into_owned();
 if text.chars().any(char::is_control){return Err(Error::Address);}Ok(text)
}
fn quote(text:&str)->Result<String,Error> {
 if text.chars().any(char::is_control){return Err(Error::Address);}
 Ok(format!("\"{}\"",text.replace('\\',"\\\\").replace('"',"\\\"")))
}
impl Query {
 pub(super) fn command(&self)->Result<String,Error> {
  if self.word.is_empty() || !atom(&self.database) || !atom(&self.strategy){return Err(Error::Address);}
  let word=quote(&self.word)?;
  let command=match self.kind {Kind::Define=>format!("DEFINE {} {word}\r\n",self.database),Kind::Match=>format!("MATCH {} {} {word}\r\n",self.database,self.strategy)};
  if command.chars().count()>1024 || command.len()>MAX_LINE {return Err(Error::Address);}Ok(command)
 }
 pub(super) fn url(&self,base:&Url)->Url {
  let enc=|s:&str|percent_encoding::utf8_percent_encode(s,percent_encoding::NON_ALPHANUMERIC).to_string();
  let mut path=match self.kind {Kind::Define=>format!("/d:{}:{}",enc(&self.word),enc(&self.database)),Kind::Match=>format!("/m:{}:{}:{}",enc(&self.word),enc(&self.database),enc(&self.strategy))};
  if let Some(nth)=self.nth {path.push_str(&format!(":{nth}"));}
  let mut url=base.clone();url.set_path(&path);url.set_query(None);url.set_fragment(None);url
 }
}
impl Target {
 pub(super) fn parse(url:&Url)->Result<Self,Error> {
  if url.scheme()!="dict" || url.host_str().is_none() || url.query().is_some() || url.as_str().len()>16384 {return Err(Error::Address);}
  if !url.username().is_empty() || url.password().is_some(){return Err(Error::Unsupported);}
  let mut url=url.clone();normalize_address(&mut url)?;if url.port()==Some(0){return Err(Error::Address);}
  let path=url.path();
  let query=if path.is_empty() || path=="/" {None} else {
   let fields=path.strip_prefix('/').ok_or(Error::Address)?.split(':').collect::<Vec<_>>();
   let kind=match fields[0] {"d"=>Kind::Define,"m"=>Kind::Match,_=>return Err(Error::Address)};
   let last=if kind==Kind::Define{3}else{4};
   if fields.len()<2 || fields.len()>last+1 {return Err(Error::Address);}
   let database=fields.get(2).filter(|s|!s.is_empty()).map(|s|decode(s)).transpose()?.unwrap_or_else(||"!".into());
   let strategy=if kind==Kind::Match {fields.get(3).filter(|s|!s.is_empty()).map(|s|decode(s)).transpose()?.unwrap_or_else(||".".into())}else{".".into()};
   let nth=fields.get(last).filter(|s|!s.is_empty()).map(|s| {
    if !s.bytes().all(|b|b.is_ascii_digit()){return Err(Error::Address);}
    s.parse::<usize>().ok().filter(|n|*n>0).ok_or(Error::Address)
   }).transpose()?;
   let query=Query {kind,word:decode(fields[1])?,database,strategy,nth};
   if !atom(&query.database) || !atom(&query.strategy){return Err(Error::Address);}
   // Empty words are a local query-form convenience, never a wire command.
   if !query.word.is_empty(){query.command()?;}Some(query)
  };
  Ok(Self {url,query})
 }
}

struct Session {io:BufReader<TcpStream>,bytes:usize}
impl Session {
 async fn line(&mut self)->Result<String,Error> {
  let mut line=Vec::new();
  loop {
   let available=self.io.fill_buf().await.map_err(|_|Error::Transport)?;
   if available.is_empty(){return Err(Error::Response);}
   let take=available.iter().position(|b|*b==b'\n').map(|i|i+1).unwrap_or(available.len());
   if line.len()+take>MAX_LINE || self.bytes+take>MAX_BYTES {return Err(Error::TooLarge);}
   line.extend_from_slice(&available[..take]);self.io.consume(take);self.bytes+=take;
   if line.last()==Some(&b'\n'){break;}
  }
  if !line.ends_with(b"\r\n"){return Err(Error::Response);}
  line.truncate(line.len()-2);
  let line=String::from_utf8(line).map_err(|_|Error::Response)?;
  if line.chars().count()+2>1024{return Err(Error::Response);}Ok(line)
 }
 async fn status(&mut self)->Result<(u16,String),Error> {
  let line=self.line().await?;if line.chars().any(|c|c.is_control() && c!='\t'){return Err(Error::Response);}let bytes=line.as_bytes();
  if bytes.len()<3 || !bytes[..3].iter().all(u8::is_ascii_digit) || (bytes.len()>3 && bytes[3]!=b' '){return Err(Error::Response);}
  let code=line[..3].parse().map_err(|_|Error::Response)?;
  if !(100..600).contains(&code){return Err(Error::Response);}
  Ok((code,line.get(4..).unwrap_or("").into()))
 }
 async fn send(&mut self,command:&str)->Result<(),Error> {self.io.get_mut().write_all(command.as_bytes()).await.map_err(|_|Error::Transport)}
 async fn complete(&mut self)->Result<(),Error> {if self.status().await?.0==250{Ok(())}else{Err(Error::Response)}}
 async fn text(&mut self)->Result<String,Error> {
  let mut text=String::new();
  loop {
   let line=self.line().await?;if line=="."{return Ok(text);}
   if line.starts_with('.') && !line.starts_with(".."){return Err(Error::Response);}
   text.push_str(line.strip_prefix('.').filter(|_|line.starts_with("..")).unwrap_or(&line));text.push('\n');
  }
 }
 async fn listing(&mut self,command:&str,code:u16,empty:u16)->Result<Result<Vec<(String,String)>,(u16,String)>,Error> {
  self.send(command).await?;let status=self.status().await?;
  if status.0==empty{return Ok(Ok(Vec::new()));}
  if status.0>=400{return Ok(Err(status));}
  if status.0!=code{return Err(Error::Response);}
  let count=count(&status.1)?;let text=self.text().await?;
  let rows=rows(&text)?;
  if rows.len()!=count{return Err(Error::Response);}self.complete().await?;Ok(Ok(rows))
 }
 async fn finish(&mut self) {
  // Transfer completion is the command's 250, never peer EOF. QUIT is
  // best-effort and bounded; a slow shutdown must not hide a valid result.
  let _=tokio::time::timeout(std::time::Duration::from_millis(500),async {
   self.send("QUIT\r\n").await?;
   if self.status().await?.0!=221{return Err(Error::Response);}Ok(())
  }).await;
  let _=self.io.get_mut().shutdown().await;
 }
}
fn count(text:&str)->Result<usize,Error> {
 let value=text.split(' ').next().ok_or(Error::Response)?;
 if value.is_empty() || !value.bytes().all(|b|b.is_ascii_digit()){return Err(Error::Response);}
 value.parse::<usize>().ok().filter(|n|*n<=MAX_ITEMS).ok_or(Error::TooLarge)
}
// Status string parameters are atoms or quoted strings, with quoted pairs.
fn token(text:&str)->Result<(String,&str),Error> {
 let text=text.trim_start_matches([' ','\t']);if text.is_empty(){return Err(Error::Response);}
 if let Some(text)=text.strip_prefix('"') {
  let mut token=String::new();let mut chars=text.char_indices();
  while let Some((i,c))=chars.next() {
   match c {
    '"'=>{let rest=&text[i+1..];if !rest.is_empty() && !rest.starts_with([' ','\t']){return Err(Error::Response);}return Ok((token,rest));},
    '\\'=>token.push(chars.next().ok_or(Error::Response)?.1),
    _=>token.push(c),
   }
  }
  Err(Error::Response)
 }else {
  let n=text.find([' ','\t']).unwrap_or(text.len());let value=&text[..n];
  if !atom(value){return Err(Error::Response);}Ok((value.into(),&text[n..]))
 }
}
fn pair(text:&str)->Result<(String,String),Error> {
 let (first,rest)=token(text)?;if !atom(&first){return Err(Error::Response);}
 let rest=rest.trim_start_matches([' ','\t']);
 let second=if rest.starts_with('"') {let (value,tail)=token(rest)?;if !tail.trim().is_empty(){return Err(Error::Response);}value}else{rest.into()};
 if second.is_empty(){return Err(Error::Response);}Ok((first,second))
}
fn rows(text:&str)->Result<Vec<(String,String)>,Error> {
 let mut rows=Vec::new();
 for line in text.lines() {if rows.len()>=MAX_ITEMS{return Err(Error::TooLarge);}rows.push(pair(line)?);}Ok(rows)
}
pub(super) async fn open(target:&Target)->Result<Reply,Error> {
 if target.query.as_ref().is_some_and(|q|q.word.is_empty()){return Err(Error::Address);}
 let host=target.url.host_str().ok_or(Error::Address)?.trim_matches(['[',']']);
 let stream=TcpStream::connect((host,target.url.port().unwrap_or(2628))).await.map_err(|_|Error::Transport)?;
 let mut session=Session {io:BufReader::new(stream),bytes:0};
 let greeting=session.status().await?;
 if greeting.0>=400{return Ok(Reply::Failure(greeting.0,greeting.1));}
 if greeting.0!=220{return Err(Error::Response);}
 session.send("CLIENT DomeBrowser/0.1\r\n").await?;
 let client=session.status().await?;if client.0!=250{return Ok(Reply::Failure(client.0,client.1));}
 let reply=if let Some(query)=&target.query {
  session.send(&query.command()?).await?;let status=session.status().await?;
  if status.0>=400 {Reply::Failure(status.0,status.1)} else {
   let total=count(&status.1)?;if total==0{return Err(Error::Response);}
   match (query.kind,status.0) {
    (Kind::Define,150)=>{
     let mut definitions=Vec::new();
     for n in 1..=total {
      let (code,parameters)=session.status().await?;if code!=151{return Err(Error::Response);}
      let (word,rest)=token(&parameters)?;let (database,rest)=token(rest)?;let (description,_)=token(rest)?;
      if !atom(&database){return Err(Error::Response);}
      let text=session.text().await?;
      if query.nth.is_none_or(|selected|selected==n){definitions.push(Definition {word,database,description,text});}
     }
     session.complete().await?;Reply::Definitions(definitions)
    },
    (Kind::Match,152)=>{
     let text=session.text().await?;let mut matches=rows(&text)?;
     if matches.len()!=total{return Err(Error::Response);}session.complete().await?;
     if let Some(n)=query.nth {matches=matches.into_iter().nth(n-1).into_iter().collect();}
     Reply::Matches(matches)
    },
    _=>return Err(Error::Response),
   }
  }
 } else {
  match session.listing("SHOW DB\r\n",110,554).await? {
   Err((code,text))=>Reply::Failure(code,text),
   Ok(databases)=>match session.listing("SHOW STRAT\r\n",111,555).await? {
    Err((code,text))=>Reply::Failure(code,text),Ok(strategies)=>Reply::Index {databases,strategies},
   },
  }
 };
 session.finish().await;Ok(reply)
}

#[cfg(test)]
mod tests {
 use super::*;
 #[test]
 fn rfc_uri_defaults_numbered_results_and_quoted_unicode_commands() {
  for (uri,expected) in [("dict://HOST/d:sun", "DEFINE ! \"sun\"\r\n"),("dict://host/m:sun::soundex:1","MATCH ! soundex \"sun\"\r\n"),("dict://host/m:two%20words:wn::2","MATCH wn . \"two words\"\r\n")] {
   assert_eq!(Target::parse(&Url::parse(uri).unwrap()).unwrap().query.unwrap().command().unwrap(),expected);
  }
  let target=Target::parse(&Url::parse("dict://host/d:%E4%B8%96%E7%95%8C%3A%22%5C:wn:1#part").unwrap()).unwrap();
  let q=target.query.unwrap();assert_eq!(q.word,"世界:\"\\");assert_eq!(q.nth,Some(1));assert_eq!(Target::parse(&q.url(&target.url)).unwrap().query,Some(q));
 }
 #[test]
 fn uri_and_command_bounds_and_optional_auth_are_explicit() {
  for uri in ["dict://host/d:x:bad%20db","dict://host/m:x:db:bad%22strategy","dict://host/d:x::0","dict://host/d:x::one","dict://host/d:x::1:extra","dict://host/d:%ff","dict://host/d:%zz","dict://host/d:x?query","dict://host:0/d:x","dict://host:25/d:x"] {assert!(Target::parse(&Url::parse(uri).unwrap()).is_err(),"{uri}");}
  assert!(matches!(Target::parse(&Url::parse("dict://user;AUTH@host/d:word").unwrap()),Err(Error::Unsupported)));
  let mut q=Query {kind:Kind::Define,word:"界".repeat(1011),database:"!".into(),strategy:".".into(),nth:None};
  assert_eq!(q.command().unwrap().chars().count(),1024);q.word.push('界');assert!(q.command().is_err());
 }
 #[test]
 fn response_parameters_and_unquoted_multiword_match_lists() {
  assert_eq!(pair("wn \"two \\\"words\\\"\"").unwrap(),("wn".into(),"two \"words\"".into()));
  assert_eq!(pair("wn Wide SCSI").unwrap(),("wn".into(),"Wide SCSI".into()));
  for line in ["\"bad db\" word","wn \"unterminated","wn \"word\"tail","wn"] {assert!(pair(line).is_err(),"{line}");}
 }
 async fn fixture(response:&[u8],uri:&str)->(Result<Reply,Error>,Vec<String>) {
  use tokio::{net::TcpListener,io::AsyncBufReadExt};
  let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();let response=response.to_vec();
  let server=tokio::spawn(async move {
   let (io,_)=listener.accept().await.unwrap();let mut io=BufReader::new(io);io.get_mut().write_all(b"220 Fixture <mime> <one@localhost>\r\n").await.unwrap();
   let mut lines=Vec::new();let mut line=String::new();io.read_line(&mut line).await.unwrap();lines.push(line);io.get_mut().write_all(b"250 client accepted\r\n").await.unwrap();
   let mut line=String::new();io.read_line(&mut line).await.unwrap();lines.push(line);let _=io.get_mut().write_all(&response).await;
   let mut line=String::new();let _=io.read_line(&mut line).await;if !line.is_empty(){lines.push(line);let _=io.get_mut().write_all(b"221 bye\r\n").await;}lines
  });
  let target=Target::parse(&Url::parse(&format!("dict://127.0.0.1:{port}{uri}")).unwrap()).unwrap();let result=open(&target).await;let lines=server.await.unwrap();(result,lines)
 }
 #[tokio::test(flavor="current_thread")]
 async fn complete_counted_definitions_unstuff_dots_and_select_nth_without_waiting_for_eof() {
  let response=b"150 2 definitions\r\n151 \"word\" wn \"First dictionary\"\r\n..dot\r\nfirst\r\n.\r\n151 \"word\" other \"Second dictionary\"\r\nsecond\r\n.\r\n250 complete\r\n";
  let (result,commands)=fixture(response,"/d:word:*:2").await;
  let Reply::Definitions(defs)=result.unwrap() else{panic!()};assert_eq!(defs.len(),1);assert_eq!(defs[0].text,"second\n");assert_eq!(commands,["CLIENT DomeBrowser/0.1\r\n","DEFINE * \"word\"\r\n","QUIT\r\n"]);
  let (result,_)=fixture(response,"/d:word:*").await;let Reply::Definitions(defs)=result.unwrap() else{panic!()};assert_eq!(defs[0].text,".dot\nfirst\n");
 }
 #[tokio::test(flavor="current_thread")]
 async fn incomplete_and_mismatched_responses_do_not_become_successful_documents() {
  for response in [b"150 1 definition\r\n151 word wn \"Dictionary\"\r\npartial\r\n".as_slice(),b"152 2 matches\r\nwn word\r\n.\r\n250 complete\r\n",b"152 1 matches\r\nwn word\r\n.\r\n500 failed\r\n",b"152 1000000 matches\r\n",b"152 1 matches\n"] {
   // Close after the fixture response to distinguish EOF from a terminator.
   let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();let response=response.to_vec();
   let server=tokio::spawn(async move {let (io,_)=listener.accept().await.unwrap();let mut io=BufReader::new(io);io.get_mut().write_all(b"220 fixture\r\n").await.unwrap();let mut line=String::new();io.read_line(&mut line).await.unwrap();io.get_mut().write_all(b"250 ok\r\n").await.unwrap();line.clear();io.read_line(&mut line).await.unwrap();let _=io.get_mut().write_all(&response).await;});
   let result=open(&Target::parse(&Url::parse(&format!("dict://127.0.0.1:{port}/m:word")).unwrap()).unwrap()).await;assert!(result.is_err());server.await.unwrap();
  }
 }
 #[tokio::test(flavor="current_thread")]
 async fn numbered_matches_keep_the_dictionary_and_whole_multiword_term() {
  let (reply,commands)=fixture(b"152 2 matches\r\nwn first\r\nother Wide SCSI\r\n.\r\n250 done\r\n","/m:scsi:*:prefix:2").await;
  let Reply::Matches(rows)=reply.unwrap() else{panic!()};assert_eq!(rows,[("other".into(),"Wide SCSI".into())]);assert_eq!(commands[1],"MATCH * prefix \"scsi\"\r\n");
 }
 #[tokio::test(flavor="current_thread")]
 async fn cancelling_a_partial_definition_releases_the_connection() {
  let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
  let (started,receive)=tokio::sync::oneshot::channel();
  let server=tokio::spawn(async move {
   let (io,_)=listener.accept().await.unwrap();let mut io=BufReader::new(io);io.get_mut().write_all(b"220 fixture\r\n").await.unwrap();
   let mut line=String::new();io.read_line(&mut line).await.unwrap();io.get_mut().write_all(b"250 ok\r\n").await.unwrap();line.clear();io.read_line(&mut line).await.unwrap();
   io.get_mut().write_all(b"150 1 definition\r\n151 word wn \"Dictionary\"\r\npartial\r\n").await.unwrap();let _=started.send(());
   let mut line=String::new();let closed=io.read_line(&mut line).await;
   assert!(matches!(closed,Ok(0)) || closed.is_err_and(|e|matches!(e.kind(),std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::ConnectionAborted)));
  });
  let task=tokio::spawn(async move {open(&Target::parse(&Url::parse(&format!("dict://127.0.0.1:{port}/d:word")).unwrap()).unwrap()).await});
  receive.await.unwrap();task.abort();assert!(task.await.unwrap_err().is_cancelled());
  tokio::time::timeout(std::time::Duration::from_secs(1),server).await.unwrap().unwrap();
 }

 #[tokio::test(flavor="current_thread")]
 async fn definition_text_preserves_utf8_control_codepoints() {
  let body="Page one\u{c}page two\u{85}next\u{7f}line\r\n";
  let response=format!("150 1 definition\r\n151 word wn \"Dictionary\"\r\n{body}.\r\n250 complete\r\n");
  let (reply,_)=fixture(response.as_bytes(),"/d:word").await;
  let Reply::Definitions(defs)=reply.unwrap() else{panic!()};
  assert_eq!(defs[0].text,"Page one\u{c}page two\u{85}next\u{7f}line\n");
 }
}
