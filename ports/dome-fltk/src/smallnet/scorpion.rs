// SPDX-License-Identifier: GPL-3.0-or-later
//! Scorpion R retrieval and document framing, from the author's draft specification.
//! https://github.com/zzo38/scorpion/blob/2847eb5f07d1a6e74afe24754829d5e16bcd4fdc/Specification
mod charset;
mod document;
use super::{Error,MAX_BYTES,MAX_REQUEST,normalize_address,transfer};
use tokio::io::{AsyncRead,AsyncReadExt,AsyncWrite,AsyncWriteExt,BufReader};
use url::Url;

const MAX_HEADER:usize=16*1024;

pub(super) fn request_bytes(url:&Url)->Result<Vec<u8>,Error> {
    if !matches!(url.scheme(),"scorpion"|"scorpions") || !url.has_host() || url.port()==Some(0) {return Err(Error::Address);}
    let mut url=url.clone();normalize_address(&mut url)?;url.set_fragment(None);
    if url.path().is_empty(){url.set_path("/");}
    if url.as_str().len()+4>MAX_REQUEST || !url.as_str().is_ascii() || url.as_str().bytes().any(|b|b<=32 || b==127){return Err(Error::Address);}
    Ok(format!("R {url}\r\n").into_bytes())
}

#[derive(Clone,Debug,PartialEq)]
pub(super) struct Metadata {pub length:Option<u64>,pub format:String,pub mime:String,pub version:Option<String>,pub document:bool}
impl Metadata {
    fn parse(text:&str)->Result<Self,Error> {
        let mut parts=text.split(' ').filter(|p|!p.is_empty());
        let length=match parts.next().ok_or(Error::Response)? {"?"=>None,n if !n.is_empty() && n.bytes().all(|b|b.is_ascii_digit())=>Some(n.parse().map_err(|_|Error::Response)?),_=>return Err(Error::Response)};
        let format=parts.next().ok_or(Error::Response)?.to_owned();
        let version=parts.next().map(str::to_owned);
        if parts.next().is_some(){return Err(Error::Response);}
        let (mime,document)=match format.parse::<mime::Mime>() {
            Ok(m)=>{let native=matches!(m.essence_str(),"text/scorpion"|"text/x-scorpion"|"application/scorpion"|"application/x-scorpion");(m.to_string(),native)},
            Err(_)=>{
                // Unknown ULFI types remain downloadable bytes. Compression and
                // nested formats are never mistaken for a native document.
                if !format.bytes().all(|b|b.is_ascii_graphic() && !b"/\\\"'".contains(&b)){return Err(Error::Response);}
                let native=format.split(':').any(|part|part=="scorpion");
                ((if native{"application/x-scorpion"}else if format=="text"{"text/plain; charset=us-ascii"}else{"application/octet-stream"}).into(),native)
            }
        };
        Ok(Self{length,format,mime,version,document})
    }
    fn displayable(&self)->bool {
        self.document || self.mime.parse::<mime::Mime>().is_ok_and(|m|m.type_()==mime::TEXT || m.type_()==mime::IMAGE)
    }
}

