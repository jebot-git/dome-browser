// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only CCSO/Ph directory queries, RFC 2378, and Gopher type 2 endpoints.
use super::{Error,MAX_BYTES,normalize_address};
use std::collections::BTreeMap;
use tokio::{io::{AsyncBufReadExt,AsyncWriteExt,BufReader},net::TcpStream};
use url::Url;
const LINE_LIMIT:usize=65536;
const ROW_LIMIT:usize=16384;
const ITEM_LIMIT:usize=1024;
#[derive(Clone,Debug,PartialEq)]
pub(super) struct Lookup {pub term:String,pub field:String,pub returns:Vec<String>}
pub(super) struct Target {pub url:Url,pub lookup:Option<Lookup>}
#[derive(Clone,Debug)]
pub(super) struct Row {pub code:i16,pub text:String}
#[derive(Debug)]
pub(super) struct Reply {pub code:u16,pub message:String,pub rows:Vec<Row>,pub index:bool,pub site:Vec<Row>,pub site_status:Option<(u16,String)>,pub latin1:bool}
#[derive(Debug)]
pub(super) struct Field {pub name:String,pub description:String,pub attributes:String}
pub(super) fn attribute(s:&str)->bool {!s.is_empty() && s.len()<=128 && s.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'_'|b'-'))}
fn decode(s:&str)->Result<String,Error> {
 let s=s.replace('+'," ");let bytes=s.as_bytes();
 for (i,b) in bytes.iter().enumerate(){if *b==b'%' && (i+2>=bytes.len() || !bytes[i+1..i+3].iter().all(u8::is_ascii_hexdigit)){return Err(Error::Address);}}
 percent_encoding::percent_decode_str(&s).decode_utf8().map(|s|s.into_owned()).map_err(|_|Error::Address)
}
fn quote(s:&str)->Result<Vec<u8>,Error> {
 let mut bytes=vec![b'"'];
 for c in s.chars() {
  match c {
   '\n'=>bytes.extend_from_slice(b"\\n"),'\t'=>bytes.extend_from_slice(b"\\t"),
   '"'|'\\'=>{bytes.push(b'\\');bytes.push(c as u8);},
   c if (32..127).contains(&(c as u32)) || (160..=255).contains(&(c as u32))=>bytes.push(c as u8),
   c if c as u32>255=>return Err(Error::Unsupported),
   _=>return Err(Error::Address),
  }
 }
 bytes.push(b'"');Ok(bytes)
}
impl Lookup {
 pub(super) fn command(&self)->Result<Vec<u8>,Error> {
  if self.term.is_empty() || (!self.field.is_empty() && !attribute(&self.field)) || self.returns.len()>64 || self.returns.iter().any(|v|!attribute(v)){return Err(Error::Address);}
  let mut bytes=b"query ".to_vec();
  if !self.field.is_empty(){bytes.extend_from_slice(self.field.to_ascii_lowercase().as_bytes());bytes.push(b'=');}
  bytes.extend_from_slice(&quote(&self.term)?);
  if !self.returns.is_empty(){bytes.extend_from_slice(b" return ");bytes.extend_from_slice(self.returns.join(" ").to_ascii_lowercase().as_bytes());}
  bytes.extend_from_slice(b"\r\n");if bytes.len()>4096{return Err(Error::Address);}Ok(bytes)
 }
 pub(super) fn url(&self,base:&Url)->Url {
  let mut url=base.clone();url.set_path("/");url.set_query(None);url.set_fragment(None);
  {let mut pairs=url.query_pairs_mut();pairs.append_pair("q",&self.term);if !self.field.is_empty(){pairs.append_pair("field",&self.field);}if !self.returns.is_empty(){pairs.append_pair("return",&self.returns.join(","));}}
  url
 }
}
impl Target {
 pub(super) fn parse(url:&Url)->Result<Self,Error> {
  if !matches!(url.scheme(),"cso"|"ph") || url.host_str().is_none() || !matches!(url.path(),""|"/") || url.as_str().len()>16384 {return Err(Error::Address);}
  if !url.username().is_empty() || url.password().is_some(){return Err(Error::Unsupported);}
  let mut url=url.clone();normalize_address(&mut url)?;if url.port()==Some(0){return Err(Error::Address);}
  let lookup=if let Some(query)=url.query() {
   let mut values=BTreeMap::new();
   for part in query.split('&') {
    let (key,value)=part.split_once('=').ok_or(Error::Address)?;let key=decode(key)?;
    if !matches!(key.as_str(),"q"|"field"|"return") || values.insert(key,decode(value)?).is_some(){return Err(Error::Address);}
   }
   let term=values.remove("q").ok_or(Error::Address)?;
   let field=values.remove("field").unwrap_or_default();
   let returns=values.remove("return").filter(|v|!v.trim().is_empty()).map(|s|s.split([',',' ','\t']).filter(|s|!s.is_empty()).map(str::to_owned).collect()).unwrap_or_default();
   let lookup=Lookup {term,field,returns};
   if !lookup.term.is_empty(){lookup.command()?;}
   else if (!lookup.field.is_empty() && !attribute(&lookup.field)) || lookup.returns.len()>64 || lookup.returns.iter().any(|s|!attribute(s)){return Err(Error::Address);}
   Some(lookup)
  }else{None};Ok(Self {url,lookup})
 }
}
pub(super) fn gopher_endpoint(url:&Url)->Result<Url,Error> {
 let mut endpoint=Url::parse("cso://example.invalid/").unwrap();endpoint.set_host(url.host_str()).map_err(|_|Error::Address)?;
 // A Gopher URI's omitted port means 70; descriptors normally explicitly use
 // 105. Its selector has no meaning in the pure CSO protocol (RFC 1436).
 endpoint.set_port(Some(url.port().unwrap_or(70))).map_err(|_|Error::Address)?;Target::parse(&endpoint)?;Ok(endpoint)
}
fn ascii_text(bytes:&[u8])->Result<String,Error> {
 if !bytes.is_ascii(){return Err(Error::Response);}
 let mut decoded=Vec::new();let mut i=0;
 while i<bytes.len() {
  if bytes[i]==b'=' && i+2<bytes.len() && bytes[i+1..i+3].iter().all(u8::is_ascii_hexdigit) {
   decoded.push(u8::from_str_radix(std::str::from_utf8(&bytes[i+1..i+3]).unwrap(),16).map_err(|_|Error::Response)?);i+=3;
  }else{decoded.push(bytes[i]);i+=1;}
 }
 Ok(decoded.into_iter().map(char::from).collect())
}
fn row(bytes:&[u8],latin1:bool)->Result<Row,Error> {
 let split=bytes.iter().position(|b|*b==b':').ok_or(Error::Response)?;
 let code=std::str::from_utf8(&bytes[..split]).map_err(|_|Error::Response)?.trim_matches([' ','\t']);
 let digits=code.strip_prefix('-').unwrap_or(code);
 if digits.len()!=3 || !digits.bytes().all(|b|b.is_ascii_digit()){return Err(Error::Response);}
 let code=code.parse::<i16>().map_err(|_|Error::Response)?;if !(100..=699).contains(&code.abs()){return Err(Error::Response);}
 let text=if latin1{bytes[split+1..].iter().copied().map(char::from).collect()}else{ascii_text(&bytes[split+1..])?};
 Ok(Row {code,text})
}
struct Session {io:BufReader<TcpStream>,bytes:usize,latin1:bool}
impl Session {
 async fn line(&mut self)->Result<Vec<u8>,Error> {
  let mut line=Vec::new();
  loop {
   let available=self.io.fill_buf().await.map_err(|_|Error::Transport)?;if available.is_empty(){return Err(Error::Response);}
   let take=available.iter().position(|b|*b==b'\n').map(|i|i+1).unwrap_or(available.len());
   if line.len()+take>LINE_LIMIT || self.bytes+take>MAX_BYTES{return Err(Error::TooLarge);}
   line.extend_from_slice(&available[..take]);self.io.consume(take);self.bytes+=take;if line.last()==Some(&b'\n'){break;}
  }
  // RFC 2378 uses CRLF; deployed Qi servers predating it emit bare LF.
  // Accept that historical response delimiter while always sending CRLF.
  line.pop();if line.last()==Some(&b'\r'){line.pop();}Ok(line)
 }
 async fn command(&mut self,command:&[u8])->Result<(u16,String,Vec<Row>),Error> {
  self.io.get_mut().write_all(command).await.map_err(|_|Error::Transport)?;let mut rows=Vec::new();
  for _ in 0..ROW_LIMIT {
   let reply=row(&self.line().await?,self.latin1)?;
   if reply.code>=200{return Ok((reply.code as u16,reply.text,rows));}
   rows.push(reply);
  }
  Err(Error::TooLarge)
 }
 async fn finish(&mut self) {
  let _=tokio::time::timeout(std::time::Duration::from_millis(500),self.command(b"quit\r\n")).await;
  let _=self.io.get_mut().shutdown().await;
 }
}
pub(super) fn structured(row:&Row)->Option<(usize,&str,&str)> {
 let mut columns=row.text.splitn(3,':');let id=columns.next()?.trim().parse::<usize>().ok()?;let name=columns.next()?.trim();let value=columns.next()?;let text=value.strip_prefix(' ').unwrap_or(value);
 Some((id,name,text))
}
impl Reply {
 pub(super) fn fields(&self)->Result<Vec<Field>,Error> {
  let mut fields:BTreeMap<usize,Field>=BTreeMap::new();
  for row in &self.rows {
   if row.code!=-200{continue;}
   let Some((id,name,text))=structured(row) else{continue;};if !attribute(name){return Err(Error::Response);}
   if !fields.contains_key(&id) && fields.len()>=ITEM_LIMIT{return Err(Error::TooLarge);}
   let field=fields.entry(id).or_insert_with(||Field {name:name.into(),description:String::new(),attributes:String::new()});
   if field.name!=name{return Err(Error::Response);}
   let mut parts=text.split_whitespace();
   if parts.next().is_some_and(|s|s.eq_ignore_ascii_case("max")) && parts.next().is_some_and(|s|!s.is_empty() && s.bytes().all(|b|b.is_ascii_digit())){field.attributes=text.into();}
   else{if !field.description.is_empty(){field.description.push('\n');}field.description.push_str(text);}
  }
  Ok(fields.into_values().collect())
 }
 pub(super) fn records(&self)->Result<Vec<(usize,Vec<(&str,&str,i16)>)>,Error> {
  let mut records:BTreeMap<usize,Vec<(&str,&str,i16)>>=BTreeMap::new();
  for row in &self.rows {
   if row.code>-200{continue;}
   if let Some((id,name,text))=structured(row) {
    if !records.contains_key(&id) && records.len()>=ITEM_LIMIT{return Err(Error::TooLarge);}
    records.entry(id).or_default().push((name,text,row.code));
   }
  }
  Ok(records.into_iter().collect())
 }
 pub(super) fn plain_text(&self)->String {
  let mut text=format!("CSO {}: {}\n",self.code,self.message);
  for row in &self.rows {if let Some((id,name,value))=structured(row){text.push_str(&format!("{id}. {name}{}: {value}\n",if row.code==-200{String::new()}else{format!(" (CSO {})",row.code.abs())}));}else{text.push_str(&format!("{}\n",row.text));}}
  if !self.site.is_empty(){text.push_str("\nServer information\n");for row in &self.site {if let Some((_,name,value))=structured(row){text.push_str(&format!("{name}: {value}\n"));}else{text.push_str(&format!("{}\n",row.text));}}}
  text
 }
}
pub(super) async fn open(target:&Target)->Result<Reply,Error> {
 let command=target.lookup.as_ref().map(Lookup::command).transpose()?;
 let host=target.url.host_str().ok_or(Error::Address)?.trim_matches(['[',']']);
 let stream=TcpStream::connect((host,target.url.port().unwrap_or(105))).await.map_err(|_|Error::Transport)?;
 let mut session=Session {io:BufReader::new(stream),bytes:0,latin1:false};
 let (code,message,rows)=session.command(b"set charset=iso-8859-1\r\n").await?;
 if (200..300).contains(&code){session.latin1=true;}
 else if !matches!(code,512..=514){session.finish().await;return Ok(Reply {code,message,rows,index:command.is_none(),site:Vec::new(),site_status:None,latin1:false});}
 let index=command.is_none();let (code,message,rows)=session.command(command.as_deref().unwrap_or(b"fields\r\n")).await?;
 let mut reply=Reply {code,message,rows,index,site:Vec::new(),site_status:None,latin1:session.latin1};
 if index && (200..300).contains(&code) {
  let (code,message,rows)=session.command(b"siteinfo\r\n").await?;reply.site=rows;reply.site_status=Some((code,message));
 }
 if index {reply.fields()?;}else{reply.records()?;}
 session.finish().await;Ok(reply)
}

