// SPDX-License-Identifier: GPL-3.0-or-later
//! Native smallnet loading. Remote documents use existing layout and no scripts.
mod document;
mod ask;
mod gopher;
mod gemini;
mod mercury;
mod guppy;
mod dict;
mod cso;
mod news;
mod scorpion;
mod titan;
mod scroll;
mod ftp;
mod fsp;
mod whois;
mod transfer;
mod spartan;

use std::{collections::VecDeque, future::Future, pin::Pin, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::Duration};
use percent_encoding::percent_decode_str;
use servo::protocol_handler::{Destination, DoneChannel, FetchContext, Initiator, NetworkError, ProtocolHandler, ProtocolRedirect, ProtocolRegistry, Request, RequestMode, ResourceFetchTiming, Response, ResponseBody};
use servo_base::generic_channel::GenericCallback;
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpStream, sync::oneshot};
use url::Url;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_REQUEST: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Error { Cancelled, Timeout, Address, Transport, TooLarge, Unsupported, Response, InputExpired, Certificate, TrustStore, Identity, NewsSettings, LocalFile, Submission }

impl Error {
    fn message(self) -> &'static str {
        match self {
            Self::Cancelled => "Smallnet load cancelled",
            Self::Timeout => "Smallnet server did not respond in time",
            Self::Address => "Invalid smallnet address or request",
            Self::Transport => "Cannot retrieve this smallnet resource",
            Self::TooLarge => "Smallnet response exceeds the document size limit",
            Self::Unsupported => "Unsupported smallnet resource type",
            Self::Response => "The smallnet server returned an invalid response",
            Self::InputExpired => "This input link expired; reload its document to try again",
            Self::Certificate => "The TLS certificate or selected identity could not be verified",
            Self::TrustStore => "Cannot read or save private smallnet TLS trust settings",
            Self::NewsSettings => "Cannot read or save news server settings",
            Self::Identity => "Cannot reuse or create the local TLS identity",
            Self::LocalFile => "Cannot read the selected local file, or it changed during preparation",
            Self::Submission => "The submission outcome could not be confirmed. Check the server before submitting again.",
        }
    }
}

pub struct Prompt {
    target: String,
    pub message: String,
    pub form: Option<String>,
    answer: oneshot::Sender<Option<String>>,
}

impl Prompt {
    pub fn respond(self, answer: Option<String>) { let _ = self.answer.send(answer); }
    pub fn is_closed(&self) -> bool { self.answer.is_closed() }
    pub fn belongs_to(&self, owner: &str) -> bool { self.target == owner }
}

struct Shared {
    prompts: Mutex<VecDeque<Prompt>>,
    notices: Mutex<VecDeque<(String,String)>>,
    inputs: Mutex<VecDeque<InputAction>>,
    private_loads: Mutex<Vec<PrivateLoad>>,
    gemini: gemini::Store,
    news: news::Store,
    scorpion: scorpion::Store,
    wake: Arc<AtomicBool>,
}

#[derive(Clone)]
struct InputAction {
    id: uuid::Uuid,
    owner: String,
    target: Url,
    label: String,
    kind: ActionKind,
}
#[derive(Clone,Copy,PartialEq)]
enum ActionKind { FspPassword, Input, ScorpionInput, TitanCompose, Abstract, Resource, NewsServer, NewsFilter }

pub struct Smallnet(Arc<Shared>);

impl Smallnet {
    #[cfg(test)]
    pub fn new(wake: Arc<AtomicBool>) -> Self {
        Self::create(wake, None)
    }

    pub fn with_profile(wake: Arc<AtomicBool>, profile: &std::path::Path) -> Self { Self::create(wake, Some(profile.join("gemini-trust.json"))) }

    fn create(wake: Arc<AtomicBool>, trust: Option<std::path::PathBuf>) -> Self {
        let news=news::Store::new(trust.as_ref().and_then(|p|p.parent()).map(|p|p.join("news-servers.json")));
        Self(Arc::new(Shared { news, scorpion:scorpion::Store::default(), prompts: Mutex::new(VecDeque::new()), notices: Mutex::new(VecDeque::new()), inputs: Mutex::new(VecDeque::new()), private_loads: Mutex::new(Vec::new()), gemini: gemini::Store::new(trust), wake }))
    }

    pub fn registry(&self) -> ProtocolRegistry {
        let mut registry = ProtocolRegistry::default();
        for scheme in ["whois", "rwhois", "fsp", "titan", "scorpion", "scorpions", "gopher", "gopher+", "gophers", "gemini", "mercury", "guppy", "dict", "cso", "ph", "news", "nntp", "scroll", "ftp", "nex", "finger", "spartan", "dome-smallnet-input"] {
            registry.register(scheme, Handler(self.0.clone())).expect("Unique native smallnet scheme");
        }
        registry
    }

    pub fn next_notice(&self, target:&str)->Option<String> {
        let mut notices=self.0.notices.lock().unwrap_or_else(|e|e.into_inner());
        let index=notices.iter().position(|(owner,_)|owner==target)?;
        notices.remove(index).map(|(_,message)|message)
    }

    pub fn next_prompt(&self, target: &str) -> Option<Prompt> {
        let mut prompts = self.0.prompts.lock().unwrap_or_else(|e| e.into_inner());
        prompts.retain(|prompt| !prompt.answer.is_closed());
        let index = prompts.iter().position(|prompt| prompt.target == target)?;
        prompts.remove(index)
    }
}

struct Handler(Arc<Shared>);

enum PrivateTask { WhoisNotice(Error), FspPassword(Result<Vec<u8>,Error>), Titan(Option<titan::Submission>), TitanNotice(Error), Scorpion {state:scorpion::Load,redirect:bool}, GopherDefinition, Submission { parsed: gopher::Request, bytes: Vec<u8> }, Gemini { state: GeminiLoad, redirect: bool } }
#[derive(Default)]
struct GeminiLoad { redirects: u8, sensitive: bool, identity: Option<gemini::Binding>, metadata:bool }
struct PrivateLoad { id: uuid::Uuid, token: uuid::Uuid, owner: String, target: Url, task: PrivateTask }
struct PrivateGuard { shared: Arc<Shared>, token: uuid::Uuid }
struct StopIdentityWorker(Arc<AtomicBool>);
impl Drop for StopIdentityWorker { fn drop(&mut self) { self.0.store(true, Ordering::Release); } }
impl Drop for PrivateGuard {
    fn drop(&mut self) { self.shared.private_loads.lock().unwrap_or_else(|e| e.into_inner()).retain(|load| load.token != self.token); }
}

impl ProtocolHandler for Handler {
    fn load<'a>(&'a self, request: &'a mut Request, done: &'a mut DoneChannel, context: &FetchContext)
        -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        let context = context.clone();
        Box::pin(async move {
            let top_level = request.mode == RequestMode::Navigate && request.destination == Destination::Document &&
                request.client.as_ref().is_none_or(|client| !client.is_nested_browsing_context);
            let download=request.initiator==Initiator::Download && request.current_url().scheme()!="dome-smallnet-input";
            if !(top_level || download) || request.method.as_str() != "GET" {
                return Response::network_error(NetworkError::ResourceLoadError("Smallnet resources require navigation or an explicit native download".into()));
            }
            let mut url = request.current_url().into_url();
            let result = self.retrieve(&mut url, request, &context).await;
            match result {
                Ok(Resource::Redirect(target)) => {
                    let mut response = Response::new(url.into(), ResourceFetchTiming::new(request.timing_type()));
                    response.custom_redirect = Some(ProtocolRedirect::new(target.into()));
                    response
                }
                Ok(Resource::Restart(continuation)) => {
                    let mut response = Response::new(url.into(), ResourceFetchTiming::new(request.timing_type()));
                    response.custom_redirect = Some(continuation);
                    response
                }
                Ok(Resource::Stream(stream)) => stream.response(url, request, done, &context),
                Ok(Resource::Document(content)) => {
                    let mut response = Response::new(url.into(), ResourceFetchTiming::new(request.timing_type()));
                    response.protocol_state = content.protocol_state;
                    response.status = servo::protocol_handler::HttpStatus::new_raw(content.status, Vec::new());
                    *response.body.lock() = ResponseBody::Done(content.bytes);
                    response.headers.insert("content-type", content.mime.parse().expect("Locally selected MIME type"));
                    if let Some(name)=content.filename {response.headers.insert("content-disposition",format!("attachment; filename*=UTF-8''{}",percent_encoding::utf8_percent_encode(&name,percent_encoding::NON_ALPHANUMERIC)).parse().expect("Locally encoded download name"));}
                    let image_policy=if content.image_data {"img-src data:; "}else{""};
                    response.headers.insert("content-security-policy", format!("default-src 'none'; {image_policy}style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; sandbox allow-top-navigation-by-user-activation allow-downloads").parse().unwrap());
                    response.headers.insert("referrer-policy", "no-referrer".parse().unwrap());
                    response.headers.insert("x-content-type-options", "nosniff".parse().unwrap());
                    response
                }
                Err(Error::Cancelled) => Response::network_error(NetworkError::LoadCancelled),
                Err(error) => Response::network_error(NetworkError::ResourceLoadError(error.message().into())),
            }
        })
    }
}

enum Resource { Document(Content), Redirect(Url), Restart(ProtocolRedirect), Stream(transfer::Transfer) }

struct Content { bytes: Vec<u8>, mime: String, status: u16, image_data:bool, protocol_state:Option<String>, filename:Option<String> }

impl Content {
    fn html(body: String) -> Self { Self::raw(body.into_bytes(), "text/html; charset=utf-8".into()) }
    fn raw(bytes: Vec<u8>, mime: String) -> Self { Self { bytes, mime, status: 200, image_data:false,protocol_state:None,filename:None } }
    fn abstract_view(mut self,enabled:bool)->Self {if enabled{self.protocol_state=Some("abstract".into());}self}
    fn image(url:&Url,mime:&str,bytes:&[u8],metadata:Option<(&scroll::Metadata,Option<&Url>)>)->Self {
        let mut content=Self::html(document::image(url,mime,bytes,metadata));content.image_data=true;content
    }
}

impl Handler {
    async fn file_resource(&self,url:&Url,mime:&str,bytes:Vec<u8>,owner:String,context:&FetchContext,download:bool)->Result<Resource,Error> {
        let kind:mime::Mime=mime.parse().map_err(|_|Error::Response)?;
        if download || kind.type_()==mime::TEXT || kind.type_()==mime::IMAGE || kind.essence_str()=="application/xhtml+xml" {
            self.typed_content(url,mime,bytes,Some(&owner),download).map(Resource::Document)
        }else {
            self.save_resource(url,mime,bytes,download_filename(url).unwrap_or_else(||"download".into()),owner,context).await
        }
    }
    async fn save_resource(&self,url:&Url,mime:&str,bytes:Vec<u8>,suggestion:String,owner:String,context:&FetchContext)->Result<Resource,Error> {
        let label=protocol_label(url);
        let spec=serde_json::json!({"title":format!("Save {label} file"),"origin":url.as_str(),"submit_label":"Save","description":"Choose a local destination for the retrieved file.",
            "fields":[{"kind":"save","label":format!("{} ({} bytes)",mime,bytes.len()),"value":suggestion,"file_title":format!("Save {label} file"),"options":[]}]});
        let answer=self.prompt_with_form(owner,String::new(),Some(spec.to_string()),context).await?;
        let fields:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;
        let path=fields.as_array().filter(|fields|fields.len()==1).and_then(|fields|fields[0]["value"].as_str()).ok_or(Error::Address)?;
        let path=std::path::PathBuf::from(path);
        if context.is_cancelled(){return Err(Error::Cancelled);}
        tokio::task::spawn_blocking(move ||ask::save(&path,&bytes)).await.map_err(|_|Error::Transport)??;
        Ok(Resource::Document(Content::html(document::plain(url,&format!("{label} file saved to the selected destination.")))))
    }

