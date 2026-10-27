// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit Titan submission. Navigation itself never sends an upload.
//! gemini://transjovian.org/titan/The%20Titan%20Specification
mod ui;
use super::{Error, gemini, normalize_address};
use std::{path::Path, sync::atomic::{AtomicU64, Ordering}, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt, BufReader};
use url::Url;

const IO_TIMEOUT: Duration = Duration::from_secs(20);
const CHUNK: usize = 64 * 1024;

/// Parameters in a supplied link are instructions from its source, not consent.
/// MIME parameter values may contain '/', so stripping only the last URI path
/// segment would leave part of a supplied request behind.
pub(super) fn public_url(url: &Url) -> Result<Url, Error> {
    if url.scheme() != "titan" { return Err(Error::Address); }
    let mut clean = url.clone();
    clean.set_username("").map_err(|_| Error::Address)?;
    clean.set_password(None).map_err(|_| Error::Address)?;
    let path = clean.path().split(';').next().unwrap_or("/").to_owned();
    clean.set_path(if path.is_empty() { "/" } else { &path });
    normalize_address(&mut clean)?;
    gemini::uri_request_bytes(&clean, "titan", 1022)?;
    Ok(clean)
}

pub(super) fn request_bytes(url: &Url, size: u64, mime: &str, token: &str) -> Result<Vec<u8>, Error> {
    let url = public_url(url)?;
    let mime: mime::Mime = mime.parse().map_err(|_| Error::Address)?;
    if token.chars().any(char::is_control) { return Err(Error::Address); }
    let encode = |text: &str| percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC).to_string();
    let mut wire = format!("{};size={size};mime={}", &url[..url::Position::AfterPath], encode(mime.as_ref()));
    if !token.is_empty() { wire.push_str(";token="); wire.push_str(&encode(token)); }
    if let Some(query) = url.query() { wire.push('?'); wire.push_str(query); }
    wire.push_str("\r\n");
    if wire.len() > 1024 || !wire.is_ascii() { return Err(Error::TooLarge); }
    Ok(wire.into_bytes())
}

enum Payload {
    Text(Vec<u8>),
    File { file: tokio::fs::File, metadata: std::fs::Metadata },
    Delete,
}
impl Payload {
    fn len(&self) -> u64 {
        match self { Self::Text(bytes) => bytes.len() as u64, Self::File {metadata, ..} => metadata.len(), Self::Delete => 0 }
    }
    async fn open(path: &Path) -> Result<Self, Error> {
        if !path.is_absolute() { return Err(Error::LocalFile); }
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::OpenOptionsExt;
            // Do not block opening a device or FIFO selected in a file dialog.
            // Deliberately selected symlinks to regular files remain usable.
            let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).map_err(|_| Error::LocalFile)?;
            let metadata = file.metadata().map_err(|_| Error::LocalFile)?;
            if !metadata.is_file() || metadata.len() == 0 { return Err(Error::LocalFile); }
            Ok(Self::File {file: tokio::fs::File::from_std(file), metadata})
        }).await.map_err(|_| Error::LocalFile)?
    }
    async fn unchanged(&self) -> Result<(), Error> {
        if let Self::File {file, metadata} = self {
            use std::os::unix::fs::MetadataExt;
            let now = file.metadata().await.map_err(|_| Error::LocalFile)?;
            if now.len()!=metadata.len() || now.mtime()!=metadata.mtime() || now.mtime_nsec()!=metadata.mtime_nsec() ||
                now.ctime()!=metadata.ctime() || now.ctime_nsec()!=metadata.ctime_nsec() { return Err(Error::LocalFile); }
        }
        Ok(())
    }
    async fn write(&mut self, writer: &mut (impl AsyncWrite + Unpin), sent: &AtomicU64) -> Result<(), Error> {
        self.unchanged().await?;
        let mut total = 0;
        match self {
            Self::Delete => {},
            Self::Text(bytes) => {
                for bytes in bytes.chunks(CHUNK) {
                    io(writer.write_all(bytes)).await?;
                    total += bytes.len() as u64; sent.store(total, Ordering::Release);
                }
            },
            Self::File {file, metadata} => {
                tokio::time::timeout(IO_TIMEOUT, file.rewind()).await.map_err(|_| Error::Timeout)?.map_err(|_| Error::LocalFile)?;
                let mut buffer = [0u8; CHUNK];
                while total < metadata.len() {
                    let count = (metadata.len()-total).min(CHUNK as u64) as usize;
                    let n = tokio::time::timeout(IO_TIMEOUT, file.read(&mut buffer[..count])).await.map_err(|_| Error::Timeout)?.map_err(|_| Error::LocalFile)?;
                    if n == 0 { return Err(Error::LocalFile); }
                    io(writer.write_all(&buffer[..n])).await?;
                    total += n as u64; sent.store(total, Ordering::Release);
                }
            },
        }
        io(writer.flush()).await?;
        self.unchanged().await
    }
}