#[cfg(test)]
mod tests {
 use super::*;
 #[test]
 fn public_uri_forms_and_readonly_latin1_quoting_roundtrip() {
  let t=Target::parse(&Url::parse("ph://HOST:105/?q=Ren%C3%A9e+%22two%22&field=Name&return=name%2Cemail#part").unwrap()).unwrap();let q=t.lookup.unwrap();
  assert_eq!(q.command().unwrap(),b"query name=\"Ren\xe9e \\\"two\\\"\" return name email\r\n");
  assert_eq!(Target::parse(&q.url(&t.url)).unwrap().lookup,Some(q));
  assert_eq!(Lookup {term:"one\ntwo\t\\".into(),field:String::new(),returns:Vec::new()}.command().unwrap(),b"query \"one\\ntwo\\t\\\\\"\r\n");
  assert_eq!(Lookup {term:"non\u{a0}breaking".into(),field:String::new(),returns:Vec::new()}.command().unwrap(),b"query \"non\xa0breaking\"\r\n");
 }
 #[test]
 fn malformed_addresses_and_unrepresentable_lookup_text_are_explicit() {
  for s in ["cso://host/path","cso://host/?q=x&q=y","cso://host/?q=%ff","cso://host/?q=%zz","cso://host/?field=name","ph://host:0/","ph://host:25/","cso://host/?q=x&field=bad%20field","cso://host/?q=x&return=name%3Bquit"] {assert!(Target::parse(&Url::parse(s).unwrap()).is_err(),"{s}");}
  assert!(matches!(Target::parse(&Url::parse("cso://host/?q=%E4%B8%96%E7%95%8C").unwrap()),Err(Error::Unsupported)));
  assert!(matches!(Target::parse(&Url::parse("ph://user@host/").unwrap()),Err(Error::Unsupported)));
 }
 #[test]
 fn signed_reply_codes_and_colon_values_follow_the_ph_framing_rules() {
  let r=row(b" -503:1: email: Not public: use the directory",false).unwrap();assert_eq!(r.code,-503);assert_eq!(structured(&r),Some((1,"email","Not public: use the directory")));
  for s in [b"100:Progress".as_slice(),b"-199:Progress",b"-699:Continuation",b"699:Done",b"201:Read only"] {assert!(row(s,false).is_ok());}
  for s in [b"0:invalid".as_slice(),b"700:invalid",b"+200:invalid",b"200 no separator"] {assert!(row(s,false).is_err());}
 }
 #[test]
 fn fallback_quoted_printable_and_true_latin1_do_not_use_web_charset_aliases() {
  assert_eq!(ascii_text(b"caf=E9 and =3D equals").unwrap(),"café and = equals");
  assert_eq!(row(b"-200:1:name:\x80\xa3\xe9",true).unwrap().text,"1:name:\u{80}£é");
  assert!(ascii_text(b"raw\xe9").is_err());
 }
 async fn serve(fallback:bool,partial:bool)->(Target,tokio::task::JoinHandle<Vec<Vec<u8>>>) {
  let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
  let task=tokio::spawn(async move {
   let (io,_)=listener.accept().await.unwrap();let mut io=BufReader::new(io);let mut commands=Vec::new();
   loop {
    let mut line=Vec::new();if io.read_until(b'\n',&mut line).await.unwrap_or(0)==0{break;}
    let quit=line==b"quit\r\n";commands.push(line.clone());
    let reply:&[u8]=if line.starts_with(b"set "){if fallback{b"513:Unknown option\r\n"}else{b"200:Done\r\n"}}else if quit{b"200:Bye\r\n"}else if partial{b"102:Progress\r\n-200:1:name: Partial\r\n"}else if fallback{b"102:One match\r\n-200:1:name: Ren=E9e\r\n-503:1:phone: Not public\r\n200:Done\r\n"}else{b"102:One match\r\n-200:1:name: Ren\xe9e\r\n-503:1:phone: Not public\r\n201:Read only\r\n"};
    if io.get_mut().write_all(reply).await.is_err(){break;}
    if quit || (partial && !line.starts_with(b"set ")){break;}
   }commands
  });
  (Target::parse(&Url::parse(&format!("cso://127.0.0.1:{port}/?q=Ren%C3%A9e")).unwrap()).unwrap(),task)
 }
 #[tokio::test(flavor="current_thread")]
 async fn negotiated_and_fallback_results_finish_before_peer_eof_and_keep_field_errors() {
  for fallback in [false,true] {
   let (target,server)=serve(fallback,false).await;let reply=open(&target).await.unwrap();assert_eq!(reply.latin1,!fallback);
   let records=reply.records().unwrap();assert_eq!(records[0].1[0],("name","Renée",-200));assert_eq!(records[0].1[1],("phone","Not public",-503));
   let commands=server.await.unwrap();assert_eq!(commands[0],b"set charset=iso-8859-1\r\n");assert_eq!(commands[1],b"query \"Ren\xe9e\"\r\n");assert_eq!(commands[2],b"quit\r\n");
  }
 }
 #[tokio::test(flavor="current_thread")]
 async fn legacy_qi_lf_responses_keep_crlf_commands_and_signed_completion() {
  let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
  let server=tokio::spawn(async move {
   let (io,_)=listener.accept().await.unwrap();let mut io=BufReader::new(io);let mut commands=Vec::new();
   // Recorded reply shape from private Qi 3.1: no charset option, LF-only
   // delimiters, nonterminal signed field rows, then positive completion.
   for response in [&b"-513:charset:unknown option\n513:No option recognized.\n"[..],&b"102:There were 2 matches to your request.\n-200:1:           alias: dome-fixture\n-200:1:            name: Dome Fixture\n-200:2:            name: Second Fixture\n200:Ok.\n"[..],&b"200:Bye!\n"[..]] {
    let mut command=Vec::new();if io.read_until(b'\n',&mut command).await.unwrap_or(0)==0{break;}commands.push(command);
    if io.get_mut().write_all(response).await.is_err(){break;}
   }commands
  });
  let target=Target::parse(&Url::parse(&format!("cso://127.0.0.1:{port}/?q=fixture")).unwrap()).unwrap();
  let result=open(&target).await;let commands=server.await.unwrap();let reply=result.unwrap();
  assert!(!reply.latin1);assert_eq!(reply.code,200);let records=reply.records().unwrap();assert_eq!(records.len(),2);assert_eq!(records[0].1[1],("name","Dome Fixture",-200));
  assert_eq!(commands,vec![b"set charset=iso-8859-1\r\n".to_vec(),b"query \"fixture\"\r\n".to_vec(),b"quit\r\n".to_vec()]);
 }
 #[tokio::test(flavor="current_thread")]
 async fn eof_before_a_nonnegative_terminal_code_is_not_a_completed_lookup() {
  let (target,server)=serve(false,true).await;assert!(matches!(open(&target).await,Err(Error::Response)));server.await.unwrap();
 }
 #[tokio::test(flavor="current_thread")]
 async fn cancelling_a_live_directory_response_releases_the_socket() {
  let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();let (started,receive)=tokio::sync::oneshot::channel();
  let server=tokio::spawn(async move {
   let (io,_)=listener.accept().await.unwrap();let mut io=BufReader::new(io);let mut line=Vec::new();io.read_until(b'\n',&mut line).await.unwrap();io.get_mut().write_all(b"200:Done\r\n").await.unwrap();
   line.clear();io.read_until(b'\n',&mut line).await.unwrap();io.get_mut().write_all(b"102:Progress\r\n-200:1:name: Partial\r\n").await.unwrap();let _=started.send(());line.clear();
   let result=io.read_until(b'\n',&mut line).await;assert!(matches!(result,Ok(0)) || result.is_err_and(|e|matches!(e.kind(),std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::ConnectionAborted)));
  });
  let task=tokio::spawn(async move{open(&Target::parse(&Url::parse(&format!("ph://127.0.0.1:{port}/?q=fixture")).unwrap()).unwrap()).await});receive.await.unwrap();task.abort();assert!(task.await.unwrap_err().is_cancelled());
  tokio::time::timeout(std::time::Duration::from_secs(1),server).await.unwrap().unwrap();
 }
 #[test]
 fn gopher_phonebook_descriptors_use_the_csoprotocol_and_keep_the_declared_endpoint() {
  for base in ["gopher://directory/","gophers://directory/"] {
   let base=Url::parse(base).unwrap();
   let html=super::super::document::gopher(&base,"2Phone book\tignored selector\tphone.example\t105\r\n.\r\n");assert!(html.contains("cso://phone.example:105/"));assert!(!html.contains("ignored%20selector"));
  }
  assert_eq!(gopher_endpoint(&Url::parse("gopher://phone.example/2selector").unwrap()).unwrap().as_str(),"cso://phone.example:70/");
 }
 #[test]
 fn schema_groups_field_attributes_and_description_without_assuming_sequential_ids() {
  let reply=Reply {code:200,message:"Done".into(),rows:[b"-200:6: alias: max 32 Indexed Lookup Public".as_slice(),b"-200:6: alias: Name to search",b"-200:3: name: max 64 Indexed Lookup Public",b"-200:3: name: Full name"].iter().map(|b|row(b,false).unwrap()).collect(),index:true,site:Vec::new(),site_status:None,latin1:false};
  let fields=reply.fields().unwrap();assert_eq!(fields.len(),2);assert_eq!(fields[0].name,"name");assert_eq!(fields[1].description,"Name to search");
 }
}