    async fn save_transfer(&self,url:&Url,stream:transfer::Transfer,suggestion:String,owner:String,context:&FetchContext)->Result<Resource,Error> {
        let label=protocol_label(url);
        let size=stream.length.map(|n|format!("{n} bytes")).unwrap_or_else(||"streaming transfer".into());
        let spec=serde_json::json!({"title":format!("Save {label} file"),"origin":url.as_str(),"submit_label":"Save","description":"Choose a local destination for this file.",
            "fields":[{"kind":"save","label":format!("{} ({size})",stream.mime),"value":suggestion,"file_title":format!("Save {label} file"),"options":[]}]});
        let answer=self.prompt_with_form(owner,String::new(),Some(spec.to_string()),context).await?;
        let fields:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;
        let path=fields.as_array().filter(|fields|fields.len()==1).and_then(|fields|fields[0]["value"].as_str()).ok_or(Error::Address)?;
        stream.save(std::path::Path::new(path),context).await?;
        Ok(Resource::Document(Content::html(document::plain(url,&format!("{label} file saved to the selected destination.")))))
    }

    async fn query_input(&self,owner:String,target:&Url,message:&str,sensitive:bool,context:&FetchContext)->Result<String,Error> {
        let label=protocol_label(target);
        let spec=serde_json::json!({"title":format!("{label} input"),"origin":gemini::authority(target),"description":format!("Send a response to this {label} server."),
            "fields":[{"kind":"preview","label":"Server prompt","value":message,"options":[]},{"kind":if sensitive{"password"}else{"text"},"label":"Your response","value":"","options":[]}]});
        let answer=self.prompt_with_form(owner,String::new(),Some(spec.to_string()),context).await?;
        let fields:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;
        let value=fields.as_array().filter(|fields|fields.len()==2).and_then(|fields|fields[1]["value"].as_str()).ok_or(Error::Address)?;
        if value.chars().any(char::is_control){return Err(Error::Address);}
        Ok(value.to_owned())
    }

    async fn dict_query(&self,owner:String,target:&dict::Target,context:&FetchContext)->Result<Url,Error> {
        let mut query=target.query.clone().ok_or(Error::Address)?;
        let mut fields=vec![serde_json::json!({"kind":"text","label":"Word","value":query.word,"options":[],"required":true}),
            serde_json::json!({"kind":"text","label":"Database (! = first matching, * = all)","value":query.database,"options":[],"required":true})];
        if query.kind==dict::Kind::Match {fields.push(serde_json::json!({"kind":"text","label":"Matching strategy (. = server default)","value":query.strategy,"options":[],"required":true}));}
        let spec=serde_json::json!({"title":if query.kind==dict::Kind::Define{"DICT definition"}else{"DICT matching"},"origin":gemini::authority(&target.url),"description":"Look up a word on this dictionary server.","submit_label":"Look up","fields":fields});
        let answer=self.prompt_with_form(owner,String::new(),Some(spec.to_string()),context).await?;
        let answer:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;
        let fields=answer.as_array().filter(|v|v.len()==if query.kind==dict::Kind::Define{2}else{3}).ok_or(Error::Address)?;
        query.word=fields[0]["value"].as_str().ok_or(Error::Address)?.into();query.database=fields[1]["value"].as_str().ok_or(Error::Address)?.into();
        if query.kind==dict::Kind::Match {query.strategy=fields[2]["value"].as_str().ok_or(Error::Address)?.into();}
        query.nth=None;query.command()?;Ok(query.url(&target.url))
    }

    async fn retrieve_dict(&self,url:&Url,request:&Request,context:&FetchContext)->Result<Resource,Error> {
        let target=dict::Target::parse(url)?;
        let owner=request.target_webview_id.as_ref().map(|id|format!("{id:?}"));
        if target.query.as_ref().is_some_and(|q|q.word.is_empty()) {
            let next=self.dict_query(owner.ok_or(Error::Cancelled)?,&target,context).await?;
            return Ok(Resource::Redirect(next));
        }
        let reply=cancellable(dict::open(&target),context,20).await?;
        if request.initiator==Initiator::Download {return Ok(Resource::Document(Content::raw(reply.plain_text().into_bytes(),"text/plain; charset=utf-8".into())));}
        let mut content=Content::html(document::dictionary(url,target.query.as_ref(),&reply,|target,label|self.input_link(target,label,owner.as_deref())));
        if let dict::Reply::Failure(code,_)=reply {content.status=match code {552=>404,530|531|532=>401,420|421=>503,_=>400};}
        Ok(Resource::Document(content))
    }

    async fn cso_query(&self,owner:String,target:&cso::Target,context:&FetchContext)->Result<Url,Error> {
        let mut lookup=target.lookup.clone().ok_or(Error::Address)?;
        let spec=serde_json::json!({"title":"Directory lookup","origin":gemini::authority(&target.url),"description":"Search this directory. Blank field selections use the server defaults.","submit_label":"Search","fields":[
            {"kind":"text","label":"Search text","value":lookup.term,"options":[],"required":true},
            {"kind":"text","label":"Search field (optional)","value":lookup.field,"options":[]},
            {"kind":"text","label":"Return fields (optional, separated by commas)","value":lookup.returns.join(","),"options":[]}]});
        let answer=self.prompt_with_form(owner,String::new(),Some(spec.to_string()),context).await?;
        let answer:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;
        let fields=answer.as_array().filter(|a|a.len()==3).ok_or(Error::Address)?;
        lookup.term=fields[0]["value"].as_str().ok_or(Error::Address)?.into();lookup.field=fields[1]["value"].as_str().ok_or(Error::Address)?.into();
        lookup.returns=fields[2]["value"].as_str().ok_or(Error::Address)?.split([',',' ','\t']).filter(|s|!s.is_empty()).map(str::to_owned).collect();
        lookup.command()?;Ok(lookup.url(&target.url))
    }

    async fn retrieve_cso(&self,url:&Url,request:&Request,context:&FetchContext)->Result<Resource,Error> {
        let target=match cso::Target::parse(url) {
            Ok(target)=>target,
            Err(Error::Unsupported)=>return Ok(Resource::Document(Content::html(document::plain(url,"This directory protocol supports US-ASCII and ISO-8859-1 search text. The requested lookup cannot be represented in that character set.")))),
            Err(error)=>return Err(error),
        };
        let owner=request.target_webview_id.as_ref().map(|id|format!("{id:?}"));
        if target.lookup.as_ref().is_some_and(|q|q.term.is_empty()) {
            let next=self.cso_query(owner.ok_or(Error::Cancelled)?,&target,context).await?;return Ok(Resource::Redirect(next));
        }
        let reply=cancellable(cso::open(&target),context,20).await?;
        if request.initiator==Initiator::Download {return Ok(Resource::Document(Content::raw(reply.plain_text().into_bytes(),"text/plain; charset=utf-8".into())));}
        let html=document::directory(url,target.lookup.as_ref(),&reply,|target,label|self.input_link(target,label,owner.as_deref()))?;
        let mut content=Content::html(html);
        content.status=match reply.code {200..=299=>200,501=>404,301|302|503..=506|516=>401,400..=499=>503,_=>400};
        Ok(Resource::Document(content))
    }

    async fn retrieve_whois(&self,url:&mut Url,request:&mut Request,private:Option<PrivateTask>,context:&FetchContext)->Result<Resource,Error> {
        let target=whois::Target::parse(url)?;*url=target.url.clone();
        let owner=request.target_webview_id.as_ref().map(|id|format!("{id:?}"));
        let action=self.input_link(url,"Change lookup",owner.as_deref());
        match private {
            Some(PrivateTask::WhoisNotice(error))=>return if context.is_cancelled(){Err(Error::Cancelled)}else{Ok(Resource::Document(Content::html(whois::document(&target,None,action.as_ref(),Some(if error==Error::Cancelled{"Lookup cancelled. No query was sent."}else{error.message()})))))},
            Some(_)=>return Err(Error::Address),None=>{},
        }
        let download=request.initiator==Initiator::Download;
        if target.query.as_ref().is_none_or(|q|q.is_empty()) {
            if download{return Err(Error::Unsupported);}
            return Ok(Resource::Document(Content::html(whois::document(&target,None,action.as_ref(),None))));
        }
        let reply=cancellable(whois::open(&target,download),context,20).await?;
        match reply {
            whois::Reply::Stream(stream)=>if download{Ok(Resource::Stream(stream))}else{self.save_transfer(url,stream,format!("{}.txt",url.scheme()),owner.ok_or(Error::Cancelled)?,context).await},
            reply=>{
                if download {let bytes=match reply{whois::Reply::Text(bytes)=>bytes,whois::Reply::Results(result)=>result.source,whois::Reply::Failure(code,message)=>format!("RWhois {code}: {message}\r\n").into_bytes(),_=>unreachable!()};return Ok(Resource::Document(Content::raw(bytes,"text/plain".into())));}
                Ok(Resource::Document(Content::html(whois::document(&target,Some(&reply),action.as_ref(),None))))
            },
        }
    }

    async fn retrieve_fsp(&self,url:&mut Url,request:&mut Request,private:Option<PrivateTask>,context:&FetchContext)->Result<Resource,Error> {
        let parsed=fsp::Target::parse(url);
        url.set_username("").map_err(|_|Error::Address)?;url.set_password(None).map_err(|_|Error::Address)?;
        for claim in &mut request.url_list {
            let mut clean=claim.url().into_url();if clean.scheme()=="fsp"{clean.set_username("").map_err(|_|Error::Address)?;clean.set_password(None).map_err(|_|Error::Address)?;*claim=servo::protocol_handler::UrlWithBlobClaim::for_url(clean.into()).map_err(|_|Error::Address)?;}
        }
        let target=parsed?;*url=target.url.clone();
        let password=match private {None=>target.password.clone(),Some(PrivateTask::FspPassword(Ok(password)))=>Some(password),Some(PrivateTask::FspPassword(Err(error)))=>return if context.is_cancelled(){Err(Error::Cancelled)}else{Ok(Resource::Document(Content::html(document::plain(url,if error==Error::Cancelled{"FSP password entry cancelled. No password was sent."}else{error.message()}))))},_=>return Err(Error::Address)};
        let owner=request.target_webview_id.as_ref().map(|id|format!("{id:?}"));
        let download=request.initiator==Initiator::Download;
        match cancellable(fsp::open(&target,password.as_deref(),download),context,30).await? {
            fsp::Reply::File {mime,bytes}=>self.file_resource(url,&mime,bytes,owner.ok_or(Error::Cancelled)?,context,download).await,
            fsp::Reply::Stream(stream)=>if download{Ok(Resource::Stream(stream))}else{self.save_transfer(url,stream,target.filename(),owner.ok_or(Error::Cancelled)?,context).await},
            fsp::Reply::Directory {url:base,entries,readme,flags}=>{
                if download{return Ok(Resource::Document(Content::raw(fsp::document(url,&base,&entries,&readme,flags,None).into_bytes(),"text/html; charset=utf-8".into())));}
                let account=self.action_link(url,"Use password for this load",owner.as_deref(),ActionKind::FspPassword);
                Ok(Resource::Document(Content::html(fsp::document(url,&base,&entries,&readme,flags,account.as_ref()))))
            },
            fsp::Reply::Failure(message)=>{
                let account=if download{None}else{self.action_link(url,"Use password for this load",owner.as_deref(),ActionKind::FspPassword)};
                let mut body=format!("<h1>FSP resource unavailable</h1><pre>{}</pre>",document::escape(&message));
                if let Some(account)=account{body.push_str(&format!("<p><a href=\"{}\" rel=noreferrer>Use password for this load</a></p>",document::escape(account.as_str())));}
                let mut content=Content::html(document::page_with_metadata(url,&body,None,Some("FSP resource unavailable")));content.status=404;Ok(Resource::Document(content))
            },
        }
    }

