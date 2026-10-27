// SPDX-License-Identifier: GPL-3.0-or-later
use super::{Payload, Submission, public_url, request_bytes, upload};
use super::super::{ActionKind, Content, Error, FetchContext, GeminiLoad, Handler, PrivateTask, Request, Resource, document, gemini};
use url::Url;

fn fields(answer: &str, count: usize) -> Result<Vec<serde_json::Value>, Error> {
    let fields: Vec<serde_json::Value> = serde_json::from_str(answer).map_err(|_| Error::Address)?;
    if fields.len()!=count {return Err(Error::Address);} Ok(fields)
}

impl Handler {
    pub(in super::super) fn titan_page(&self, target:&Url, owner:&str, title:&str, message:&str, prepare:bool) -> Result<Resource,Error> {
        let mut read=target.clone();read.set_scheme("gemini").map_err(|_|Error::Address)?;
        let mut body=format!("<h1>{}</h1><p>{}</p><p><a href=\"{}\" rel=noreferrer>View resource with Gemini</a></p>",document::escape(title),document::escape(message),document::escape(read.as_str()));
        if prepare {
            if let Some(action)=self.action_link(target,"Prepare a Titan action",Some(owner),ActionKind::TitanCompose) {body.push_str(&format!("<p><a href=\"{}\">Prepare upload or deletion</a></p>",document::escape(action.as_str())));}
        }
        Ok(Resource::Document(Content::html(document::page_with_metadata(target,&body,None,Some(title)))))
    }

    pub(in super::super) fn titan_stopped(&self,target:&Url,owner:&str,error:Error,attempted:bool,context:&FetchContext)->Result<Resource,Error> {
        if context.is_cancelled(){return Err(Error::Cancelled);}
        if error==Error::Cancelled {
            return self.titan_page(target,owner,if attempted {"Titan submission stopped"}else{"Titan submission"},if attempted {"No further submission was made."}else{"No submission was sent."},true);
        }
        if attempted {return self.titan_page(target,owner,"Titan outcome unconfirmed",Error::Submission.message(),false);}
        self.titan_page(target,owner,"Titan action not sent",&format!("No submission was sent. {}",error.message()),true)
    }