async fn io<T>(future: impl std::future::Future<Output=std::io::Result<T>>) -> Result<T, Error> {
    tokio::time::timeout(IO_TIMEOUT, future).await.map_err(|_| Error::Timeout)?.map_err(|_| Error::Submission)
}

pub(super) struct Submission { payload: Payload, mime: String, token: String, description: String }
impl Submission {
    fn deleting(&self) -> bool { matches!(self.payload, Payload::Delete) }
    fn button(&self) -> &'static str { if self.deleting() {"Delete"} else {"Upload"} }
}

/// Watch for early server responses while writing. Every write has a stall
/// deadline; a large progressing transfer does not have a fixed total limit.
async fn upload_stream(stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static, target: &Url, submission: &mut Submission) -> Result<gemini::Reply, Error> {
    submission.payload.unchanged().await?;
    let request = request_bytes(target, submission.payload.len(), &submission.mime, &submission.token)?;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    io(writer.write_all(&request)).await?;
    let sent = AtomicU64::new(0);
    let reply = {
        let sending = submission.payload.write(&mut writer, &sent);
        let response = gemini::response_header(&mut reader, target);
        tokio::pin!(sending); tokio::pin!(response);
        tokio::select! {
            biased;
            result = &mut response => result?,
            result = &mut sending => {
                // If the peer stopped reading because it already rejected the
                // intent, preserve that response instead of a write error.
                match result {
                    Ok(()) => tokio::time::timeout(IO_TIMEOUT, &mut response).await.map_err(|_| Error::Timeout)??,
                    Err(error) => match tokio::time::timeout(Duration::from_secs(2), &mut response).await {
                        Ok(Ok(reply)) => reply,
                        _ => return Err(error),
                    },
                }
            },
        }
    };
    if matches!(reply, gemini::Reply::Success(..)) && sent.load(Ordering::Acquire) != submission.payload.len() { return Err(Error::Response); }
    if matches!(reply, gemini::Reply::Success(..)) { submission.payload.unchanged().await?; }
    if let gemini::Reply::Success(mime, _) = &reply {
        let kind: mime::Mime = mime.parse().map_err(|_| Error::Response)?;
        if kind.type_()!=mime::TEXT && kind.type_()!=mime::IMAGE && kind.essence_str()!="application/xhtml+xml" {
            return Ok(gemini::Reply::Stream(super::transfer::Transfer::new(mime.clone(),None,reader)
                .with_completion(async move {io(writer.shutdown()).await})));
        }
    }
    let result = tokio::time::timeout(IO_TIMEOUT, gemini::complete_response(reply, &mut reader)).await.map_err(|_| Error::Timeout)?;
    let _ = tokio::time::timeout(IO_TIMEOUT, writer.shutdown()).await;
    result
}