#[derive(Debug,PartialEq)]
pub(super) enum Header {File(Metadata),Input(String),Redirect(Url),Failure{status:u8,message:String,retry_after:Option<u32>},Identity{status:u8,scope:String,message:String}}
pub(super) fn header(bytes:&[u8],url:&Url)->Result<Header,Error> {
    if bytes.len()>MAX_HEADER || !bytes.is_ascii(){return Err(Error::Response);}
    let text=std::str::from_utf8(bytes).map_err(|_|Error::Response)?.strip_suffix("\r\n").ok_or(Error::Response)?;
    let b=text.as_bytes();
    if b.len()<3 || !b[0].is_ascii_digit() || !b[1].is_ascii_digit() || b[2]!=b' ' || b.iter().any(|b|*b<32 || *b==127){return Err(Error::Response);}
    let status=(b[0]-b'0')*10+b[1]-b'0';let parameters=&text[3..];
    match status/10 {
        1=>Ok(Header::Input(parameters.into())),
        2 if !matches!(status,21|22)=>Ok(Header::File(Metadata::parse(parameters)?)),
        3 if !parameters.is_empty()=>{
            let mut target=url.join(parameters).map_err(|_|Error::Response)?;
            if target.fragment().is_none(){target.set_fragment(url.fragment());}
            if matches!(target.scheme(),"scorpion"|"scorpions") {
                normalize_address(&mut target)?;if target.path().is_empty(){target.set_path("/");}request_bytes(&target)?;
            }
            Ok(Header::Redirect(target))
        },
        4=>{
            let (delay,message)=parameters.split_once(' ').unwrap_or((parameters,""));
            let retry_after=match delay {"?"=>None,n if !n.is_empty() && n.bytes().all(|b|b.is_ascii_digit())=>Some(n.parse::<u32>().ok().filter(|n|*n<=i32::MAX as u32).ok_or(Error::Response)?),_=>return Err(Error::Response)};
            Ok(Header::Failure{status,message:message.into(),retry_after})
        },
        5=>Ok(Header::Failure{status,message:parameters.into(),retry_after:None}),
        6=>{
            let(scope,message)=parameters.split_once(' ').unwrap_or((parameters,""));
            Ok(Header::Identity{status,scope:scope.into(),message:message.into()})
        },
        _=>Err(Error::Response),
    }
}

pub(super) enum Reply {File(Metadata,Vec<u8>),Stream(Metadata,transfer::Transfer),Other(Header)}
pub(super) async fn receive<S:AsyncRead+AsyncWrite+Unpin+Send+'static>(mut stream:S,url:&Url,download:bool)->Result<Reply,Error> {
    stream.write_all(&request_bytes(url)?).await.map_err(|_|Error::Transport)?;
    let(reader,mut writer)=tokio::io::split(stream);let mut reader=BufReader::new(reader);let mut line=Vec::new();
    loop {
        if line.len()>=MAX_HEADER{return Err(Error::TooLarge);}
        line.push(reader.read_u8().await.map_err(|_|Error::Response)?);
        if line.ends_with(b"\r\n"){break;}
    }
    let public=public(url)?;
    match header(&line,&public)? {
        Header::File(metadata)=>{
            let reader:Box<dyn AsyncRead+Unpin+Send>=match metadata.length {Some(n)=>Box::new(reader.take(n)),None=>Box::new(reader)};
            if download || !metadata.displayable() || metadata.length.is_some_and(|n|n>MAX_BYTES as u64) {
                let transfer=transfer::Transfer::new(metadata.mime.clone(),metadata.length,reader).with_completion(async move{writer.shutdown().await.map_err(|_|Error::Transport)});
                return Ok(Reply::Stream(metadata,transfer));
            }
            let mut bytes=Vec::new();reader.take((MAX_BYTES+1) as u64).read_to_end(&mut bytes).await.map_err(|_|Error::Transport)?;
            if bytes.len()>MAX_BYTES{return Err(Error::TooLarge);}
            if metadata.length.is_some_and(|n|n!=bytes.len() as u64){return Err(Error::Response);}
            writer.shutdown().await.map_err(|_|Error::Transport)?;
            Ok(Reply::File(metadata,bytes))
        },
        h=>{let _=writer.shutdown().await;Ok(Reply::Other(h))},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_normalizes_authority_and_path_and_omits_only_the_fragment() {
        for (text,expected) in [("scorpion://EXAMPLE.invalid?q=two%20words#section","R scorpion://example.invalid/?q=two%20words\r\n"),("scorpions://host:1517/a%20b#anchor","R scorpions://host:1517/a%20b\r\n"),("scorpion://reader:pass@host/","R scorpion://reader:pass@host/\r\n")] {
            assert_eq!(request_bytes(&Url::parse(text).unwrap()).unwrap(),expected.as_bytes());
        }
        for text in ["gemini://host/","scorpion://host:0/","scorpion://host:25/"]{assert!(request_bytes(&Url::parse(text).unwrap()).is_err());}
    }
    #[test]
    fn status_fields_preserve_declared_lengths_formats_and_versions() {
        let url=Url::parse("scorpion://host/file#section").unwrap();
        let Header::File(m)=header(b"20 12 text/plain;charset=utf-8 V1\r\n",&url).unwrap() else{panic!()};
        assert_eq!(m.length,Some(12));assert_eq!(m.format,"text/plain;charset=utf-8");assert_eq!(m.version,Some("V1".into()));
        assert!(header(b"20 -1 text/plain\r\n",&url).is_err());
        let Header::File(m)=header(b"20 ? text/scorpion Version1\r\n",&url).unwrap() else{panic!()};assert!(m.document);assert_eq!(m.length,None);
        assert_eq!(header(b"44 30 Slow down\r\n",&url).unwrap(),Header::Failure{status:44,message:"Slow down".into(),retry_after:Some(30)});
        let Header::Redirect(next)=header(b"30 ../other\r\n",&url).unwrap() else{panic!()};assert_eq!(next.as_str(),"scorpion://host/other#section");
        for line in [b"20 0\r\n".as_slice(),b"20 ? text/plain\n",b"21 100 text/plain\r\n",b"44 2147483648 wait\r\n",b"20x3 text/plain\r\n"]{assert!(header(line,&url).is_err());}
    }
    #[test]
    fn known_length_does_not_wait_for_peer_eof_but_truncation_is_not_success() {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async{
            for (response,expected) in [(b"20 3 text/plain\r\nabc".as_slice(),true),(b"20 4 text/plain\r\nabc",false)] {
                let(client,mut server)=tokio::io::duplex(1024);
                let server=tokio::spawn(async move{let mut request=Vec::new();loop{request.push(server.read_u8().await.unwrap());if request.ends_with(b"\r\n"){break;}}server.write_all(response).await.unwrap();if expected{let mut byte=[0];server.read(&mut byte).await.unwrap();} });
                let result=tokio::time::timeout(std::time::Duration::from_secs(2),receive(client,&Url::parse("scorpion://host/").unwrap(),false)).await.unwrap();
                assert_eq!(result.is_ok(),expected);if let Ok(Reply::File(_,bytes))=result{assert_eq!(bytes,b"abc");}
                server.await.unwrap();
            }
        });
    }
}

