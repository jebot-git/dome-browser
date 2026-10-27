/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::{future::{Future,ready},pin::Pin,sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::body::Incoming;
use net::fetch::methods::{DoneChannel,FetchContext};
use net::protocols::{ProtocolHandler,ProtocolRegistry};
use net_traits::{CoreResourceMsg,FetchChannels,FetchResponseMsg,NetworkError,ResourceFetchTiming,ResourceTimingType,fetch_async};
use net_traits::blob_url_store::UrlWithBlobClaim;
use net_traits::request::{Referrer,Request,RequestBuilder,RequestMode};
use net_traits::response::{ProtocolRedirect,Response,ResponseBody};
use servo_base::generic_channel::{self,GenericCallback};
use servo_url::ServoUrl;
use crate::{fetch_with_context,make_body,make_server,mock_origin,new_fetch_context};

#[test]
fn native_view_state_is_internal_and_survives_request_serialization_and_response_filtering() {
    let url=ServoUrl::parse("scroll://localhost/resource?lookup#2").unwrap();
    let mut builder=RequestBuilder::new(None,UrlWithBlobClaim::for_url(url.clone()).unwrap(),Referrer::NoReferrer);
    builder.protocol_state=Some("abstract".into());
    let mut serialized=serde_json::to_value(&builder).unwrap();
    let restored:RequestBuilder=serde_json::from_value(serialized.clone()).unwrap();
    assert_eq!(restored.build().protocol_state.as_deref(),Some("abstract"));
    serialized.as_object_mut().unwrap().remove("protocol_state");
    let legacy:RequestBuilder=serde_json::from_value(serialized).unwrap();assert!(legacy.build().protocol_state.is_none());
    let request=builder.build();assert_eq!(request.clone().protocol_state.as_deref(),Some("abstract"));
    assert!(!request.headers.contains_key("protocol-state"));
    let mut response=Response::new(url,ResourceFetchTiming::new(ResourceTimingType::Navigation));
    response.headers.insert("protocol-state","abstract".parse().unwrap());
    assert!(response.metadata().unwrap().metadata().protocol_state.is_none());
    response.protocol_state=Some("abstract".into());
    for kind in [net_traits::response::ResponseType::Basic,net_traits::response::ResponseType::Cors] {
        let metadata:net_traits::Metadata=response.clone().to_filtered(kind).metadata().unwrap().into();
        assert_eq!(metadata.protocol_state.as_deref(),Some("abstract"));
    }
}

#[test]
fn native_stream_acknowledgement_waits_for_the_real_fetch_consumer() {
    let (core,receiver)=generic_channel::channel().unwrap();
    let (entered,enter)=crossbeam_channel::bounded(1);let (release,resume)=crossbeam_channel::bounded(1);
    let (finished,finish)=crossbeam_channel::bounded(1);
    let url=ServoUrl::parse("ftp://localhost/file.bin").unwrap();
    let builder=RequestBuilder::new(None,UrlWithBlobClaim::for_url(url).unwrap(),Referrer::NoReferrer).origin(mock_origin());let id=builder.id;
    fetch_async(&core,builder,None,Box::new(move |message|match message {
        FetchResponseMsg::ProcessResponseChunk(_,bytes)=>{
            assert_eq!(bytes.as_ref(),b"native streamed bytes");entered.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(10)).unwrap();
        },
        FetchResponseMsg::ProcessResponseEOF(..)=>{finished.send(()).unwrap();},
        _=>{},
    }));
    let CoreResourceMsg::Fetch(_,FetchChannels::ResponseMsg(callback))=receiver.try_recv_timeout(Duration::from_secs(10)).ok().expect("Expected resource-thread message") else {panic!("Expected native fetch callback")};
    let (acknowledged,ack)=crossbeam_channel::bounded(1);
    let acknowledgement=GenericCallback::new(move |_|{let _=acknowledged.send(());}).unwrap();
    callback.send(FetchResponseMsg::ProcessResponseChunkWithAck(id,Bytes::from_static(b"native streamed bytes"),acknowledgement)).unwrap();
    enter.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(ack.recv_timeout(Duration::from_millis(40)).is_err());
    release.send(()).unwrap();ack.recv_timeout(Duration::from_secs(10)).unwrap();
    callback.send(FetchResponseMsg::ProcessResponseEOF(id,Ok(()),ResourceFetchTiming::new(ResourceTimingType::Resource))).unwrap();
    finish.recv_timeout(Duration::from_secs(10)).unwrap();
}