    async fn retrieve_ftp(&self, url: &mut Url, request: &mut Request, context: &FetchContext) -> Result<Resource, Error> {
        let parsed=ftp::Target::parse(url);
        // URI-supplied credentials belong only to this load. Neither response
        // URLs nor the navigation chain retain them, including on errors.
        url.set_username("").map_err(|_|Error::Address)?;
        url.set_password(None).map_err(|_|Error::Address)?;
        for claim in &mut request.url_list {
            let mut clean=claim.url().into_url();
            if clean.scheme()=="ftp" {
                clean.set_username("").map_err(|_|Error::Address)?;
                clean.set_password(None).map_err(|_|Error::Address)?;
                *claim=servo::protocol_handler::UrlWithBlobClaim::for_url(clean.into()).map_err(|_|Error::Address)?;
            }
        }
        let target=parsed?;*url=target.url.clone();
        let owner=request.target_webview_id.as_ref().map(|id|format!("{id:?}")).ok_or(Error::Cancelled)?;
        let mut credentials=target.credentials.clone();
        for attempt in 0..4 {
            match cancellable(ftp::open(&target,credentials.as_ref(),request.initiator==Initiator::Download),context,20).await? {
                ftp::Reply::Stream(stream) => {
                    if request.initiator==Initiator::Download { return Ok(Resource::Stream(stream)); }
                    return self.save_transfer(url, stream, target.filename(), owner.clone(), context).await;
                },
                ftp::Reply::File {mime,bytes}=>{
                    return self.file_resource(url,&mime,bytes,owner.clone(),context,request.initiator==Initiator::Download).await;
                },
                ftp::Reply::Directory {base,entries}=>return Ok(Resource::Document(Content::html(document::ftp(url,&base,&entries)))),
                ftp::Reply::Authenticate {account}=>{
                    if attempt==3 {return Ok(Resource::Document(Content::html(document::plain(url,"FTP login was not accepted."))));}
                    let spec=serde_json::json!({"title":"FTP account","origin":format!("ftp://{}:{}",url.host_str().unwrap_or(""),url.port().unwrap_or(21)),
                        "submit_label":"Log in","description":"Credentials apply to this server for the current load.",
                        "fields":[{"kind":"text","label":"Username","value":credentials.as_ref().map(|c|c.user.as_str()).unwrap_or("anonymous"),"required":true,"options":[]},
                            {"kind":"password","label":"Password","value":"","options":[]},
                            {"kind":"text","label":"Account (when required by the server)","value":"","required":account,"options":[]}]});
                    let answer=self.prompt_with_form(owner.clone(),String::new(),Some(spec.to_string()),context).await?;
                    let fields:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;
                    let fields=fields.as_array().filter(|fields|fields.len()==3).ok_or(Error::Address)?;
                    credentials=Some(ftp::Credentials {user:fields[0]["value"].as_str().ok_or(Error::Address)?.to_owned(),password:Some(fields[1]["value"].as_str().ok_or(Error::Address)?.to_owned()),account:fields[2]["value"].as_str().ok_or(Error::Address)?.to_owned()});
                },
                ftp::Reply::Failure(code,message)=>{
                    let mut content=Content::html(document::plain(url,&format!("FTP {code}\n\n{message}")));
                    content.status=if code<500 {503}else{400};return Ok(Resource::Document(content));
                },
            }
        }
        Err(Error::Response)
    }

    async fn choose_tls_identity(&self, owner: String, target: &Url, pin: &gemini::Pin, status: u8, message: String, tls12: bool, context: &FetchContext) -> Result<gemini::Binding, Error> {
        let label=protocol_label(target);
        let locations = crate::identities::Locations::current().map_err(|_| Error::Identity)?;
        let lookup = locations.clone();
        let candidates = tokio::task::spawn_blocking(move || crate::identities::discover(&lookup)).await.map_err(|_| Error::Identity)?.map_err(|_| Error::Identity)?;
        let mut options: Vec<_> = candidates.iter().map(|item| item.label.clone()).collect();
        options.extend(["Create shared identity".into(), "Import PEM files".into()]);
        let explanation = if tls12 { "TLS 1.2 can expose the public client certificate during negotiation. The private key stays local." }
            else { "The private key stays local. This identity is used only for the displayed server and path." };
        let spec = serde_json::json!({"title":format!("{label} client identity"),"origin":gemini::authority(target),"submit_label":"Continue","description":explanation,
            "fields":[{"kind":"preview","label":"Server request and scope","value":format!("{label} status {status}\n{message}\nScope: {}{}",gemini::authority(target),target.path()),"options":[]},
                {"kind":"choice","label":"Identity source","value":"","options":options},
                {"kind":"check","label":"Remember for this server and path","value":"0","options":[]}]});
        let answer = self.prompt_with_form(owner.clone(), String::new(), Some(spec.to_string()), context).await?;
        let fields: Vec<serde_json::Value> = serde_json::from_str(&answer).map_err(|_| Error::Address)?;
        if fields.len() != 3 { return Err(Error::Address); }
        let selected = fields[1]["choice"].as_u64().and_then(|value| usize::try_from(value).ok()).ok_or(Error::Address)?;
        let remember = fields[2]["checked"].as_bool().unwrap_or(false);
        let source = if selected < candidates.len() {
            candidates[selected].source.clone()
        } else if selected == candidates.len() {
            let spec = serde_json::json!({"title":"Create TLS identity","origin":gemini::authority(target),"submit_label":"Create and use",
                "description":"Creates a reusable certificate. It does not register a Misfin mailbox.",
                "fields":[{"kind":"text","label":"Identity name","value":"Personal identity","required":true,"options":[]},
                    {"kind":"text","label":"Misfin mailbox address (mailbox@hostname)","value":"","required":true,"options":[]}]});
            let answer = self.prompt_with_form(owner.clone(), String::new(), Some(spec.to_string()), context).await?;
            let fields: Vec<serde_json::Value> = serde_json::from_str(&answer).map_err(|_| Error::Address)?;
            if fields.len() != 2 { return Err(Error::Address); }
            let name = fields[0]["value"].as_str().ok_or(Error::Address)?.to_owned();
            let address = fields[1]["value"].as_str().ok_or(Error::Address)?.to_owned();
            let cancelled = Arc::new(AtomicBool::new(false)); let _guard = StopIdentityWorker(cancelled.clone());
            let worker = tokio::task::spawn_blocking(move || crate::identities::create(&locations, &name, &address, &cancelled));
            let created = cancellable(async { worker.await.map_err(|_| Error::Identity)?.map_err(|_| Error::Identity) }, context, 35).await?;
            created.source
        } else if selected == candidates.len()+1 {
            let spec = serde_json::json!({"title":"Select PEM identity","origin":gemini::authority(target),"submit_label":"Use identity","description":explanation,
                "fields":[{"kind":"preview","label":"Identity scope","value":format!("{}{}",gemini::authority(target),target.path()),"options":[]},
                    {"kind":"file","label":"Certificate PEM","value":"","options":[],"file_title":"Choose a client certificate"},
                    {"kind":"file","label":"Private key PEM (kept locally)","value":"","options":[],"file_title":"Choose the local private key"}]});
            let answer = self.prompt_with_form(owner, String::new(), Some(spec.to_string()), context).await?;
            let fields: Vec<serde_json::Value> = serde_json::from_str(&answer).map_err(|_| Error::Address)?;
            if fields.len() != 3 { return Err(Error::Address); }
            crate::identities::Source::Pem { certificate: fields[1]["value"].as_str().ok_or(Error::Address)?.into(), key: fields[2]["value"].as_str().ok_or(Error::Address)?.into(), fingerprint: None }
        } else { return Err(Error::Address); };
        if !source.valid() { return Err(Error::Address); }
        let mut binding = gemini::Binding { authority: gemini::authority(target), path: target.path().to_owned(), server_pin: pin.fingerprint.clone(), identity: source };
        let binding = tokio::task::spawn_blocking(move || {
            let identity = gemini::Identity::load(&binding)?;
            // Approval refers to the certificate just validated, including
            // manually imported PEM files that originally had no catalog ID.
            if let crate::identities::Source::Pem { fingerprint, .. } = &mut binding.identity {
                *fingerprint = Some(identity.fingerprint());
            }
            Ok::<_, Error>(binding)
        }).await.map_err(|_| Error::Certificate)??;
        if remember { let saved = binding.clone(); self.tls_io(move |store| store.remember(saved)).await?; }
        Ok(binding)
    }