    pub(in super::super) async fn prepare_titan(&self, owner: String, target: &Url, context: &FetchContext) -> Result<Submission, Error> {
        let method=serde_json::json!({"title":"Titan action","origin":target.as_str(),"submit_label":"Continue",
            "description":"Choose how to change this resource.","fields":[
                {"kind":"choice","label":"Action","value":"","options":["Upload text","Upload a file","Delete resource"]}]});
        let answer=self.prompt_with_form(owner.clone(),String::new(),Some(method.to_string()),context).await?;
        let mode=fields(&answer,1)?[0]["choice"].as_u64().filter(|value|*value<=2).ok_or(Error::Address)?;
        let mut submission=if mode==2 {
            // Deletion still permits a server token, and gets its own clearly
            // labelled final decision. Empty files/text never select it.
            let spec=serde_json::json!({"title":"Titan deletion","origin":target.as_str(),"submit_label":"Review",
                "description":"Review deletion before sending it to this server.","fields":[
                    {"kind":"text","label":"MIME type of resource","value":"text/gemini","options":[]},
                    {"kind":"password","label":"Server token (optional)","value":"","options":[]}]});
            let answer=self.prompt_with_form(owner.clone(),String::new(),Some(spec.to_string()),context).await?;
            let answer=fields(&answer,2)?;
            let mime=answer[0]["value"].as_str().ok_or(Error::Address)?.trim();
            Submission {payload:Payload::Delete,mime:if mime.is_empty(){"text/gemini"}else{mime}.into(),token:answer[1]["value"].as_str().ok_or(Error::Address)?.into(),description:"Delete this resource (zero bytes).".into()}
        }else {
            let mut description="Choose content and review it before sending.".to_owned();
            loop {
                let spec=serde_json::json!({"title":if mode==0 {"Titan text"}else{"Titan file"},"origin":target.as_str(),"submit_label":"Review","description":description,
                    "fields":[
                        {"kind":if mode==0 {"multiline"}else{"file"},"label":if mode==0 {"Text to upload"}else{"File to upload"},"value":"","options":[],"file_title":"Choose the file to upload"},
                        {"kind":"text","label":if mode==0 {"MIME type"}else{"MIME type (blank uses filename)"},"value":if mode==0 {"text/gemini"}else{""},"options":[]},
                        {"kind":"password","label":"Server token (optional)","value":"","options":[]}]});
                let answer=self.prompt_with_form(owner.clone(),String::new(),Some(spec.to_string()),context).await?;
                let answer=fields(&answer,3)?;
                let value=answer[0]["value"].as_str().ok_or(Error::Address)?;
                let mime=answer[1]["value"].as_str().ok_or(Error::Address)?.trim();
                let token=answer[2]["value"].as_str().ok_or(Error::Address)?.to_owned();
                let (payload,detail,default_mime)=if mode==0 {
                    if value.is_empty() {description="Text is empty. Use Delete resource to request deletion.".into();continue;}
                    if value.len()>super::super::MAX_BYTES {return Err(Error::TooLarge);}
                    (Payload::Text(value.as_bytes().to_vec()),"Text entered in this dialog".into(),"text/gemini".into())
                }else {
                    let path=std::path::Path::new(value);
                    let payload=match Payload::open(path).await {Ok(payload)=>payload,Err(_)=>{description="Choose a readable, non-empty regular file. Use Delete resource for deletion.".into();continue;}};
                    let mime=if path.extension().is_some_and(|extension|matches!(extension.to_str(),Some("gmi"|"gemini"))){"text/gemini".into()}else{mime_guess::from_path(path).first_or_octet_stream().to_string()};
                    (payload,format!("File: {}",path.display()),mime)
                };
                let mime=if mime.is_empty(){default_mime}else{mime.to_owned()};
                if request_bytes(target,payload.len(),&mime,&token).is_err() {description="Check the MIME type and token length before continuing.".into();continue;}
                break Submission {payload,mime,token,description:detail};
            }
        };
        // Validate deletion's token through the same formatter too.
        request_bytes(target,submission.payload.len(),&submission.mime,&submission.token)?;
        self.review_titan(&owner,target,&mut submission,false,context).await?;
        Ok(submission)
    }

    async fn review_titan(&self, owner: &str, target: &Url, submission: &mut Submission, retry: bool, context: &FetchContext) -> Result<(), Error> {
        submission.payload.unchanged().await?;
        let preview=format!("Resource: {target}\n{}\nBytes: {}\nMIME type: {}\nServer token: {}",submission.description,submission.payload.len(),submission.mime,if submission.token.is_empty(){"none"}else{"provided"});
        let spec=serde_json::json!({"title":if submission.deleting(){"Review Titan deletion"}else{"Review Titan upload"},"origin":gemini::authority(target),"submit_label":submission.button(),
            "description":if retry {"This is a new submission attempt. Confirm before sending again."}else if submission.deleting(){"Delete the displayed resource on this server."}else{"Send the selected content to this server."},
            "fields":[{"kind":"preview","label":"Submission","value":preview,"options":[]}]});
        self.prompt_with_form(owner.to_owned(),String::new(),Some(spec.to_string()),context).await?;
        submission.payload.unchanged().await
    }