struct CleanupCount(Arc<AtomicUsize>);
impl Drop for CleanupCount {fn drop(&mut self){self.0.fetch_add(1,Ordering::SeqCst);}}
fn cleanup_callback(count:Arc<AtomicUsize>)->GenericCallback<()> {
    let mut guard=Some(CleanupCount(count));
    GenericCallback::new(move |_|drop(guard.take())).unwrap()
}
struct NativeRedirect {url:ServoUrl,repeat:bool,released:Arc<AtomicUsize>}
impl ProtocolHandler for NativeRedirect {
    fn load<'a>(&'a self,request:&'a mut Request,_done:&'a mut DoneChannel,_context:&FetchContext)->Pin<Box<dyn Future<Output=Response>+Send+'a>> {
        let mut response=Response::new(request.current_url(),ResourceFetchTiming::new(request.timing_type()));
        response.custom_redirect=Some(ProtocolRedirect {url:if self.repeat{request.current_url()}else{self.url.clone()},count_redirect:true,finished:Some(cleanup_callback(self.released.clone()))});
        Box::pin(ready(response))
    }
}
#[test]
fn native_redirects_preserve_response_delivery_and_stop_at_the_limit() {
    let handler=move |_:http::Request<Incoming>,response:&mut http::Response<BoxBody<Bytes,hyper::Error>>| {
        *response.body_mut()=make_body(b"Native redirected response".to_vec());
    };
    let (server,url)=make_server(handler);
    for repeat in [false,true] {
        let mut context=new_fetch_context(None,None);let mut protocols=ProtocolRegistry::with_internal_protocols();
        let released=Arc::new(AtomicUsize::new(0));
        protocols.register("native-transfer-test",NativeRedirect {url:url.url().clone(),repeat,released:released.clone()}).unwrap();
        context.protocols=Arc::new(protocols);
        let source=ServoUrl::parse("native-transfer-test://localhost/source").unwrap();
        let request=RequestBuilder::new(None,UrlWithBlobClaim::for_url(source).unwrap(),Referrer::NoReferrer).origin(mock_origin()).mode(RequestMode::Navigate).build();
        let (sender,receiver)=std::sync::mpsc::channel();
        std::thread::spawn(move ||{let response=fetch_with_context(request,&mut context);let _=sender.send(response);});
        let response=receiver.recv_timeout(Duration::from_secs(10)).expect("Native redirect must deliver its terminal response");
        if repeat {assert_eq!(response.get_network_error(),Some(&NetworkError::TooManyRedirects));}
        else {
            assert!(!response.is_network_error());assert_eq!(response.url(),Some(&url.url()));
            assert_eq!(*response.actual_response().body.lock(),ResponseBody::Done(b"Native redirected response".to_vec()));
        }
        assert_eq!(released.load(Ordering::SeqCst),if repeat{11}else{1});
    }
    let _=server.close();
}