    async fn tls_io<T: Send + 'static>(&self, job: impl FnOnce(&gemini::Store) -> Result<T, Error> + Send + 'static) -> Result<T, Error> {
        let shared = self.0.clone();
        tokio::task::spawn_blocking(move || job(&shared.gemini)).await.map_err(|_| Error::TrustStore)?
    }

    // Native TLS trust is scoped to each scheme/host/port. Only the Gemini
    // and Scroll protocols may load an explicitly approved client identity.
    async fn tls_session(&self, target: &Url, owner: &str, state: &mut GeminiLoad, context: &FetchContext) -> Result<gemini::Session, Error> {
        let label=protocol_label(target);
        let mut changes = 0;
        let session = loop {
            let lookup = target.clone();
            let expected = self.tls_io(move |store| store.pin(&lookup)).await?;
            let lookup = target.clone(); let for_identity = expected.clone();
            let identities=matches!(target.scheme(),"titan"|"gemini"|"scroll"|"scorpions");
            let saved = if identities { self.tls_io(move |store| match for_identity { Some(pin) => store.identity(&lookup, &pin), None => Ok(None) }).await? } else {None};
            let binding = state.identity.as_ref().filter(|_| identities).filter(|binding| expected.as_ref().is_some_and(|pin| binding.matches(&target, pin))).cloned().or(saved);
            let identity = match binding {
                Some(binding) => Some(tokio::task::spawn_blocking(move || gemini::Identity::load(&binding)).await.map_err(|_| Error::Certificate)??),
                None => None,
            };
            match cancellable(gemini::connect(&target, expected.clone(), identity), context, 20).await? {
                gemini::Connection::Ready(session) => {
                    let lookup = target.clone(); let pin = session.certificate.pin.clone(); let prior = expected.clone();
                    let accepted = self.tls_io(move |store| store.accept(&lookup, pin, prior.as_ref())).await?;
                    if accepted { break session; }
                    session.close().await;
                }
                gemini::Connection::Changed => {
                    if changes >= 3 { return Err(Error::Certificate); }
                    changes += 1;
                    // Confirm possession of the new key without any client
                    // identity or application request before offering trust.
                    let probe = match cancellable(gemini::connect(&target, None, None), context, 20).await? {
                        gemini::Connection::Ready(session) => session,
                        _ => return Err(Error::Certificate),
                    };
                    let cert = probe.certificate.clone(); probe.close().await;
                    let previous = expected.as_ref().map(|pin| pin.fingerprint.as_str()).unwrap_or("none");
                    let spec = serde_json::json!({"title":format!("{label} certificate changed"),"origin":gemini::authority(&target),"submit_label":"Trust",
                        "description":"Review this server's new certificate before continuing.","fields":[{"kind":"preview","label":"Certificate details",
                        "value":format!("Previous SHA-256: {previous}\nNew SHA-256: {}\nSubject: {}\nValid: {}",cert.pin.fingerprint,cert.subject,cert.validity),"options":[]}]});
                    self.prompt_with_form(owner.to_owned(), String::new(), Some(spec.to_string()), context).await?;
                    let lookup = target.clone();
                    let accepted = self.tls_io(move |store| store.accept(&lookup, cert.pin, expected.as_ref())).await?;
                    if !accepted { continue; }
                    state.identity = None;
                }
            }
        };
        Ok(session)
    }

    async fn retrieve_gemini_family(&self, url: &mut Url, request: &mut Request, mut state: GeminiLoad, context: &FetchContext) -> Result<Resource, Error> {
        let target = url.clone();
        if state.sensitive {
            for claim in &mut request.url_list {
                let mut clean = claim.url().into_url();
                if matches!(clean.scheme(),"gemini"|"mercury"|"guppy"|"scroll") {
                    clean.set_query(None);
                    *claim = servo::protocol_handler::UrlWithBlobClaim::for_url(clean.into()).map_err(|_| Error::Address)?;
                }
            }
            url.set_query(None);
        }
        let owner = request.target_webview_id.as_ref().map(|id| format!("{id:?}")).ok_or(Error::Cancelled)?;
        let download=request.initiator==Initiator::Download;
        let (reply,tls) = if target.scheme()=="mercury" {
            // This transport never consults TLS pins or identity bindings.
            (cancellable(mercury::open(&target,download),context,20).await?,None)
        } else if target.scheme()=="guppy" {
            (cancellable(guppy::open(&target,download),context,20).await?,None)
        } else {
            let session=self.tls_session(&target,&owner,&mut state,context).await?;
            let pin=session.certificate.pin.clone();
            let tls12 = session.tls12();
            (cancellable(session.request(&target,state.metadata,download),context,20).await?,Some((pin,tls12)))
        };
        match reply {
            gemini::Reply::Stream(stream) => {
                if download {Ok(Resource::Stream(stream))}
                else {self.save_transfer(url,stream,download_filename(url).unwrap_or_else(||"download".into()),owner,context).await}
            },
            gemini::Reply::Success(mime, bytes) => self.file_resource(url,&mime,bytes,owner,context,download).await,
            gemini::Reply::ScrollSuccess {mime,bytes,metadata}=>{
                let mime:mime::Mime=mime.parse().map_err(|_|Error::Response)?;
                if download {return Ok(Resource::Document(Content::raw(bytes,mime.to_string())));}
                let abstract_link=self.action_link(url,if state.metadata{"Open resource"}else{"Resource abstract"},Some(&owner),if state.metadata{ActionKind::Resource}else{ActionKind::Abstract});
                if state.metadata || (mime.type_()==mime::TEXT && mime.subtype().as_str()=="scroll") {
                    let encoding=if state.metadata{encoding_rs::UTF_8}else{mime.get_param(mime::CHARSET).map(|v|encoding_rs::Encoding::for_label(v.as_str().as_bytes()).ok_or(Error::Unsupported)).transpose()?.unwrap_or(encoding_rs::UTF_8)};
                    let (text,_,_)=encoding.decode(&bytes);
                    let language=mime.get_param("lang").or_else(||mime.get_param("language")).map(|v|v.as_str());
                    Ok(Resource::Document(Content::html(document::scroll(url,&text,language,&metadata,abstract_link.as_ref(),state.metadata,|target,prompt|self.input_link(target,prompt,Some(&owner)))).abstract_view(state.metadata)))
                }else if mime.type_()==mime::TEXT && mime.subtype()!=mime::HTML {
                    let encoding=mime.get_param(mime::CHARSET).map(|v|encoding_rs::Encoding::for_label(v.as_str().as_bytes()).ok_or(Error::Unsupported)).transpose()?.unwrap_or(encoding_rs::UTF_8);
                    let (text,_,_)=encoding.decode(&bytes);
                    Ok(Resource::Document(Content::html(document::scroll_plain(url,&text,&metadata,abstract_link.as_ref()))))
                }else if mime.type_()==mime::IMAGE {
                    Ok(Resource::Document(Content::image(url,mime.as_ref(),&bytes,Some((&metadata,abstract_link.as_ref())))))
                }else {
                    self.file_resource(url,mime.as_ref(),bytes,owner,context,false).await
                }
            },
            gemini::Reply::Input { sensitive, prompt } => {
                let value=self.query_input(owner,&target,&prompt,sensitive,context).await?;
                let mut next = target;
                next.set_query(Some(&percent_encoding::utf8_percent_encode(&value, percent_encoding::NON_ALPHANUMERIC).to_string()));
                if next.scheme()=="scroll" {scroll::request_bytes(&next,&scroll::languages(),state.metadata)?;}else if next.scheme()=="mercury" {mercury::request_bytes(&next)?;}else if next.scheme()=="guppy" {guppy::request_bytes(&next)?;}else{gemini::request_bytes(&next)?;}
                state.sensitive |= sensitive;
                self.restart_private(request, next, PrivateTask::Gemini { state, redirect: false }, context)
            }
            gemini::Reply::Redirect(next) => {
                if state.redirects >= 5 || (state.sensitive && next.scheme() != target.scheme()) { return Err(Error::Response); }
                if target.scheme()=="guppy" {
                    let mut notices=self.0.notices.lock().unwrap_or_else(|e|e.into_inner());
                    notices.retain(|(existing,_)|existing!=&owner);
                    if notices.len()>=32 {notices.pop_front();}
                    notices.push_back((owner.clone(),format!("Guppy redirected to {next}")));
                    self.0.wake.store(true,Ordering::Release);
                }
                state.redirects += 1;
                if next.scheme()!=target.scheme(){state.metadata=false;state.identity=None;}
                if !matches!(next.scheme(),"gemini"|"mercury"|"guppy"|"scroll") { return Ok(Resource::Redirect(next)); }
                self.restart_private(request, next, PrivateTask::Gemini { state, redirect: true }, context)
            }
            gemini::Reply::Identity(status, message) => {
                let (pin,tls12)=tls.ok_or(Error::Unsupported)?;
                state.identity = Some(self.choose_tls_identity(owner, &target, &pin, status, message, tls12, context).await?);
                self.restart_private(request, target, PrivateTask::Gemini { state, redirect: false }, context)
            }
            gemini::Reply::Failure(status, message) => {
                if target.scheme()=="guppy" {
                    let mut content=Content::html(document::plain(url,&format!("Guppy error\n\n{message}")));content.status=400;
                    return Ok(Resource::Document(content));
                }
                let label = match status { 41 => "Server unavailable", 42 => "Server application error", 43 => "Proxy error", 44 => "Try again later", 51 => "Not found", 52 => "Gone", 53 => "Proxy request refused", 59 => "Bad request", _ if status < 50 => "Temporary failure", _ => "Permanent failure" };
                let mut content = Content::html(document::plain(url, &format!("{} {status}: {label}\n\n{message}",protocol_label(&target))));
                content.status = match status { 51 => 404, 52 => 410, 44 => 429, 40..=49 => 503, _ => 400 };
                if target.scheme()=="scroll" {content=content.abstract_view(state.metadata);}
                Ok(Resource::Document(content))
            }
        }
    }

    fn bind_private(&self, id: uuid::Uuid, owner: String, target: Url, task: PrivateTask) -> Result<PrivateGuard, Error> {
        let mut loads = self.0.private_loads.lock().unwrap_or_else(|e| e.into_inner());
        if loads.len() >= 32 || loads.iter().any(|load| load.id == id) { return Err(Error::Transport); }
        let token = uuid::Uuid::new_v4();
        loads.push(PrivateLoad { id, token, owner, target, task });
        Ok(PrivateGuard { shared: self.0.clone(), token })
    }

    fn take_private(&self, id: uuid::Uuid, owner: Option<&str>, target: &Url) -> Result<Option<PrivateTask>, Error> {
        let mut loads = self.0.private_loads.lock().unwrap_or_else(|e| e.into_inner());
        let Some(index) = loads.iter().position(|load| load.id == id) else { return Ok(None); };
        if Some(loads[index].owner.as_str()) != owner || &loads[index].target != target { return Err(Error::Address); }
        Ok(Some(loads.remove(index).task))
    }

    fn restart_private(&self, original: &Request, target: Url, task: PrivateTask, context: &FetchContext) -> Result<Resource, Error> {
        if context.is_cancelled() { return Err(Error::Cancelled); }
        let interaction = matches!(&task, PrivateTask::Gemini { redirect: false, .. } | PrivateTask::Scorpion {redirect:false,..} | PrivateTask::Titan(_) | PrivateTask::TitanNotice(_));
        if !interaction && original.redirect_count >= 10 { return Err(Error::Response); }
        let owner = original.target_webview_id.as_ref().map(|id| format!("{id:?}")).ok_or(Error::Cancelled)?;
        let mut guard = Some(self.bind_private(original.id.0, owner, target.clone(), task)?);
        let finished = GenericCallback::new(move |_| { drop(guard.take()); }).map_err(|_| Error::Transport)?;
        Ok(Resource::Restart(ProtocolRedirect { url: target.into(), count_redirect: !interaction, finished: Some(finished) }))
    }

    // Referenced form definitions are bounded data consumed by this handler,
    // rather than the original navigation/download response.
    async fn fetch_definition(&self, original: &Request, target: Url, context: &FetchContext) -> Result<Response, Error> {
        if context.is_cancelled() { return Err(Error::Cancelled); }
        if original.redirect_count >= 10 { return Err(Error::Response); }
        let owner = original.target_webview_id.as_ref().map(|id| format!("{id:?}")).ok_or(Error::Cancelled)?;
        let _guard = self.bind_private(original.id.0, owner, target.clone(), PrivateTask::GopherDefinition)?;
        let mut next = original.clone(); next.redirect_count += 1;
        let target = servo::protocol_handler::UrlWithBlobClaim::for_url(target.into()).map_err(|_| Error::Address)?;
        next.url_list = vec![target];
        Ok(servo::protocol_handler::fetch(next, &mut servo::protocol_handler::DiscardFetch, context).await)
    }

    async fn review(&self, owner: String, target: &Url, title: &str, label: &str, preview: String, button: &str, context: &FetchContext) -> Result<(), Error> {
        let spec = serde_json::json!({"title":title,"origin":format!("{}:{}", target.host_str().unwrap_or(""), target.port().unwrap_or(70)),
            "description":label,"submit_label":button,"fields":[{"kind":"preview","label":label,"value":preview,"options":[]}]});
        self.prompt_with_form(owner, String::new(), Some(spec.to_string()), context).await?;
        Ok(())
    }

    async fn open_gopher(&self, url: &Url, request: &gopher::Request, body: Option<&[u8]>, download: bool, owner: Option<&str>, context: &FetchContext) -> Result<gopher::Opened, Error> {
        if url.scheme()=="gophers" {
            let session=self.tls_session(url,owner.ok_or(Error::Cancelled)?,&mut GeminiLoad::default(),context).await?;
            cancellable(gopher::open_tls(url,request,body,download,session),context,20).await
        } else {cancellable(gopher::open(url,request,body,download),context,20).await}
    }

    async fn resolve_form(&self, original: &Request, target: &Url, bytes: &[u8], context: &FetchContext) -> Result<ask::Form, Error> {
        let mut definition = ask::definition(&String::from_utf8_lossy(bytes), false)?;
        let mut seen = Vec::new();
        let mut approved = vec![(target.host_str().unwrap_or("").to_owned(), target.port().unwrap_or(70))];
        for _ in 0..4 {
            match definition {
                ask::Definition::Form(form) => return Ok(form),
                ask::Definition::Reference(mut reference) => {
                    if target.scheme()=="gophers" {reference=gopher::secure_url(&reference)?;}
                    normalize_address(&mut reference)?;
                    if seen.contains(&reference) { return Err(Error::Response); }
                    seen.push(reference.clone());
                    let authority = (reference.host_str().unwrap_or("").to_owned(), reference.port().unwrap_or(70));
                    if !approved.contains(&authority) {
                        let owner = original.target_webview_id.as_ref().map(|id| format!("{id:?}")).ok_or(Error::Cancelled)?;
                        self.review(owner, &reference, "Review Gopher+ form source", "Continue retrieves a form definition. Answers go to the original form server.", format!("Form target: {}:{}\nDefinition: {}", target.host_str().unwrap_or(""), target.port().unwrap_or(70), reference), "Continue", context).await?;
                        approved.push(authority);
                    }
                    let response = self.fetch_definition(original, reference.clone(), context).await?;
                    if !response.status.is_success() || response.url().map(|u| u.as_str()) != Some(reference.as_str()) { return Err(Error::Response); }
                    let bytes = match &*response.body.lock() { ResponseBody::Done(bytes) => bytes.clone(), _ => return Err(Error::Response) };
                    definition = ask::definition(&String::from_utf8_lossy(&bytes), true)?;
                }
            }
        }
        match definition { ask::Definition::Form(form) => Ok(form), _ => Err(Error::Response) }
    }

    fn input_action(&self, url: &Url, owner: Option<&str>) -> Result<InputAction, Error> {
        if url.scheme() != "dome-smallnet-input" || url.query().is_some() || url.fragment().is_some() {
            return Err(Error::InputExpired);
        }
        let id = uuid::Uuid::parse_str(url.path()).map_err(|_| Error::InputExpired)?;
        self.0.inputs.lock().unwrap_or_else(|e| e.into_inner()).iter()
            .find(|action| action.id == id && Some(action.owner.as_str()) == owner).cloned().ok_or(Error::InputExpired)
    }

    fn input_link(&self,target:&Url,label:&str,owner:Option<&str>)->Option<Url> {
        self.action_link(target,label,owner,ActionKind::Input)
    }
    fn action_link(&self,target:&Url,label:&str,owner:Option<&str>,kind:ActionKind)->Option<Url> {
        if !((kind==ActionKind::FspPassword && target.scheme()=="fsp") || (kind==ActionKind::TitanCompose && target.scheme()=="titan") || (kind==ActionKind::ScorpionInput && matches!(target.scheme(),"scorpion"|"scorpions"|"gemini"|"mercury"|"guppy"|"scroll"|"spartan"|"http"|"https")) || target.scheme()=="scroll" || (kind==ActionKind::Input && matches!(target.scheme(),"spartan"|"dict"|"cso"|"ph"|"whois"|"rwhois")) || (matches!(kind,ActionKind::NewsServer|ActionKind::NewsFilter) && matches!(target.scheme(),"news"|"nntp"))) || target.as_str().len()>if matches!(target.scheme(),"dict"|"cso"|"ph"){16384}else{8192} {return None;}
        let owner=owner?;let id=uuid::Uuid::new_v4();
        let mut inputs=self.0.inputs.lock().unwrap_or_else(|e|e.into_inner());
        if inputs.len()>=4096{inputs.pop_front();}
        inputs.push_back(InputAction {id,owner:owner.into(),target:target.clone(),label:label.chars().take(160).collect(),kind});
        Url::parse(&format!("dome-smallnet-input:{id}")).ok()
    }

    async fn prompt(&self, owner: String, message: String, context: &FetchContext) -> Result<String, Error> {
        self.prompt_with_form(owner, message, None, context).await
    }

    async fn prompt_with_form(&self, owner: String, message: String, form: Option<String>, context: &FetchContext) -> Result<String, Error> {
        let (answer, receive) = oneshot::channel();
        {
            let mut prompts = self.0.prompts.lock().unwrap_or_else(|e| e.into_inner());
            prompts.retain(|prompt| !prompt.answer.is_closed());
            if prompts.len() >= 32 { return Err(Error::Transport); }
            prompts.push_back(Prompt { target: owner, message, form, answer });
        }
        self.0.wake.store(true, Ordering::Release);
        // User interaction has no network deadline. Navigation cancellation or
        // dropping the native prompt still releases the pending load.
        let receive = async { receive.await.map_err(|_| Error::Cancelled)?.ok_or(Error::Cancelled) };
        tokio::pin!(receive);
        loop {
            if context.is_cancelled() { return Err(Error::Cancelled); }
            tokio::select! { value = &mut receive => return value, _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
        }
    }

    async fn retrieve(&self, url: &mut Url, request: &mut Request, context: &FetchContext) -> Result<Resource, Error> {
        let download=request.initiator==Initiator::Download;
        let owner = request.target_webview_id.as_ref().map(|id| format!("{id:?}"));
        let private = self.take_private(request.id.0, owner.as_deref(), url)?;
        if url.scheme() == "dome-smallnet-input" {
            let action = self.input_action(url, owner.as_deref())?;
            if action.kind==ActionKind::FspPassword {
                let spec=serde_json::json!({"title":"FSP password","origin":action.target.as_str(),"description":"Use a password for this resource load. FSP sends passwords without encryption.","submit_label":"Read resource","fields":[{"kind":"password","label":"Password","value":"","required":true,"options":[]}]});
                let answer=self.prompt_with_form(action.owner,String::new(),Some(spec.to_string()),context).await;
                let value=answer.and_then(|answer| {let fields:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;let value=fields.as_array().filter(|v|v.len()==1).and_then(|v|v[0]["value"].as_str()).ok_or(Error::Address)?;Ok(value.as_bytes().to_vec())});
                return self.restart_private(request,action.target,PrivateTask::FspPassword(value),context);
            }
            if action.kind==ActionKind::TitanCompose {
                match self.prepare_titan(action.owner.clone(),&action.target,context).await {
                    Ok(submission)=>return self.restart_private(request,action.target,PrivateTask::Titan(Some(submission)),context),
                    Err(error)=>return self.restart_private(request,action.target,PrivateTask::TitanNotice(error),context),
                }
            }
            if matches!(action.target.scheme(),"news"|"nntp") {
                let next=match action.kind {ActionKind::NewsServer=>self.configure_news(action.owner,&action.target,context).await?,ActionKind::NewsFilter=>self.filter_news(action.owner,&action.target,context).await?,_=>return Err(Error::Address)};
                return Ok(Resource::Redirect(next));
            }
            if action.kind==ActionKind::ScorpionInput {return Ok(Resource::Redirect(self.scorpion_input(action.owner,&action.target,&action.label,context).await?));}
            if action.kind!=ActionKind::Input {
                return self.restart_private(request,action.target,PrivateTask::Gemini {state:GeminiLoad {metadata:action.kind==ActionKind::Abstract,..Default::default()},redirect:false},context);
            }
            if matches!(action.target.scheme(),"whois"|"rwhois") {
                let target=whois::Target::parse(&action.target)?;
                let spec=serde_json::json!({"title":format!("{} lookup",target.label()),"origin":gemini::authority(&target.url),"description":"Send a read-only lookup to this directory server.","submit_label":"Look up","fields":[{"kind":"text","label":"Lookup","value":target.query.as_deref().unwrap_or(""),"required":true,"options":[]},{"kind":"check","label":"Use ISO-8859-1 instead of UTF-8 for this server","value":if target.latin1{"1"}else{"0"},"options":[]}]});
                let answer=self.prompt_with_form(action.owner,String::new(),Some(spec.to_string()),context).await;
                let next=answer.and_then(|answer|{let fields:serde_json::Value=serde_json::from_str(&answer).map_err(|_|Error::Address)?;let fields=fields.as_array().filter(|v|v.len()==2).ok_or(Error::Address)?;let query=fields[0]["value"].as_str().ok_or(Error::Address)?;let latin1=fields[1]["checked"].as_bool().ok_or(Error::Address)?;target.with_query(query.into(),latin1)});
                return match next {Ok(next)=>Ok(Resource::Redirect(next.url)),Err(error)=>self.restart_private(request,target.url,PrivateTask::WhoisNotice(error),context)};
            }
            if action.target.scheme()=="dict" {
                let target=dict::Target::parse(&action.target)?;
                let next=self.dict_query(action.owner,&target,context).await?;
                return Ok(Resource::Redirect(next));
            }
            if matches!(action.target.scheme(),"cso"|"ph") {
                let target=cso::Target::parse(&action.target)?;
                let next=self.cso_query(action.owner,&target,context).await?;return Ok(Resource::Redirect(next));
            }
            let value=if action.target.scheme()=="scroll" {self.query_input(action.owner,&action.target,&action.label,false,context).await?}
                else {self.prompt(action.owner,format!("{}\n{}",action.target,action.label),context).await?};
            if value.len() > MAX_BYTES { return Err(Error::TooLarge); }
            *url = action.target;
            url.set_query(Some(&percent_encoding::utf8_percent_encode(&value, percent_encoding::NON_ALPHANUMERIC).to_string()));
            return Ok(Resource::Redirect(url.clone()));
        }
        if url.scheme()=="titan" {return self.retrieve_titan(url,request,private,context).await;}
        if matches!(url.scheme(),"scorpion"|"scorpions") {
            let state=match private {Some(PrivateTask::Scorpion{state,..})=>state,None=>scorpion::Load::default(),_=>return Err(Error::Address)};
            return self.retrieve_scorpion(url,request,state,context).await;
        }
        if matches!(url.scheme(),"whois"|"rwhois") {return self.retrieve_whois(url,request,private,context).await;}
        if url.scheme()=="fsp" {return self.retrieve_fsp(url,request,private,context).await;}
        if url.scheme()=="ftp" {
            if private.is_some() {return Err(Error::Address);}
            return self.retrieve_ftp(url,request,context).await;
        }
        if matches!(url.scheme(),"dict"|"cso"|"ph"|"news"|"nntp") && (!url.username().is_empty() || url.password().is_some()) {
            // Optional DICT authentication is not silently downgraded to an
            // anonymous lookup. No supplied credentials are sent to the server.
            for claim in &mut request.url_list {
                let mut clean=claim.url().into_url();
                if matches!(clean.scheme(),"dict"|"cso"|"ph"|"news"|"nntp") {
                    clean.set_username("").map_err(|_|Error::Address)?;clean.set_password(None).map_err(|_|Error::Address)?;
                    *claim=servo::protocol_handler::UrlWithBlobClaim::for_url(clean.into()).map_err(|_|Error::Address)?;
                }
            }
            url.set_username("").map_err(|_|Error::Address)?;url.set_password(None).map_err(|_|Error::Address)?;
            let mut content=Content::html(document::plain(url,&format!("{} authentication is not supported. Open this server without user information to browse its public resources.",protocol_label(url))));content.status=401;
            return Ok(Resource::Document(content));
        }
        if matches!(url.scheme(),"news"|"nntp") {
            if private.is_some(){return Err(Error::Address);}
            return self.retrieve_news(url,request,context).await;
        }
        if url.scheme() == "spartan" {
            // Userinfo has no protocol meaning and is never sent or retained in
            // the effective document address.
            url.set_username("").map_err(|_| Error::Address)?;
            url.set_password(None).map_err(|_| Error::Address)?;
        }
        if url.host_str().is_none() || url.password().is_some() || (url.scheme() != "finger" && !url.username().is_empty()) {
            return Err(Error::Address);
        }
        normalize_address(url)?;
        if matches!(url.scheme(),"gemini"|"mercury"|"guppy"|"scroll") && url.path().is_empty() { url.set_path("/"); }
        if url.as_str() != request.current_url().as_str() {
            return Ok(Resource::Redirect(url.clone()));
        }
        if matches!(url.scheme(),"gemini"|"mercury"|"guppy"|"scroll") {
            let state = match private {
                Some(PrivateTask::Gemini { state, .. }) => state,
                None => GeminiLoad {metadata:url.scheme()=="scroll" && !download && request.protocol_state.as_deref()==Some("abstract"),..Default::default()},
                _ => return Err(Error::Address),
            };
            return self.retrieve_gemini_family(url, request, state, context).await;
        }
        if url.scheme()=="dict" {
            if private.is_some(){return Err(Error::Address);}
            return self.retrieve_dict(url,request,context).await;
        }
        if matches!(url.scheme(),"cso"|"ph") {
            if private.is_some(){return Err(Error::Address);}
            return self.retrieve_cso(url,request,context).await;
        }
        if matches!(url.scheme(), "gopher" | "gopher+" | "gophers") {
            let candidate=gopher::Request::parse(url)?;
            if candidate.kind==b'2' {
                if private.is_some() || candidate.supplied.is_some() || !matches!(candidate.command,None|Some(gopher::Command::Default)){return Err(Error::Unsupported);}
                return Ok(Resource::Redirect(cso::gopher_endpoint(url)?));
            }
            let (parsed, supplied) = match private {
                Some(PrivateTask::GopherDefinition) => {
                    let parsed = candidate;
                    if parsed.kind != b'0' || parsed.supplied.is_some() || matches!(parsed.command, Some(gopher::Command::Ask)) { return Err(Error::Address); }
                    let gopher::Opened::Buffered(reply)=self.open_gopher(url,&parsed,None,false,owner.as_deref(),context).await? else {return Err(Error::Response);};
                    if reply.failed { return Err(Error::Response); }
                    let bytes = if parsed.command.is_none() { gopher_text(&String::from_utf8_lossy(&reply.bytes)).into_bytes() } else { reply.bytes };
                    return Ok(Resource::Document(Content::raw(bytes, "application/octet-stream".into())));
                }
                Some(PrivateTask::Submission { parsed, bytes }) => (parsed, Some(bytes)),
                Some(PrivateTask::WhoisNotice(_)|PrivateTask::FspPassword(_)|PrivateTask::Gemini { .. }|PrivateTask::Scorpion { .. }|PrivateTask::Titan(_)|PrivateTask::TitanNotice(_)) => return Err(Error::Address),
                None => (candidate, None),
            };
            if let Some(bytes) = &parsed.supplied {
                let clean = parsed.clean_url(url)?;
                // Remove the URI-supplied data from the final Fetch URL list,
                // including cancellation/error paths, before asking the user.
                sanitize_supplied_chain(&mut request.url_list)?;
                *url = clean.clone();
                self.review(owner.clone().ok_or(Error::Cancelled)?, &clean, "Review Gopher+ data", "Submit sends this URI-supplied data to the displayed server.", ask::preview(bytes), "Submit", context).await?;
                return self.restart_private(request, clean, PrivateTask::Submission { parsed: parsed.without_supplied(), bytes: bytes.clone() }, context);
            }
            if supplied.is_none() && parsed.needs_search() {
                let target = owner.clone().ok_or(Error::Cancelled)?;
                let value = self.prompt(target, format!("Search {}", url.host_str().unwrap_or("Gopher")), context).await?;
                return Ok(Resource::Redirect(parsed.with_search(&value, url)?));
            }
            let ask = matches!(parsed.command, Some(gopher::Command::Ask));
            let mut transfer = if ask { parsed.with_command(gopher::Command::Attributes("!+ASK".into())) } else { parsed.clone() };
            let mut opened=self.open_gopher(url,&transfer,supplied.as_deref(),download,owner.as_deref(),context).await?;
            if ask && matches!(&opened,gopher::Opened::Buffered(reply) if !reply.failed) {
                let gopher::Opened::Buffered(reply)=&opened else {unreachable!()};
                let form = self.resolve_form(request, url, &reply.bytes, context).await?;
                let mut spec = serde_json::to_value(&form).map_err(|_| Error::Response)?;
                spec["origin"] = serde_json::json!(format!("Gopher+ form — {}:{}", url.host_str().unwrap_or(""), url.port().unwrap_or(70)));
                let answer = self.prompt_with_form(owner.clone().ok_or(Error::Cancelled)?, String::new(), Some(spec.to_string()), context).await?;
                let submission = tokio::task::spawn_blocking(move || form.submission(&answer)).await.map_err(|_| Error::Transport)??;
                if context.is_cancelled() { return Err(Error::Cancelled); }
                transfer = parsed.with_command(gopher::Command::Default);
                // Same target and original Fetch policy/cancellation. Answers
                // exist only in this pending load, never in history or URLs.
                opened=self.open_gopher(url,&transfer,Some(&submission.bytes),download || submission.save.is_some(),owner.as_deref(),context).await?;
                if let Some(path)=submission.save {
                    if let gopher::Opened::Stream(stream)=opened {
                        if download {return Ok(Resource::Stream(stream.also_save_to(path)));}
                        stream.save(&path,context).await?;
                        return Ok(Resource::Document(Content::html(document::plain(url, "Gopher+ response saved to the selected file."))));
                    }
                }
            }
            let reply=match opened {
                gopher::Opened::Stream(stream)=>{
                    if download {return Ok(Resource::Stream(stream));}
                    return self.save_transfer(url,stream,parsed.filename(),owner.ok_or(Error::Cancelled)?,context).await;
                },
                gopher::Opened::Buffered(reply)=>reply,
            };
            let text = String::from_utf8_lossy(&reply.bytes);
            if reply.failed {
                let code = text.split_ascii_whitespace().next();
                let mut content = Content::html(document::gopher_error(url, &text));
                content.status = match code { Some("1") => 404, Some("2") => 503, Some("3") => 410, _ => 500 };
                return Ok(Resource::Document(content));
            }
            if download {
                let mime=if let Some(gopher::Command::View {mime,..})=&parsed.command {mime.clone()}
                    else if matches!(transfer.command,Some(gopher::Command::Attributes(_))) {"text/plain".into()}
                    else {match parsed.kind {b'0'=>"text/plain".into(),b'1'|b'7'=>"application/gopher-menu".into(),b'h'=>"text/html".into(),b'g'=>"image/gif".into(),b'I'=>image_mime(url,&reply.bytes),_=>"application/octet-stream".into()}};
                let bytes=if parsed.command.is_none() && matches!(parsed.kind,b'0'|b'1'|b'7'){gopher_resource(&reply.bytes)}else{reply.bytes};
                return Ok(Resource::Document(Content::raw(bytes,mime)));
            }
            if matches!(transfer.command, Some(gopher::Command::Attributes(_))) {
                return Ok(Resource::Document(Content::html(document::gopher_attributes(url, &text))));
            }
            if let Some(gopher::Command::View { mime, .. }) = &parsed.command {
                return if mime.eq_ignore_ascii_case("application/gopher-menu") {
                    Ok(Resource::Document(Content::html(document::gopher(url, &text))))
                } else { self.file_resource(url,mime,reply.bytes,owner.clone().ok_or(Error::Cancelled)?,context,false).await };
            }
            return match parsed.kind {
                b'1' | b'7' => Ok(Content::html(document::gopher(url, &text))),
                b'0' if parsed.command.is_none() => Ok(Content::html(document::plain(url, &gopher_text(&text)))),
                b'0' => Ok(Content::html(document::plain(url, &text))),
                b'h' => self.typed_content(url, "text/html", reply.bytes, owner.as_deref(),false),
                b'g' => Ok(Content::image(url,"image/gif",&reply.bytes,None)),
                b'I' => {
                    let mime=image_mime(url,&reply.bytes);
                    return self.file_resource(url,&mime,reply.bytes,owner.clone().ok_or(Error::Cancelled)?,context,false).await;
                },
                b'9' | b'4' | b'5' | b'6' => return self.file_resource(url,"application/octet-stream",reply.bytes,owner.clone().ok_or(Error::Cancelled)?,context,false).await,
                _ => Err(Error::Unsupported),
            }.map(Resource::Document);
        }
        if url.scheme()=="ftp" {
            if private.is_some() {return Err(Error::Address);}
            return self.retrieve_ftp(url,request,context).await;
        }
        if url.scheme() == "spartan" {
            let reply = cancellable(spartan::open(url,download), context, 20).await?;
            return match reply {
                spartan::Reply::Stream(stream) => {
                    if download {return Ok(Resource::Stream(stream));}
                    self.save_transfer(url,stream,download_filename(url).unwrap_or_else(||"download".into()),owner.clone().ok_or(Error::Cancelled)?,context).await
                },
                spartan::Reply::Success(mime, bytes) => self.file_resource(url,&mime,bytes,owner.clone().ok_or(Error::Cancelled)?,context,download).await,
                spartan::Reply::Redirect(target) => Ok(Resource::Redirect(target)),
                spartan::Reply::Failure(status, text) => {
                    let mut content = Content::html(document::plain(url, &text));
                    content.status = status;
                    Ok(Resource::Document(content))
                }
            };
        }
        let scheme = url.scheme().to_owned();
        let (port, selector, kind) = match scheme.as_str() {
            "nex" => {
                if url.query().is_some() { return Err(Error::Address); }
                (1900, decoded(url.path())?, b'n')
            }
            "finger" => {
                if url.query().is_some() { return Err(Error::Address); }
                let name = if url.path().trim_matches('/').is_empty() { url.username() } else { url.path().trim_start_matches('/') };
                (79, decoded(name)?, b'f')
            }
            _ => return Err(Error::Unsupported),
        };
        if selector.len() > MAX_REQUEST { return Err(Error::Address); }
        let mime=if scheme=="finger" || url.path().ends_with('/') || url.path().is_empty(){"text/plain".into()}else{mime_guess::from_path(url.path()).first_or_text_plain().to_string()};
        let mime_kind:mime::Mime=mime.parse().map_err(|_|Error::Response)?;
        let binary=!(mime_kind.type_()==mime::TEXT || mime_kind.type_()==mime::IMAGE || mime_kind.essence_str()=="application/xhtml+xml");
        if download || binary {
            let reader=cancellable(open_exchange(url,port,selector),context,20).await?;
            let stream=transfer::Transfer::new(mime,None,reader);
            if download{return Ok(Resource::Stream(stream));}
            return self.save_transfer(url,stream,download_filename(url).unwrap_or_else(||"download".into()),owner.clone().ok_or(Error::Cancelled)?,context).await;
        }
        let bytes = cancellable(exchange(url, port, selector), context, 20).await?;
        let text = String::from_utf8_lossy(&bytes);
        match (scheme.as_str(), kind) {
            ("finger", _) => Ok(Content::html(document::plain(url, &text))),
            ("nex", _) if url.path().ends_with('/') || url.path().is_empty() => Ok(Content::html(document::nex(url, &text))),
            ("nex", _) => {
                let mime = mime_guess::from_path(url.path()).first_or_text_plain().to_string();
                return self.file_resource(url,&mime,bytes,owner.clone().ok_or(Error::Cancelled)?,context,false).await;
            }
            _ => Err(Error::Unsupported),
        }.map(Resource::Document)
    }

    fn typed_content(&self, url: &Url, metadata: &str, bytes: Vec<u8>, owner: Option<&str>,download:bool) -> Result<Content, Error> {
        let mime: mime::Mime = metadata.parse().map_err(|_| Error::Response)?;
        if download {return Ok(Content::raw(bytes,mime.to_string()));}
        if mime.type_()==mime::IMAGE {return Ok(Content::image(url,mime.as_ref(),&bytes,None));}
        if mime.type_() == mime::TEXT && mime.subtype() != mime::HTML {
            let encoding = mime.get_param(mime::CHARSET).map(|charset| encoding_rs::Encoding::for_label(charset.as_str().as_bytes()).ok_or(Error::Unsupported))
                .transpose()?.unwrap_or(encoding_rs::UTF_8);
            let (text, _, _) = encoding.decode(&bytes);
            if mime.subtype().as_str() == "gemini" {
                let language = mime.get_param("lang").map(|value| value.as_str());
                Ok(Content::html(document::gemtext(url, &text, language, |target, label| self.input_link(target, label, owner))))
            } else if mime.subtype().as_str()=="scroll" {
                let language=mime.get_param("lang").or_else(||mime.get_param("language")).map(|v|v.as_str());
                Ok(Content::html(document::scroll(url,&text,language,&scroll::Metadata::default(),None,false,|target,label|self.input_link(target,label,owner))))
            } else { Ok(Content::html(document::plain(url, &text))) }
        } else { Ok(Content::raw(bytes, mime.to_string())) }
    }
}

