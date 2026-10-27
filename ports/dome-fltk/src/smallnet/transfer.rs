// SPDX-License-Identifier: GPL-3.0-or-later
//! Incremental native file transfers. Display documents keep their separate bound.
use super::Error;
use std::{future::Future, path::{Path,PathBuf}, pin::Pin, time::Duration};
use servo::protocol_handler::{Data,DoneChannel,FetchContext,NetworkError,Request,ResourceFetchTiming,Response,ResponseBody};
use servo_base::generic_channel::GenericCallback;
use tokio::{io::{AsyncRead,AsyncReadExt,AsyncWriteExt},sync::{mpsc,oneshot}};

const CHUNK:usize=64*1024;
const IDLE:Duration=Duration::from_secs(30);
type Completion=Pin<Box<dyn Future<Output=Result<(),Error>>+Send>>;

pub(super) struct Transfer {
    pub mime:String,
    pub length:Option<u64>,
    reader:Box<dyn AsyncRead+Unpin+Send>,
    completion:Option<Completion>,
    destination:Option<PathBuf>,
}
enum Sink<'a> { Channel(&'a mpsc::UnboundedSender<Data>), File(&'a mut tokio::fs::File) }

async fn interrupt<T>(future:impl Future<Output=Result<T,Error>>,cancelled:&(impl Fn()->bool+Sync))->Result<T,Error> {
    tokio::pin!(future);
    let deadline=tokio::time::Instant::now()+IDLE;
    loop {
        if cancelled(){return Err(Error::Cancelled);}
        tokio::select! {
            value=&mut future=>return value,
            _=tokio::time::sleep_until(deadline)=>return Err(Error::Timeout),
            _=tokio::time::sleep(Duration::from_millis(50))=>{},
        }
    }
}
impl Transfer {
    pub(super) fn new(mime:String,length:Option<u64>,reader:impl AsyncRead+Unpin+Send+'static)->Self {
        Self {mime,length,reader:Box::new(reader),completion:None,destination:None}
    }
    pub(super) fn with_completion(mut self,future:impl Future<Output=Result<(),Error>>+Send+'static)->Self {
        self.completion=Some(Box::pin(future));self
    }
    pub(super) fn also_save_to(mut self,path:PathBuf)->Self {self.destination=Some(path);self}
    async fn pump(&mut self,sink:&mut Sink<'_>,cancelled:&(impl Fn()->bool+Sync))->Result<u64,Error> {
        let mut destination=match self.destination.take() {Some(path)=>Some(interrupt(Save::create(&path),cancelled).await?),None=>None};
        let mut bytes=vec![0;CHUNK];let mut received=0u64;
        loop {
            let count=interrupt(async {self.reader.read(&mut bytes).await.map_err(|_|Error::Transport)},cancelled).await?;
            if count==0{break;}
            received=received.checked_add(count as u64).ok_or(Error::TooLarge)?;
            if self.length.is_some_and(|length|received>length){return Err(Error::Response);}
            if let Some(target)=&mut destination {
                interrupt(async {target.file.write_all(&bytes[..count]).await.map_err(|_|Error::Transport)},cancelled).await?;
            }
            match sink {
                Sink::Channel(sender)=>{
                    let (reply,receive)=oneshot::channel();let mut reply=Some(reply);
                    let ack=GenericCallback::new(move |_| {if let Some(reply)=reply.take(){let _=reply.send(());}}).map_err(|_|Error::Transport)?;
                    sender.send(Data::PayloadWithAck(bytes[..count].to_vec().into(),ack)).map_err(|_|Error::Cancelled)?;
                    // The acknowledgement travels to the actual file consumer, not
                    // merely to an intermediate network or IPC queue.
                    interrupt(async {receive.await.map_err(|_|Error::Cancelled)},cancelled).await?;
                },
                Sink::File(file)=>interrupt(async {file.write_all(&bytes[..count]).await.map_err(|_|Error::Transport)},cancelled).await?,
            }
        }
        // Tokio's file writer can report a write failure at its next flush.
        // Observe that result before accepting protocol completion or saving.
        if let Sink::File(file)=sink {
            interrupt(async {file.flush().await.map_err(|_|Error::Transport)},cancelled).await?;
        }
        if self.length.is_some_and(|length|received!=length){return Err(Error::Response);}
        if let Some(completion)=self.completion.take(){interrupt(completion,cancelled).await?;}
        if let Some(target)=&mut destination {
            interrupt(async {target.file.flush().await.map_err(|_|Error::Transport)},cancelled).await?;
            target.commit(cancelled).await?;
        }
        Ok(received)
    }
    pub(super) fn response(mut self,url:url::Url,request:&Request,done:&mut DoneChannel,context:&FetchContext)->Response {
        let mut response=Response::new(url.into(),ResourceFetchTiming::new(request.timing_type()));
        response.headers.insert("content-type",self.mime.parse().expect("Locally selected MIME type"));
        if let Some(length)=self.length{response.headers.insert("content-length",length.to_string().parse().unwrap());}
        response.headers.insert("referrer-policy","no-referrer".parse().unwrap());
        response.headers.insert("x-content-type-options","nosniff".parse().unwrap());
        *response.body.lock()=ResponseBody::Receiving(Vec::new());
        let body=response.body.clone();let context=context.clone();
        let (sender,receiver)=mpsc::unbounded_channel();*done=Some((sender.clone(),receiver));
        tokio::spawn(async move {
            let result=self.pump(&mut Sink::Channel(&sender),&||context.is_cancelled()).await;
            *body.lock()=ResponseBody::Done(Vec::new());
            let terminal=match result {
                Ok(_)=>Data::Done,
                Err(Error::Cancelled)=>Data::Error(NetworkError::LoadCancelled),
                Err(error)=>Data::Error(NetworkError::ResourceLoadError(error.message().into())),
            };
            let _=sender.send(terminal);
        });
        response
    }
    pub(super) async fn save(mut self,path:&Path,context:&FetchContext)->Result<(),Error> {
        let mut target=Save::create(path).await?;
        self.pump(&mut Sink::File(&mut target.file),&||context.is_cancelled()).await?;
        target.commit(&||context.is_cancelled()).await
    }
}
struct Save {file:tokio::fs::File,temporary:PathBuf,target:PathBuf,committed:bool}
impl Save {
    async fn commit(&mut self,cancelled:&(impl Fn()->bool+Sync))->Result<(),Error> {
        interrupt(async {self.file.sync_all().await.map_err(|_|Error::Transport)},cancelled).await?;
        if cancelled(){return Err(Error::Cancelled);}
        tokio::fs::rename(&self.temporary,&self.target).await.map_err(|_|Error::Transport)?;
        self.committed=true;Ok(())
    }
    async fn create(path:&Path)->Result<Self,Error> {
        if !path.is_absolute(){return Err(Error::Address);}
        let target=path.to_owned();let parent=target.parent().ok_or(Error::Address)?;
        let temporary=parent.join(format!(".dome-smallnet-{}",uuid::Uuid::new_v4()));
        tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::OpenOptionsExt;
            let file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary).map_err(|_|Error::Transport)?;
            Ok(Self {file:tokio::fs::File::from_std(file),temporary,target,committed:false})
        }).await.map_err(|_|Error::Transport)?
    }
}
impl Drop for Save {fn drop(&mut self){if !self.committed{let _=std::fs::remove_file(&self.temporary);}}}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc,atomic::{AtomicBool,Ordering}};
    #[tokio::test(flavor="current_thread")]
    async fn nntp_source_streams_beyond_preview_size_and_requires_the_dot_terminator() {
        use tokio::{net::TcpListener,io::{AsyncBufReadExt,BufReader}};
        use super::super::news;
        for complete in [true,false] {
            let expected=[b"Subject: Large fixture\r\nMessage-ID: <large@fixture>\r\n\r\n.dot\r\n".as_slice(),&vec![b'x';super::super::MAX_BYTES+17],b"\r\n"].concat();let payload=expected.clone();
            let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
            let server=tokio::spawn(async move {
                let (socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);socket.write_all(b"201 Fixture\r\n").await.unwrap();
                let mut line=String::new();socket.read_line(&mut line).await.unwrap();assert_eq!(line,"CAPABILITIES\r\n");socket.write_all(b"101 Caps\r\nVERSION 2\r\nREADER\r\n.\r\n").await.unwrap();
                line.clear();socket.read_line(&mut line).await.unwrap();assert_eq!(line,"ARTICLE <large@fixture>\r\n");socket.write_all(b"220 0 <large@fixture> Article\r\n").await.unwrap();
                // Only the line-start dot is stuffed; the delivered message
                // must retain one dot and all of its original CRLF bytes.
                let prefix=payload.windows(6).position(|p|p==b".dot\r\n").unwrap();socket.write_all(&payload[..prefix]).await.unwrap();socket.write_all(b".").await.unwrap();socket.write_all(&payload[prefix..]).await.unwrap();
                if complete {socket.write_all(b".\r\n").await.unwrap();line.clear();socket.read_line(&mut line).await.unwrap();assert_eq!(line,"QUIT\r\n");socket.write_all(b"205 Bye\r\n").await.unwrap();}
            });
            let target=news::Target::parse(&url::Url::parse(&format!("news://127.0.0.1:{port}/large@fixture")).unwrap()).unwrap();let endpoint=news::Server::for_url(&target.url).unwrap();
            let news::Reply::Stream(mut stream)=news::open(&target,&endpoint,true).await.unwrap() else {panic!("Expected source stream")};
            let path=std::env::temp_dir().join(format!("dome-news-stream-{}",uuid::Uuid::new_v4()));let mut destination=Save::create(&path).await.unwrap();
            let result=tokio::time::timeout(Duration::from_secs(10),stream.pump(&mut Sink::File(&mut destination.file),&||false)).await.unwrap();
            if complete {assert_eq!(result,Ok(expected.len() as u64));assert_eq!(std::fs::read(&destination.temporary).unwrap(),expected);}else{assert_eq!(result,Err(Error::Transport));}
            let temporary=destination.temporary.clone();drop(destination);assert!(!path.exists());assert!(!temporary.exists());server.await.unwrap();
        }
    }
    struct TlsShutdown { called:Arc<AtomicBool>, fail:bool }
    impl tokio::io::AsyncWrite for TlsShutdown {
        fn poll_write(self:Pin<&mut Self>,_:&mut std::task::Context<'_>,bytes:&[u8])->std::task::Poll<std::io::Result<usize>> {std::task::Poll::Ready(Ok(bytes.len()))}
        fn poll_flush(self:Pin<&mut Self>,_:&mut std::task::Context<'_>)->std::task::Poll<std::io::Result<()>> {std::task::Poll::Ready(Ok(()))}
        fn poll_shutdown(self:Pin<&mut Self>,_:&mut std::task::Context<'_>)->std::task::Poll<std::io::Result<()>> {
            self.called.store(true,Ordering::Release);
            std::task::Poll::Ready(if self.fail {Err(std::io::Error::other("Fixture TLS shutdown failure"))}else{Ok(())})
        }
    }
    fn tls_wire(scheme:&str,mime:&str,body:&[u8])->Vec<u8> {
        let header=if scheme=="scroll" {format!("24 {mime}\r\nFixture author\r\nFixture abstract\r\n\r\n")}else{format!("20 {mime}\r\n")};
        [header.as_bytes(),body].concat()
    }
    #[tokio::test(flavor="current_thread")]
    async fn gopher_files_exceed_preview_limit_with_framing_and_keep_open_peers() {
        use tokio::{net::TcpListener,io::{AsyncBufReadExt,BufReader}};
        for mode in ["count","dot","eof","basic"] {
            let expected=[b"..Line\r\nLong unbroken line ".as_slice(),&vec![b'x';super::super::MAX_BYTES+17],b"\r\n"].concat();let payload=expected.clone();
            let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
            let server=tokio::spawn(async move {
                let (socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);let mut request=String::new();socket.read_line(&mut request).await.unwrap();
                assert_eq!(request,if mode=="basic" {"file.bin\r\n"}else{"file.bin\t+\r\n"});
                let wire=match mode {
                    "count"=>[format!("+{}\r\n",payload.len()).as_bytes(),&payload].concat(),
                    "dot"=>[b"+-1\r\n.".as_slice(),&payload,b".\r\n"].concat(),
                    "eof"=>[b"+-2\r\n".as_slice(),&payload].concat(),
                    _=>payload,
                };
                socket.write_all(&wire).await.unwrap();
                if matches!(mode,"count"|"dot"){assert!(socket.read_u8().await.is_err());}
            });
            let url=url::Url::parse(&format!("{}://127.0.0.1:{port}/9file.bin",if mode=="basic"{"gopher"}else{"gopher+"})).unwrap();
            let request=super::super::gopher::Request::parse(&url).unwrap();
            let opened=super::super::gopher::open(&url,&request,None,true).await.unwrap();
            let super::super::gopher::Opened::Stream(mut stream)=opened else {panic!("Expected streamed Gopher file")};
            let path=std::env::temp_dir().join(format!("dome-gopher-large-{}",uuid::Uuid::new_v4()));let mut target=Save::create(&path).await.unwrap();
            assert_eq!(tokio::time::timeout(Duration::from_secs(5),stream.pump(&mut Sink::File(&mut target.file),&||false)).await.unwrap(),Ok(expected.len()as u64));
            assert_eq!(std::fs::read(&target.temporary).unwrap(),expected);drop(stream);drop(target);server.await.unwrap();
        }
    }
    #[tokio::test(flavor="current_thread")]
    async fn additional_form_destination_receives_the_original_stream_and_commits_only_on_completion() {
        let directory=std::env::temp_dir().join(format!("dome-transfer-copy-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&directory).unwrap();
        let path=directory.join("chosen.bin");let expected=vec![71;CHUNK*3+11];
        for failure in [false,true] {
            std::fs::write(&path,b"Existing chosen destination").unwrap();
            let mut transfer=Transfer::new("application/octet-stream".into(),None,std::io::Cursor::new(expected.clone())).also_save_to(path.clone())
                .with_completion(async move {if failure{Err(Error::Response)}else{Ok(())}});
            let (sender,mut receiver)=mpsc::unbounded_channel();
            let task=tokio::spawn(async move {transfer.pump(&mut Sink::Channel(&sender),&||false).await});
            let mut received=Vec::new();
            while let Some(Data::PayloadWithAck(bytes,ack))=receiver.recv().await {
                received.extend(bytes);assert_eq!(std::fs::read(&path).unwrap(),b"Existing chosen destination");ack.send(()).unwrap();
            }
            assert_eq!(received,expected);
            assert_eq!(task.await.unwrap(),if failure{Err(Error::Response)}else{Ok(expected.len()as u64)});
            assert_eq!(std::fs::read(&path).unwrap(),if failure{b"Existing chosen destination".as_slice()}else{expected.as_slice()});
            assert_eq!(std::fs::read_dir(&directory).unwrap().count(),1);
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[tokio::test(flavor="current_thread")]
    async fn tls_files_stream_prefetched_bytes_beyond_preview_limit_and_finish_after_consumption() {
        for scheme in ["gemini","scroll"] {
            let expected=[b"\0TLS header/body boundary\r\n.\r\n\xff".as_slice(),&vec![53;super::super::MAX_BYTES+17]].concat();
            let reader=tokio::io::BufReader::new(std::io::Cursor::new(tls_wire(scheme,"application/octet-stream",&expected)));
            let called=Arc::new(AtomicBool::new(false));let writer=TlsShutdown {called:called.clone(),fail:false};
            let url=url::Url::parse(&format!("{scheme}://localhost/file.bin")).unwrap();
            let reply=super::super::gemini::receive(reader,writer,&url,false,false).await.unwrap();
            let super::super::gemini::Reply::Stream(mut stream)=reply else {panic!("Expected a streamed TLS response")};
            assert!(!called.load(Ordering::Acquire));assert_eq!(stream.mime,"application/octet-stream");
            let path=std::env::temp_dir().join(format!("dome-tls-stream-{}",uuid::Uuid::new_v4()));let mut target=Save::create(&path).await.unwrap();
            assert_eq!(stream.pump(&mut Sink::File(&mut target.file),&||false).await,Ok(expected.len() as u64));
            assert_eq!(std::fs::read(&target.temporary).unwrap(),expected);assert!(called.load(Ordering::Acquire));drop(target);assert!(!path.exists());
        }
    }
    #[tokio::test(flavor="current_thread")]
    async fn tls_text_downloads_stream_while_document_and_scroll_abstract_previews_stay_bounded() {
        let body=vec![b'x';super::super::MAX_BYTES+1];
        for (scheme,mime,metadata) in [("gemini","text/gemini",false),("scroll","text/scroll",false),("scroll","application/octet-stream",true)] {
            let url=url::Url::parse(&format!("{scheme}://localhost/large")).unwrap();
            let reader=tokio::io::BufReader::new(std::io::Cursor::new(tls_wire(scheme,mime,&body)));
            assert!(matches!(super::super::gemini::receive(reader,tokio::io::sink(),&url,metadata,false).await,Err(Error::TooLarge)));
            let reader=tokio::io::BufReader::new(std::io::Cursor::new(tls_wire(scheme,mime,&body)));
            let reply=super::super::gemini::receive(reader,tokio::io::sink(),&url,metadata,true).await.unwrap();
            let super::super::gemini::Reply::Stream(mut stream)=reply else {panic!("Explicit downloads must stream")};
            let path=std::env::temp_dir().join(format!("dome-tls-text-{}",uuid::Uuid::new_v4()));let mut target=Save::create(&path).await.unwrap();
            assert_eq!(stream.pump(&mut Sink::File(&mut target.file),&||false).await,Ok(body.len() as u64));
            assert_eq!(std::fs::read(&target.temporary).unwrap(),body);drop(target);
        }
    }
    #[tokio::test(flavor="current_thread")]
    async fn tls_shutdown_failure_does_not_accept_a_finished_save() {
        for scheme in ["gemini","scroll"] {
            let reader=tokio::io::BufReader::new(std::io::Cursor::new(tls_wire(scheme,"application/octet-stream",b"Complete body")));
            let called=Arc::new(AtomicBool::new(false));let writer=TlsShutdown {called:called.clone(),fail:true};
            let url=url::Url::parse(&format!("{scheme}://localhost/file.bin")).unwrap();
            let reply=super::super::gemini::receive(reader,writer,&url,false,true).await.unwrap();
            let super::super::gemini::Reply::Stream(mut stream)=reply else {panic!("Expected a stream")};
            let path=std::env::temp_dir().join(format!("dome-tls-failed-{}",uuid::Uuid::new_v4()));let mut target=Save::create(&path).await.unwrap();let temporary=target.temporary.clone();
            assert_eq!(stream.pump(&mut Sink::File(&mut target.file),&||false).await,Err(Error::Transport));assert!(called.load(Ordering::Acquire));
            drop(target);assert!(!path.exists());assert!(!temporary.exists());
        }
    }
    #[tokio::test(flavor="current_thread")]
    async fn streamed_chunks_wait_for_the_consumer_and_completion() {
        let data=vec![37;CHUNK*3+7];let expected=data.clone();let completed=Arc::new(AtomicBool::new(false));let flag=completed.clone();
        let mut transfer=Transfer::new("application/octet-stream".into(),Some(data.len() as u64),std::io::Cursor::new(data))
            .with_completion(async move {flag.store(true,Ordering::Release);Ok(())});
        let (sender,mut receiver)=mpsc::unbounded_channel();
        let task=tokio::spawn(async move {transfer.pump(&mut Sink::Channel(&sender),&||false).await});
        let mut received=Vec::new();
        while let Some(Data::PayloadWithAck(bytes,ack))=receiver.recv().await {
            assert!(bytes.len()<=CHUNK);received.extend(bytes);
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(!completed.load(Ordering::Acquire));assert!(receiver.try_recv().is_err());assert!(!task.is_finished());
            ack.send(()).unwrap();
        }
        assert_eq!(task.await.unwrap(),Ok(expected.len() as u64));assert_eq!(received,expected);assert!(completed.load(Ordering::Acquire));
    }
    #[tokio::test(flavor="current_thread")]
    async fn spartan_stream_preserves_prefetched_body_and_exceeds_preview_limit() {
        use tokio::{net::TcpListener,io::{AsyncBufReadExt,BufReader}};
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
        let expected=[b"\0Header/body boundary\r\n.\r\n\xff".as_slice(),&vec![41;super::super::MAX_BYTES+17]].concat();let payload=expected.clone();
        let server=tokio::spawn(async move {
            let (socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);let mut request=String::new();socket.read_line(&mut request).await.unwrap();
            assert_eq!(request,"127.0.0.1 /file.bin 0\r\n");
            // One write lets the buffered header reader prefetch body bytes.
            let wire=[b"2 application/octet-stream\r\n".as_slice(),&payload].concat();socket.get_mut().write_all(&wire).await.unwrap();
        });
        let url=url::Url::parse(&format!("spartan://127.0.0.1:{port}/file.bin")).unwrap();
        let reply=tokio::time::timeout(Duration::from_secs(2),super::super::spartan::open(&url,true)).await.unwrap().unwrap();
        let super::super::spartan::Reply::Stream(mut stream)=reply else {panic!("Expected a streamed response")};
        assert_eq!(stream.mime,"application/octet-stream");
        let path=std::env::temp_dir().join(format!("dome-spartan-stream-{}",uuid::Uuid::new_v4()));let mut target=Save::create(&path).await.unwrap();
        assert_eq!(stream.pump(&mut Sink::File(&mut target.file),&||false).await,Ok(expected.len() as u64));
        assert_eq!(std::fs::read(&target.temporary).unwrap(),expected);drop(target);assert!(!path.exists());server.await.unwrap();
    }
    #[tokio::test(flavor="current_thread")]
    async fn plain_stream_does_not_inherit_the_document_preview_limit() {
        use tokio::{net::TcpListener,io::{AsyncBufReadExt,BufReader}};
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
        let payload=vec![93;super::super::MAX_BYTES+1];let expected=payload.clone();
        let server=tokio::spawn(async move {
            for _ in 0..2 {
                let (socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);let mut request=String::new();socket.read_line(&mut request).await.unwrap();
                assert_eq!(request,"/large.txt\r\n");let _=socket.get_mut().write_all(&payload).await;
            }
        });
        let url=url::Url::parse(&format!("nex://127.0.0.1:{port}/large.txt")).unwrap();
        assert_eq!(super::super::exchange(&url,1900,b"/large.txt".to_vec()).await,Err(Error::TooLarge));
        let reader=super::super::open_exchange(&url,1900,b"/large.txt".to_vec()).await.unwrap();let mut stream=Transfer::new("text/plain".into(),None,reader);
        let path=std::env::temp_dir().join(format!("dome-plain-stream-{}",uuid::Uuid::new_v4()));let mut target=Save::create(&path).await.unwrap();
        assert_eq!(stream.pump(&mut Sink::File(&mut target.file),&||false).await,Ok(expected.len() as u64));assert_eq!(std::fs::read(&target.temporary).unwrap(),expected);
        drop(target);assert!(!path.exists());server.await.unwrap();
    }
    #[tokio::test(flavor="current_thread")]
    async fn cancellation_releases_an_unacknowledged_transfer() {
        let mut transfer=Transfer::new("application/octet-stream".into(),None,std::io::Cursor::new(vec![9;CHUNK*4]));
        let (sender,mut receiver)=mpsc::unbounded_channel();let cancelled=Arc::new(AtomicBool::new(false));let flag=cancelled.clone();
        let task=tokio::spawn(async move {transfer.pump(&mut Sink::Channel(&sender),&||flag.load(Ordering::Acquire)).await});
        let first=receiver.recv().await.unwrap();assert!(matches!(first,Data::PayloadWithAck(..)));
        cancelled.store(true,Ordering::Release);
        assert_eq!(tokio::time::timeout(Duration::from_secs(1),task).await.unwrap().unwrap(),Err(Error::Cancelled));
        assert!(receiver.try_recv().is_err());drop(first);
    }
    #[tokio::test(flavor="current_thread")]
    async fn failed_or_incomplete_saves_preserve_the_destination_and_remove_temporary_files() {
        let directory=std::env::temp_dir().join(format!("dome-transfer-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&directory).unwrap();
        let path=directory.join("file.bin");std::fs::write(&path,b"Existing destination").unwrap();
        let mut target=Save::create(&path).await.unwrap();
        let mut transfer=Transfer::new("application/octet-stream".into(),Some(5),std::io::Cursor::new(vec![1;4]));
        assert_eq!(transfer.pump(&mut Sink::File(&mut target.file),&||false).await,Err(Error::Response));drop(target);
        assert_eq!(std::fs::read(&path).unwrap(),b"Existing destination");assert_eq!(std::fs::read_dir(&directory).unwrap().count(),1);
        let mut target=Save::create(&path).await.unwrap();target.file=tokio::fs::File::open(&path).await.unwrap();
        let mut transfer=Transfer::new("application/octet-stream".into(),None,std::io::Cursor::new(vec![1;4]));
        assert_eq!(transfer.pump(&mut Sink::File(&mut target.file),&||false).await,Err(Error::Transport));drop(target);
        assert_eq!(std::fs::read(&path).unwrap(),b"Existing destination");assert_eq!(std::fs::read_dir(&directory).unwrap().count(),1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