struct NativeInteraction {stage:AtomicUsize,released:Arc<AtomicUsize>,fail:bool}
impl ProtocolHandler for NativeInteraction {
    fn load<'a>(&'a self,request:&'a mut Request,done:&'a mut DoneChannel,_context:&FetchContext)->Pin<Box<dyn Future<Output=Response>+Send+'a>> {
        Box::pin(async move {
            assert_eq!(request.redirect_count,10,"Interaction must not consume a redirect");
            let mut response=Response::new(request.current_url(),ResourceFetchTiming::new(request.timing_type()));
            let stage=self.stage.fetch_add(1,Ordering::SeqCst);
            if stage<3 {
                let mut url=request.current_url().into_url();url.set_path(&format!("/retry-{}",stage+1));
                response.custom_redirect=Some(ProtocolRedirect {url:url.into(),count_redirect:false,finished:Some(cleanup_callback(self.released.clone()))});
            } else if self.fail {
                return Response::network_error(NetworkError::ResourceLoadError("Fixture terminal error".into()));
            } else {
                assert_eq!(stage,3);assert_eq!(self.released.load(Ordering::SeqCst),0);
                let (sender,receiver)=tokio::sync::mpsc::unbounded_channel();*done=Some((sender.clone(),receiver));
                *response.body.lock()=ResponseBody::Receiving(Vec::new());let body=response.body.clone();
                response.headers.insert("content-type","application/octet-stream".parse().unwrap());
                tokio::spawn(async move {
                    let (signal,wait)=tokio::sync::oneshot::channel();let mut signal=Some(signal);
                    let ack=GenericCallback::new(move |_|{if let Some(signal)=signal.take(){let _=signal.send(());}}).unwrap();
                    sender.send(net::fetch::methods::Data::PayloadWithAck(Bytes::from_static(b"\0native retried stream\r\n.\xff"),ack)).unwrap();
                    tokio::time::timeout(Duration::from_secs(5),wait).await.unwrap().unwrap();
                    *body.lock()=ResponseBody::Done(Vec::new());sender.send(net::fetch::methods::Data::Done).unwrap();
                });
            }
            response
        })
    }
}

#[test]
fn native_interaction_retries_keep_the_original_stream_consumer_and_release_private_state() {
    for fail in [false,true] {
        let released=Arc::new(AtomicUsize::new(0));let mut context=new_fetch_context(None,None);
        let mut protocols=ProtocolRegistry::with_internal_protocols();
        protocols.register("native-interaction-test",NativeInteraction {stage:AtomicUsize::new(0),released:released.clone(),fail}).unwrap();context.protocols=Arc::new(protocols);
        let source=ServoUrl::parse("native-interaction-test://localhost/source").unwrap();
        let mut request=RequestBuilder::new(None,UrlWithBlobClaim::for_url(source).unwrap(),Referrer::NoReferrer).origin(mock_origin()).mode(RequestMode::Navigate).build();request.redirect_count=10;
        let id=request.id;let bytes=Arc::new(std::sync::Mutex::new(Vec::new()));let chunks=bytes.clone();let (finished,finish)=crossbeam_channel::bounded(1);
        let mut target=GenericCallback::new(move |message|match message.unwrap() {
            FetchResponseMsg::ProcessResponseChunkWithAck(actual,chunk,ack)=>{assert_eq!(id,actual);chunks.lock().unwrap().extend_from_slice(&chunk);ack.send(()).unwrap();},
            FetchResponseMsg::ProcessResponseEOF(actual,result,_)=>{assert_eq!(id,actual);finished.send(result).unwrap();},
            _=>{},
        }).unwrap();
        net::async_runtime::spawn_blocking_task(async move {net::fetch::methods::fetch(request,&mut target,&mut context).await;});
        let result=finish.recv_timeout(Duration::from_secs(10)).expect("Retry must deliver a terminal response to the original consumer");
        assert_eq!(result.is_err(),fail);
        assert_eq!(bytes.lock().unwrap().as_slice(),if fail{b"".as_slice()}else{b"\0native retried stream\r\n.\xff".as_slice()});
        assert_eq!(released.load(Ordering::SeqCst),3);
    }
    let released=Arc::new(AtomicUsize::new(0));drop(cleanup_callback(released.clone()));
    assert_eq!(released.load(Ordering::SeqCst),1,"Abandoning an in-process continuation releases its state");
}