fn protocol_label(url:&Url)->&'static str {
    match url.scheme(){"whois"=>"WHOIS","rwhois"=>"RWhois","fsp"=>"FSP","titan"=>"Titan","scorpion"|"scorpions"=>"Scorpion","scroll"=>"Scroll","ftp"=>"FTP","news"|"nntp"=>"News", "dict"=>"DICT","cso"|"ph"=>"CSO/Ph","gemini"=>"Gemini","mercury"=>"Mercury","guppy"=>"Guppy","gopher"=>"Gopher","gopher+"=>"Gopher+","gophers"=>"Gopher TLS","nex"=>"Nex","spartan"=>"Spartan","finger"=>"Finger",_=>"Smallnet"}
}

fn image_mime(url:&Url,bytes:&[u8])->String {
    image::guess_format(bytes).map(|format|format.to_mime_type().to_owned()).unwrap_or_else(|_| {
        // URI command fields and percent escapes are not part of the resource
        // filename. SVG detection needs that filename rather than raster magic.
        let filename=download_filename(url).unwrap_or_else(||url.path().to_owned());
        mime_guess::from_path(filename).first_or_octet_stream().to_string()
    })
}
pub fn download_filename(url:&Url)->Option<String> {
    match url.scheme() {
        "gopher"|"gopher+"|"gophers"=>gopher::Request::parse(url).ok().map(|request|request.filename()),
        "whois"|"rwhois"=>Some(format!("{}.txt",url.scheme())),
        "fsp"=>fsp::Target::parse(url).ok().map(|target|target.filename()),
        "ftp"=>ftp::Target::parse(url).ok().map(|target|target.filename()),
        "dict"=>Some("dictionary.txt".into()),
        "cso"|"ph"=>Some("directory.txt".into()),
        "news"|"nntp"=>news::Target::parse(url).ok().map(|t|if t.part.is_some(){"attachment".into()}else if matches!(t.kind,news::Kind::Article(_)|news::Kind::Group(_,Some(_))){"article.eml".into()}else{"news.txt".into()}),
        "scorpion"|"scorpions"|"gemini"|"mercury"|"guppy"|"scroll"|"nex"|"spartan"|"finger"=>Some(percent_decode_str(url.path_segments().and_then(|mut parts|parts.next_back()).unwrap_or("")).decode_utf8_lossy().into_owned()),
        _=>None,
    }
}