#[derive(Default)]
pub(super) struct Load {tls:super::GeminiLoad,credentials:Option<(String,Option<String>)>,reviewed:Option<String>}
#[derive(Default)]
pub(super) struct Store(std::sync::Mutex<std::collections::BTreeMap<String,Url>>);
impl Store {
    fn previous(&self,owner:&str)->Option<Url>{self.0.lock().unwrap_or_else(|e|e.into_inner()).get(owner).cloned()}
    fn loaded(&self,owner:&str,url:&Url){let mut map=self.0.lock().unwrap_or_else(|e|e.into_inner());if map.len()>=256 && !map.contains_key(owner){map.pop_first();}map.insert(owner.into(),url.clone());}
}
fn authority(url:&Url)->String {format!("{}://{}:{}",url.scheme(),url.host_str().unwrap_or(""),url.port().unwrap_or(1517))}
fn public(url:&Url)->Result<Url,Error>{let mut url=url.clone();url.set_username("").map_err(|_|Error::Address)?;url.set_password(None).map_err(|_|Error::Address)?;Ok(url)}

impl super::Handler {
    pub(super) async fn scorpion_input(&self,owner:String,target:&Url,message:&str,context:&super::FetchContext)->Result<Url,Error> {
        let form=serde_json::json!({"title":"Scorpion link input","origin":target.as_str(),"description":"Send query text to this link's destination.","fields":[
            {"kind":"preview","label":"Link","value":message,"options":[]},
            {"kind":"text","label":"Your response","value":"","options":[]}]});
        let answer=self.prompt_with_form(owner,String::new(),Some(form.to_string()),context).await?;
        let fields:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;
        let fields=fields.as_array().filter(|v|v.len()==2).ok_or(Error::Address)?;
        let value=fields[1]["value"].as_str().ok_or(Error::Address)?;
        if value.chars().any(char::is_control) || value.len()>MAX_REQUEST{return Err(Error::Address);}
        let mut next=target.clone();next.set_query(Some(&percent_encoding::utf8_percent_encode(value,percent_encoding::NON_ALPHANUMERIC).to_string()));Ok(next)
    }
    async fn scorpion_credentials(&self,owner:String,target:&Url,context:&super::FetchContext)->Result<(String,Option<String>),Error> {
        let form=serde_json::json!({"title":"Scorpion login","origin":target.as_str(),"description":"This server requests a user name and password. Login information applies to this navigation and its redirects on the same server; it is not saved in history.","fields":[
            {"kind":"text","label":"User name","value":"","options":[]},
            {"kind":"password","label":"Password","value":"","options":[]}]});
        let answer=self.prompt_with_form(owner,String::new(),Some(form.to_string()),context).await?;
        let fields:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;
        let fields=fields.as_array().filter(|v|v.len()==2).ok_or(Error::Address)?;
        let user=fields[0]["value"].as_str().ok_or(Error::Address)?;let password=fields[1]["value"].as_str().ok_or(Error::Address)?;
        if user.len()+password.len()>MAX_REQUEST || user.chars().chain(password.chars()).any(char::is_control){return Err(Error::Address);}
        let mut wire=target.clone();wire.set_username(user).map_err(|_|Error::Address)?;wire.set_password(Some(password)).map_err(|_|Error::Address)?;
        Ok((wire.username().into(),wire.password().map(str::to_owned)))
    }
    pub(super) async fn retrieve_scorpion(&self,url:&mut Url,request:&mut super::Request,mut state:Load,context:&super::FetchContext)->Result<super::Resource,Error> {
        use super::{Resource,Content,PrivateTask,ActionKind,cancellable,document as page};
        let owner=request.target_webview_id.as_ref().map(|id|format!("{id:?}")).ok_or(Error::Cancelled)?;
        let download=request.initiator==super::Initiator::Download;
        let supplied_credentials=!url.username().is_empty() || url.password().is_some();
        if supplied_credentials {
            state.credentials=Some((url.username().into(),url.password().map(str::to_owned)));
            *url=public(url)?;
            for claim in &mut request.url_list {
                let current=claim.url().into_url();
                if matches!(current.scheme(),"scorpion"|"scorpions") {*claim=servo::protocol_handler::UrlWithBlobClaim::for_url(public(&current)?.into()).map_err(|_|Error::Address)?;}
            }
        }
        normalize_address(url)?;if url.path().is_empty(){url.set_path("/");}
        if supplied_credentials || url.as_str()!=request.current_url().as_str(){return self.restart_private(request,url.clone(),PrivateTask::Scorpion{state,redirect:false},context);}
        let target=url.clone();let destination=authority(&target);
        if state.reviewed.as_deref()!=Some(destination.as_str()) {
            if let Some(previous)=self.0.scorpion.previous(&owner).filter(|previous|authority(previous)!=destination) {
                self.review(owner.clone(),&target,"Open Scorpion server","Open this link on a different server?",format!("From: {}\nTo: {}",previous,target),"Open",context).await?;
                state.reviewed=Some(destination);
            }
        }
        let mut wire=target.clone();
        if let Some((user,password))=&state.credentials {wire.set_username(user).map_err(|_|Error::Address)?;wire.set_password(password.as_deref()).map_err(|_|Error::Address)?;}
        request_bytes(&wire)?;
        let (reply,tls)=if target.scheme()=="scorpions" {
            let session=self.tls_session(&target,&owner,&mut state.tls,context).await?;let pin=session.certificate.pin.clone();let tls12=session.tls12();
            (cancellable(receive(session.into_stream(),&wire,download),context,20).await?,Some((pin,tls12)))
        }else {
            let host=target.host_str().ok_or(Error::Address)?.trim_matches(['[',']']);
            let stream=cancellable(async{tokio::net::TcpStream::connect((host,target.port().unwrap_or(1517))).await.map_err(|_|Error::Transport)},context,20).await?;
            (cancellable(receive(stream,&wire,download),context,20).await?,None)
        };
        let resource=match reply {
            Reply::Stream(metadata,stream)=>{
                if download{return Ok(Resource::Stream(stream));}
                self.save_transfer(url,stream,super::download_filename(url).filter(|n|!n.is_empty()).unwrap_or_else(||if metadata.document{"document.scorp".into()}else{"download".into()}),owner.clone(),context).await?
            },
            Reply::File(metadata,bytes)=>{
                if download {return Ok(Resource::Document(Content::raw(bytes,metadata.mime)));}
                if metadata.document {
                    let html=document::render(url,&bytes,|target,label|self.action_link(target,label,Some(&owner),ActionKind::ScorpionInput))?;
                    Resource::Document(Content::html(html))
                }else {self.file_resource(url,&metadata.mime,bytes,owner.clone(),context,false).await?}
            },
            Reply::Other(Header::Input(message))=>{
                if target.query().is_some(){return Err(Error::Response);}
                let next=self.scorpion_input(owner,&target,&message,context).await?;
                return self.restart_private(request,next,PrivateTask::Scorpion{state,redirect:false},context);
            },
            Reply::Other(Header::Redirect(next))=>{
                if state.tls.redirects>=5{return Err(Error::Response);}
                let mut next=next;let clean=public(&next)?;
                if authority(&clean)!=authority(&target) {
                    let title=if target.scheme()=="scorpions" && clean.scheme()=="scorpion"{"Continue without TLS"}else{"Follow Scorpion redirect"};
                    self.review(owner.clone(),&clean,title,"Continue to this destination?",format!("From: {}\nTo: {}",target,clean),"Continue",context).await?;
                    state.credentials=None;state.tls.identity=None;state.reviewed=Some(authority(&clean));
                }
                if !next.username().is_empty() || next.password().is_some(){state.credentials=Some((next.username().into(),next.password().map(str::to_owned)));next=clean;}
                if !matches!(next.scheme(),"scorpion"|"scorpions") {
                    let label=page::escape(next.as_str());let link=page::link_target(url,next.as_str());
                    let body=match link {Some(next)=>format!("<h1>Scorpion redirect</h1><p><a href=\"{}\" rel=noreferrer>Continue to {label}</a></p>",page::escape(next.as_str())),None=>format!("<h1>Scorpion redirect</h1><p>Unsupported destination: {label}</p>")};
                    Resource::Document(Content::html(page::page(url,&body)))
                }else {state.tls.redirects+=1;return self.restart_private(request,next,PrivateTask::Scorpion{state,redirect:true},context);}
            },
            Reply::Other(Header::Identity{status,scope,message})=>{
                let Some((pin,tls12))=tls else {
                    let mut next=target.clone();next.set_scheme("scorpions").map_err(|_|Error::Address)?;
                    self.review(owner,&next,"Use TLS for Scorpion identity","This server requests a client certificate.","Continue with TLS before selecting a certificate. No identity is sent over the current connection.".into(),"Continue",context).await?;
                    state.credentials=None;state.reviewed=Some(authority(&next));return self.restart_private(request,next,PrivateTask::Scorpion{state,redirect:false},context);
                };
                state.tls.identity=Some(self.choose_tls_identity(owner,&target,&pin,status,format!("{message}\nServer scope hint: {scope}"),tls12,context).await?);
                return self.restart_private(request,target,PrivateTask::Scorpion{state,redirect:false},context);
            },
            Reply::Other(Header::Failure{status:56,..})=>{
                state.credentials=Some(self.scorpion_credentials(owner,&target,context).await?);
                return self.restart_private(request,target,PrivateTask::Scorpion{state,redirect:false},context);
            },
            Reply::Other(Header::Failure{status,message,retry_after})=>{
                let retry=retry_after.map(|s|format!("\nSuggested retry delay: {s} seconds.")).unwrap_or_default();
                let mut content=Content::html(page::plain(url,&format!("Scorpion {status}\n\n{message}{retry}")));content.status=match status{51=>404,52=>410,44=>429,40..=49=>503,54=>403,_=>400};Resource::Document(content)
            },
            Reply::Other(Header::File(_))=>return Err(Error::Response),
        };
        self.0.scorpion.loaded(&owner,url);Ok(resource)
    }
}