async fn upload(session: gemini::Session, target: &Url, submission: &mut Submission, context: &super::FetchContext) -> Result<gemini::Reply, Error> {
    let future = upload_stream(session.into_stream(), target, submission);
    tokio::pin!(future);
    loop {
        if context.is_cancelled() { return Err(Error::Cancelled); }
        tokio::select! {
            result = &mut future => return result,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supplied_parameters_are_discarded_and_only_the_resource_is_persisted() {
        let url=Url::parse("titan://user:pass@host/a%3Bb;token=old;mime=text/plain;size=99?name=one;two#part").unwrap();
        assert_eq!(public_url(&url).unwrap().as_str(),"titan://host/a%3Bb?name=one;two#part");
        assert_eq!(public_url(&Url::parse("titan://host").unwrap()).unwrap().as_str(),"titan://host/");
        for url in ["gemini://host/x","titan://host:0/x","titan://host:25/x"] {assert!(public_url(&Url::parse(url).unwrap()).is_err());}
    }
    #[test]
    fn intent_uses_byte_count_and_places_encoded_parameters_before_the_query() {
        let url=Url::parse("titan://host/Test;size=900;token=old?username=Alex#local").unwrap();
        assert_eq!(request_bytes(&url,3,"text/gemini","a;= &é").unwrap(),b"titan://host/Test;size=3;mime=text%2Fgemini;token=a%3B%3D%20%26%C3%A9?username=Alex\r\n");
        assert!(request_bytes(&url,1,"invalid","x").is_err());
        assert!(request_bytes(&url,1,"text/plain","x\r\n").is_err());
        assert!(request_bytes(&url,1,"text/plain",&"x".repeat(1024)).is_err());
    }
    #[tokio::test]
    async fn exact_text_and_explicit_zero_byte_delete_receive_normal_responses() {
        for bytes in [b"hello".to_vec(),"Café 世界".as_bytes().to_vec(),Vec::new()] {
            let (client,mut server)=tokio::io::duplex(128);
            let expected=bytes.clone();
            let peer=tokio::spawn(async move {
                let mut header=Vec::new();while !header.ends_with(b"\r\n") {header.push(server.read_u8().await.unwrap());}
                assert!(String::from_utf8(header).unwrap().contains(&format!(";size={};",expected.len())));
                let mut body=vec![0;expected.len()];server.read_exact(&mut body).await.unwrap();assert_eq!(body,expected);
                server.write_all(b"20 text/plain\r\nDone").await.unwrap();server.shutdown().await.unwrap();
            });
            let mut submission=Submission {payload:if bytes.is_empty(){Payload::Delete}else{Payload::Text(bytes)},mime:"text/plain".into(),token:String::new(),description:String::new()};
            assert!(matches!(upload_stream(client,&Url::parse("titan://host/file").unwrap(),&mut submission).await.unwrap(),gemini::Reply::Success(_,bytes) if bytes==b"Done"));peer.await.unwrap();
        }
    }
    #[tokio::test]
    async fn early_rejection_is_read_without_waiting_for_a_large_body() {
        let (client,mut server)=tokio::io::duplex(128);
        let peer=tokio::spawn(async move {
            let mut header=Vec::new();while !header.ends_with(b"\r\n") {header.push(server.read_u8().await.unwrap());}
            server.write_all(b"50 Intent declined\r\n").await.unwrap();server.shutdown().await.unwrap();
        });
        let mut submission=Submission {payload:Payload::Text(vec![b'x';1024*1024]),mime:"text/plain".into(),token:String::new(),description:String::new()};
        let reply=tokio::time::timeout(Duration::from_secs(3),upload_stream(client,&Url::parse("titan://host/file").unwrap(),&mut submission)).await.unwrap().unwrap();
        assert!(matches!(reply,gemini::Reply::Failure(50,_)));peer.await.unwrap();
    }
    #[tokio::test]
    async fn success_before_the_declared_body_is_sent_is_not_accepted() {
        let (client,mut server)=tokio::io::duplex(128);
        let peer=tokio::spawn(async move {
            let mut header=Vec::new();while !header.ends_with(b"\r\n") {header.push(server.read_u8().await.unwrap());}
            server.write_all(b"20 text/plain\r\nPremature").await.unwrap();server.shutdown().await.unwrap();
        });
        let mut submission=Submission {payload:Payload::Text(vec![b'x';1024*1024]),mime:"text/plain".into(),token:String::new(),description:String::new()};
        assert!(matches!(upload_stream(client,&Url::parse("titan://host/file").unwrap(),&mut submission).await,Err(Error::Response)));peer.await.unwrap();
    }
    #[tokio::test]
    async fn selected_files_must_be_regular_nonempty_and_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let directory=std::env::temp_dir().join(format!("titan-file-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();std::fs::set_permissions(&directory,std::fs::Permissions::from_mode(0o700)).unwrap();
        let source=directory.join("source");std::fs::write(&source,b"original").unwrap();
        let payload=Payload::open(&source).await.unwrap();assert_eq!(payload.len(),8);assert!(payload.unchanged().await.is_ok());
        std::fs::write(&source,b"changed-length").unwrap();assert!(payload.unchanged().await.is_err());
        assert!(Payload::open(&directory).await.is_err());
        std::fs::write(directory.join("empty"),b"").unwrap();assert!(Payload::open(&directory.join("empty")).await.is_err());
        drop(payload);std::fs::remove_dir_all(directory).unwrap();
    }
}