fn gopher_resource(bytes:&[u8])->Vec<u8> {
    let mut body=Vec::new();
    for line in bytes.split_inclusive(|byte|*byte==b'\n') {
        if matches!(line,b".\r\n"|b".\n"|b"."){break;}
        body.extend_from_slice(if line.starts_with(b".."){&line[1..]}else{line});
    }
    body
}

fn sanitize_supplied_chain(urls: &mut [servo::protocol_handler::UrlWithBlobClaim]) -> Result<(), Error> {
    // Keep the navigation's earlier origins/redirect ordering while removing
    // supplied native form data, including pre-normalization URI entries.
    for claim in urls.iter_mut().rev() {
        let url = claim.url().into_url();
        if matches!(url.scheme(), "gopher" | "gopher+" | "gophers") {
            let parsed = gopher::Request::parse(&url)?;
            if parsed.supplied.is_some() {
                *claim = servo::protocol_handler::UrlWithBlobClaim::for_url(parsed.clean_url(&url)?.into()).map_err(|_| Error::Address)?;
            }
        }
    }
    Ok(())
}

fn normalize_address(url: &mut Url) -> Result<(), Error> {
    if url.scheme()=="scroll" {scroll::validate_host(url)?;}
    let host = percent_decode_str(url.host_str().ok_or(Error::Address)?).decode_utf8().map_err(|_| Error::Address)?;
    let host = url::Host::parse(&host).map_err(|_| Error::Address)?.to_string();
    url.set_host(Some(&host)).map_err(|_| Error::Address)?;
    if let Some(port) = url.port() {
        let default = match url.scheme() {
            "gopher" | "gopher+" | "gophers" => 70, "nex" => 1900, "finger" => 79, "spartan" => 300,
            "scorpion"|"scorpions"=>1517, "gemini"|"titan" => 1965, "mercury" => 1961, "guppy" => 6775, "dict" => 2628, "cso"|"ph"=>105, "news"|"nntp"=>119, "scroll" => 5699, "ftp"|"fsp" => 21, "whois"=>43,"rwhois"=>4321, _ => return Err(Error::Address),
        };
        if port != default && !(matches!(url.scheme(),"news"|"nntp") && port==563) && servo::protocol_handler::is_bad_port(port) { return Err(Error::Address); }
    }
    Ok(())
}