    pub(in super::super) async fn retrieve_titan(&self, url: &mut Url, request: &mut Request, task: Option<PrivateTask>, context: &FetchContext) -> Result<Resource, Error> {
        if request.initiator==super::super::Initiator::Download {return Err(Error::Unsupported);}
        let owner=request.target_webview_id.as_ref().map(|id|format!("{id:?}")).ok_or(Error::Cancelled)?;
        let target=public_url(url)?;let changed=&target!=url;*url=target.clone();
        for claim in &mut request.url_list {
            let current=claim.url().into_url();
            if current.scheme()=="titan" {*claim=servo::protocol_handler::UrlWithBlobClaim::for_url(public_url(&current)?.into()).map_err(|_|Error::Address)?;}
        }
        let submission=match task {Some(PrivateTask::Titan(submission))=>submission,Some(PrivateTask::TitanNotice(error))=>return self.titan_stopped(&target,&owner,error,false,context),None=>None,_=>return Err(Error::Address)};
        if changed {return self.restart_private(request,target,PrivateTask::Titan(submission),context);}
        let Some(submission)=submission else {
            return self.titan_page(&target,&owner,"Titan submission","Upload content or delete this resource after reviewing the action.",true);
        };
        let mut attempted=false;
        match self.submit_titan(url,owner.clone(),submission,&mut attempted,context).await {
            Ok(resource)=>Ok(resource),
            Err(error)=>self.titan_stopped(url,&owner,error,attempted,context),
        }
    }

    async fn submit_titan(&self,url:&mut Url,owner:String,mut submission:Submission,attempted:&mut bool,context:&FetchContext)->Result<Resource,Error> {
        let mut wire_target=url.clone();let mut state=GeminiLoad::default();
        loop {
            submission.payload.unchanged().await?;
            let session=self.tls_session(&wire_target,&owner,&mut state,context).await?;
            let pin=session.certificate.pin.clone();let tls12=session.tls12();
            *attempted=true;
            let reply=upload(session,&wire_target,&mut submission,context).await?;
            match reply {
                gemini::Reply::Success(mime,bytes)=>return self.file_resource(url,&mime,bytes,owner,context,false).await,
                gemini::Reply::Stream(stream)=>return self.save_transfer(url,stream,"titan-response".into(),owner,context).await,
                gemini::Reply::Redirect(next)=>{
                    // A response redirect is an inert destination link. Even
                    // a Titan destination starts a fresh preparation flow.
                    let next=if next.scheme()=="titan" {public_url(&next)?}else{next};
                    let mut body="<h1>Titan response</h1><p>The server returned a destination.</p>".to_owned();
                    if let Some(link)=document::link_target(url,next.as_str()) {body.push_str(&format!("<p><a href=\"{}\" rel=noreferrer>Open response destination</a></p>",document::escape(link.as_str())));}
                    return Ok(Resource::Document(Content::html(document::page_with_metadata(url,&body,None,Some("Titan response")))));
                },
                gemini::Reply::Failure(status,message)=>{
                    return Ok(Resource::Document(Content::html(document::page_with_metadata(url,&format!("<h1>Titan {status}</h1><pre>{}</pre>",document::escape(&message)),None,Some("Titan response")))));
                },
                gemini::Reply::Identity(status,message)=>{
                    state.identity=Some(self.choose_tls_identity(owner.clone(),&wire_target,&pin,status,message,tls12,context).await?);
                    let mut display=wire_target.clone();if state.sensitive {display.set_query(None);}
                    self.review_titan(&owner,&display,&mut submission,true,context).await?;
                    *url=display;
                },
                gemini::Reply::Input {sensitive,prompt}=>{
                    let value=self.query_input(owner.clone(),&wire_target,&prompt,sensitive,context).await?;
                    wire_target.set_query(Some(&percent_encoding::utf8_percent_encode(&value,percent_encoding::NON_ALPHANUMERIC).to_string()));
                    state.sensitive|=sensitive;
                    request_bytes(&wire_target,submission.payload.len(),&submission.mime,&submission.token)?;
                    let mut display=wire_target.clone();if state.sensitive {display.set_query(None);}
                    self.review_titan(&owner,&display,&mut submission,true,context).await?;
                    *url=display;
                },
                gemini::Reply::ScrollSuccess {..}=>return Err(Error::Response),
            }
        }
    }
}