/// Resource address for sessions, bookmarks and history. Submission data is
/// per-load state: reopening a saved address must not repeat an upload.
pub fn persistent_url(url:&Url)->Option<Url> {
    let mut url=url.clone();
    if matches!(url.scheme(),"whois"|"rwhois") {return whois::Target::parse(&url).ok().map(|target|target.url);}
    if url.scheme()=="fsp" {return fsp::Target::parse(&url).ok().map(|target|target.url);}
    if url.scheme()=="titan" {return titan::public_url(&url).ok();}
    if matches!(url.scheme(),"news"|"nntp") {return news::Target::parse(&url).ok().map(|target|target.url);}
    if !matches!(url.scheme(),"scorpion"|"scorpions"|"gopher"|"gopher+"|"gophers"|"gemini"|"mercury"|"guppy"|"dict"|"cso"|"ph"|"scroll"|"ftp"|"nex"|"finger"|"spartan") {return None;}
    normalize_address(&mut url).ok()?;
    if url.port()==Some(0) {return None;}
    if url.scheme()=="ftp" {return ftp::Target::parse(&url).ok().map(|target|target.url);}
    if url.scheme()=="spartan" {
        url.set_username("").ok()?;url.set_password(None).ok()?;
        url.set_query(None);
    }
    if url.password().is_some() || (url.scheme()!="finger" && !url.username().is_empty()) {return None;}
    match url.scheme() {
        "gopher"|"gopher+"|"gophers"=>{
            let parsed=gopher::Request::parse(&url).ok()?;
            if parsed.supplied.is_some() {return parsed.clean_url(&url).ok();}
        },
        "scorpion"|"scorpions"=>{scorpion::request_bytes(&url).ok()?;},
        "gemini"=>{gemini::request_bytes(&url).ok()?;},
        "mercury"=>{mercury::request_bytes(&url).ok()?;},
        "guppy"=>{guppy::request_bytes(&url).ok()?;},
        "dict"=>{dict::Target::parse(&url).ok()?;},
        "cso"|"ph"=>{cso::Target::parse(&url).ok()?;},
        "scroll"=>{scroll::request_bytes(&url,"en",false).ok()?;},
        "nex"|"finger"=>{
            if url.query().is_some() {return None;}
            let selector=if url.scheme()=="finger" && url.path().trim_matches('/').is_empty(){url.username()}else{url.path()};
            if decoded(selector).ok()?.len()>MAX_REQUEST {return None;}
        },
        "spartan"=>{spartan::request_bytes(&url).ok()?;},
        _=>return None,
    }
    Some(url)
}

fn decoded(text: &str) -> Result<Vec<u8>, Error> {
    let bytes: Vec<u8> = percent_decode_str(text).collect();
    if bytes.iter().any(|b| *b < 32 || *b == 127) { return Err(Error::Address); }
    Ok(bytes)
}

fn gopher_text(text: &str) -> String {
    let mut result = String::new();
    for line in text.lines() {
        if line == "." { break; }
        result.push_str(line.strip_prefix("..").map(|rest| format!(".{rest}")).as_deref().unwrap_or(line));
        result.push('\n');
    }
    result
}

async fn open_exchange(url:&Url,default_port:u16,mut selector:Vec<u8>)->Result<TcpStream,Error> {
    if selector.len()>MAX_REQUEST{return Err(Error::Address);}
    let host = url.host_str().ok_or(Error::Address)?.trim_matches(['[', ']']);
    let port = url.port().unwrap_or(default_port);
    if port == 0 { return Err(Error::Address); }
    let mut stream = TcpStream::connect((host, port)).await.map_err(|_| Error::Transport)?;
    selector.extend_from_slice(b"\r\n");
    stream.write_all(&selector).await.map_err(|_| Error::Transport)?;
    Ok(stream)
}

async fn exchange(url:&Url,default_port:u16,selector:Vec<u8>)->Result<Vec<u8>,Error> {
    let stream=open_exchange(url,default_port,selector).await?;
    let mut bytes = Vec::new();
    stream.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes).await.map_err(|_| Error::Transport)?;
    if bytes.len() > MAX_BYTES { return Err(Error::TooLarge); }
    Ok(bytes)
}

async fn cancellable<T>(future: impl Future<Output = Result<T, Error>>, context: &FetchContext, seconds: u64) -> Result<T, Error> {
    tokio::pin!(future);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        if context.is_cancelled() { return Err(Error::Cancelled); }
        tokio::select! {
            value = &mut future => return value,
            _ = tokio::time::sleep_until(deadline) => return Err(Error::Timeout),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn download_bytes_keep_encoding_line_endings_and_exclude_gopher_framing() {
        let smallnet=Smallnet::new(Arc::new(AtomicBool::new(false)));let handler=Handler(smallnet.0.clone());
        let url=Url::parse("gemini://host/file.txt").unwrap();let bytes=b"caf\xe9\r\n..text\r\n";
        let content=handler.typed_content(&url,"text/plain; charset=iso-8859-1",bytes.to_vec(),None,true).unwrap();
        assert_eq!(content.bytes,bytes);assert!(!content.image_data);
        assert_eq!(gopher_resource(b"caf\xe9\r\n..text\r\n.\r\nignored"),b"caf\xe9\r\n.text\r\n");
        assert_eq!(gopher_resource(b"plain\n..dot\n.\n"),b"plain\n.dot\n");
    }
    #[test]
    fn svg_mime_uses_decoded_resource_names_without_protocol_command_fields() {
        let svg=b"<svg xmlns='http://www.w3.org/2000/svg'/>";
        for text in ["gophers://host/I/picture.svg%09%09%2B","gopher+://host/I%2Fpicture%2Esvg","gopher://host/I/picture.svg%09%09%2B","nex://host/picture%2Esvg"] {
            assert_eq!(image_mime(&Url::parse(text).unwrap(),svg),"image/svg+xml","{text}");
        }
        assert_eq!(image_mime(&Url::parse("gophers://host/I/picture.svg%09%09%2B").unwrap(),b"\x89PNG\r\n\x1a\n"),"image/png");
    }

    #[test]
    fn native_image_presentation_retains_bytes_and_does_not_enable_images_in_other_documents() {
        use base64::{Engine as _,engine::general_purpose::STANDARD};
        let smallnet=Smallnet::new(Arc::new(AtomicBool::new(false)));let handler=Handler(smallnet.0.clone());
        let url=Url::parse("nex://host/picture.png").unwrap();let bytes=b"\x89PNG\r\n\x1a\nfixture";
        let displayed=handler.typed_content(&url,"image/png",bytes.to_vec(),None,false).unwrap();
        assert!(displayed.image_data);let html=String::from_utf8(displayed.bytes).unwrap();assert!(html.contains(url.as_str()));
        let encoded=html.split("data:image/png;base64,").nth(1).unwrap().split('"').next().unwrap();assert_eq!(STANDARD.decode(encoded).unwrap(),bytes);
        let saved=handler.typed_content(&url,"image/png",bytes.to_vec(),None,true).unwrap();assert_eq!(saved.bytes,bytes);assert_eq!(saved.mime,"image/png");assert!(!saved.image_data);
        let ordinary=handler.typed_content(&url,"text/html",b"<p>Document</p>".to_vec(),None,false).unwrap();assert!(!ordinary.image_data);
    }
    #[test]
    fn sanitizing_supplied_data_retains_the_navigation_origin_chain() {
        let mut urls: Vec<_> = ["https://example.com/original", "gopher+://host/0form%09%09%2B%091%0D%0A%2B3%0D%0Aone", "gopher+://host:70/0form%09%09%2B%091%0D%0A%2B3%0D%0Aone"].iter()
            .map(|url| servo::protocol_handler::UrlWithBlobClaim::for_url(Url::parse(url).unwrap().into()).unwrap()).collect();
        sanitize_supplied_chain(&mut urls).unwrap();
        assert_eq!(urls.len(), 3);
        assert_eq!(urls[0].as_str(), "https://example.com/original");
        assert!(urls[1].as_str().ends_with("%09%09%3F"));
        assert!(urls[2].as_str().ends_with("%09%09%3F"));
        assert!(urls.iter().all(|url| !url.as_str().contains("one")));
    }
    #[test]
    fn internal_fetch_actions_are_one_use_owner_and_target_bound_and_removed_on_drop() {
        let smallnet = Smallnet::new(Arc::new(AtomicBool::new(false)));
        let handler = Handler(smallnet.0.clone());
        let id = uuid::Uuid::new_v4();
        let url = Url::parse("gopher+://host/0definition").unwrap();
        let guard = handler.bind_private(id, "view-a".into(), url.clone(), PrivateTask::GopherDefinition).unwrap();
        assert!(handler.take_private(id, Some("view-b"), &url).is_err());
        assert!(handler.take_private(id, Some("view-a"), &Url::parse("gopher+://other/0definition").unwrap()).is_err());
        assert!(matches!(handler.take_private(id, Some("view-a"), &url).unwrap(), Some(PrivateTask::GopherDefinition)));
        assert!(handler.take_private(id, Some("view-a"), &url).unwrap().is_none());
        let next_guard = handler.bind_private(id, "view-a".into(), url.clone(), PrivateTask::GopherDefinition).unwrap();
        drop(guard);
        assert!(matches!(handler.take_private(id, Some("view-a"), &url).unwrap(), Some(PrivateTask::GopherDefinition)));
        drop(next_guard);
        let guard = handler.bind_private(id, "view-a".into(), url.clone(), PrivateTask::GopherDefinition).unwrap();
        drop(guard);
        assert!(smallnet.0.private_loads.lock().unwrap().is_empty());
        let mut guards = Vec::new();
        for _ in 0..32 { guards.push(handler.bind_private(uuid::Uuid::new_v4(), "view-a".into(), url.clone(), PrivateTask::GopherDefinition).unwrap()); }
        assert!(handler.bind_private(uuid::Uuid::new_v4(), "view-a".into(), url, PrivateTask::GopherDefinition).is_err());
        drop(guards);
        assert!(smallnet.0.private_loads.lock().unwrap().is_empty());
    }
    #[test]
    fn protocol_selectors_preserve_encoded_octets() {
        assert_eq!(gopher::Request::parse(&Url::parse("gopher://host/").unwrap()).unwrap().kind, b'1');
        assert!(gopher::Request::parse(&Url::parse("gopher://host/0/a%20b%25c").unwrap()).is_ok());
        assert_eq!(decoded("%E4%B8%96%E7%95%8C"), Ok("世界".as_bytes().to_vec()));
    }
    #[test]
    fn text_framing_preserves_escaped_periods() {
        assert_eq!(gopher_text("one\r\n..two\r\n.\r\nignored"), "one\n.two\n");
    }
    #[test]
    fn line_requests_reject_control_octets() {
        for value in ["%00", "%0D", "%0A", "%09", "%7F"] { assert_eq!(decoded(value), Err(Error::Address)); }
    }

    #[test]
    fn native_addresses_reuse_network_port_policy_with_protocol_defaults() {
        for text in ["finger://host:79/", "gopher+://host:70/", "spartan://host:300/", "nex://host:1900/", "nex://host:31000/"] {
            assert!(normalize_address(&mut Url::parse(text).unwrap()).is_ok());
        }
        for text in ["nex://host:0/", "gopher://host:79/", "spartan://host:21/", "finger://host:25/"] {
            assert_eq!(normalize_address(&mut Url::parse(text).unwrap()), Err(Error::Address));
        }
    }

    #[test]
    fn input_actions_require_their_owning_view_and_do_not_prompt_on_render() {
        let smallnet = Smallnet::new(Arc::new(AtomicBool::new(false)));
        let handler = Handler(smallnet.0.clone());
        let target = Url::parse("spartan://example.com/search").unwrap();
        let action = handler.input_link(&target, "Search", Some("view-a")).unwrap();
        assert_eq!(handler.input_action(&action, Some("view-a")).unwrap().target, target);
        assert!(matches!(handler.input_action(&action, Some("view-b")), Err(Error::InputExpired)));
        assert!(matches!(handler.input_action(&Url::parse(&format!("dome-smallnet-input:{}", uuid::Uuid::new_v4())).unwrap(), Some("view-a")), Err(Error::InputExpired)));
        assert!(smallnet.next_prompt("view-a").is_none());
        assert!(handler.input_link(&Url::parse("https://example.com/").unwrap(), "Input", Some("view-a")).is_none());
    }

    #[test]
    fn input_action_retention_is_bounded_and_old_links_expire() {
        let smallnet = Smallnet::new(Arc::new(AtomicBool::new(false)));
        let handler = Handler(smallnet.0.clone());
        let target = Url::parse("spartan://example.com/search").unwrap();
        let first = handler.input_link(&target, "Search", Some("view-a")).unwrap();
        for _ in 0..4096 { handler.input_link(&target, "Search", Some("view-a")); }
        assert!(matches!(handler.input_action(&first, Some("view-a")), Err(Error::InputExpired)));
        assert_eq!(smallnet.0.inputs.lock().unwrap().len(), 4096);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_exchange_preserves_the_requested_path_and_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let byte = stream.read_u8().await.unwrap();
                request.push(byte);
                if byte == b'\n' { break; }
            }
            assert_eq!(request, b"/hello world\r\n");
            stream.write_all("Hello 世界\n".as_bytes()).await.unwrap();
        });
        let url = Url::parse(&format!("nex://127.0.0.1:{port}/hello%20world")).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), exchange(&url, 1900, decoded(url.path()).unwrap())).await.unwrap().unwrap();
        assert_eq!(result, "Hello 世界\n".as_bytes());
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_exchange_enforces_the_response_limit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            while stream.read_u8().await.unwrap() != b'\n' {}
            let _ = stream.write_all(&vec![b'x'; MAX_BYTES + 1]).await;
        });
        let url = Url::parse(&format!("nex://127.0.0.1:{port}/")).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), exchange(&url, 1900, b"/".to_vec())).await.unwrap();
        assert_eq!(result, Err(Error::TooLarge));
        server.await.unwrap();
    }
}
