// SPDX-License-Identifier: GPL-3.0-or-later
//! Gopher URI mapping (RFC 4266) and the Gopher+ transfer protocol.
//! `gopher+` is a convenience alias using the original memo's request layout.
use super::{Error, MAX_BYTES, MAX_REQUEST, decoded};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, percent_encode};
use std::{future::Future,pin::Pin,task::{Context,Poll}};
use tokio::{io::{AsyncBufRead,AsyncBufReadExt,AsyncRead,AsyncReadExt,AsyncWrite,AsyncWriteExt,BufReader,ReadBuf},net::TcpStream};
use url::Url;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Command {
    Default,
    View { mime: String, language: Option<String> },
    Attributes(String),
    Ask,
}

impl Command {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.iter().any(|byte| *byte < 32 || *byte == 127) { return Err(Error::Address); }
        let text = std::str::from_utf8(bytes).map_err(|_| Error::Address)?;
        match text {
            "+" => Ok(Self::Default),
            "?" => Ok(Self::Ask),
            _ if text.starts_with(['!', '$']) => {
                if text[1..].bytes().any(|b| !(b.is_ascii_alphanumeric() || b"+ -_".contains(&b))) {
                    return Err(Error::Address);
                }
                Ok(Self::Attributes(text.into()))
            }
            _ if text.starts_with('+') => {
                let mut fields = text[1..].split_ascii_whitespace();
                let mime = fields.next().ok_or(Error::Address)?;
                let mime: mime::Mime = mime.parse().map_err(|_| Error::Address)?;
                let language = fields.next();
                if fields.next().is_some() || language.is_some_and(|s| s.bytes().any(|b| !(b.is_ascii_alphanumeric() || b"_-".contains(&b)))) {
                    return Err(Error::Address);
                }
                Ok(Self::View { mime: mime.to_string(), language: language.map(str::to_owned) })
            }
            _ => Err(Error::Address),
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Default => "+".into(),
            Self::Ask => "?".into(),
            Self::Attributes(text) => text.clone(),
            Self::View { mime, language } => format!("+{mime}{}", language.as_ref().map(|s| format!(" {s}")).unwrap_or_default()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Request {
    pub kind: u8,
    selector: Vec<u8>,
    pub search: Option<Vec<u8>>,
    pub command: Option<Command>,
    pub supplied: Option<Vec<u8>>,
    memo_layout: bool,
}

impl Request {
    pub(super) fn filename(&self)->String {
        String::from_utf8_lossy(&self.selector).rsplit(['/', '\\']).next().unwrap_or("").to_owned()
    }

    pub(super) fn parse(url: &Url) -> Result<Self, Error> {
        if !matches!(url.scheme(),"gopher"|"gopher+"|"gophers") || url.as_str().len() > MAX_REQUEST { return Err(Error::Address); }
        let path: Vec<u8> = percent_decode_str(url.path().strip_prefix('/').unwrap_or(url.path())).collect();
        let (kind, path) = path.split_first().map(|(kind, rest)| (*kind, rest)).unwrap_or((b'1', &[]));
        if !kind.is_ascii_graphic() { return Err(Error::Address); }
        let fields: Vec<&[u8]> = path.splitn(3, |byte| *byte == b'\t').collect();
        if fields.iter().take(2).any(|field| field.iter().any(|b| *b < 32 || *b == 127)) {
            return Err(Error::Address);
        }
        let memo_layout = url.scheme() == "gopher+";
        let mut search = fields.get(1).map(|value| value.to_vec());
        if let Some(query) = url.query() {
            if search.is_some() || kind != b'7' { return Err(Error::Address); }
            search = Some(decoded(query)?);
        }
        if kind != b'7' && search.as_ref().is_some_and(|s| !s.is_empty()) { return Err(Error::Address); }
        let (command_bytes, supplied) = match fields.get(2) {
            Some(value) => match value.iter().position(|b| *b == b'\t') {
                Some(index) => {
                    let data = value[index+1..].strip_prefix(b"1\r\n").ok_or(Error::Address)?;
                    (Some(&value[..index]), Some(uri_data(data)?))
                }
                None => (Some(*value), None),
            },
            None => (None, None),
        };
        let command = command_bytes.map(Command::parse).transpose()?
            .or(if memo_layout { Some(Command::Default) } else { None });
        if supplied.is_some() && !matches!(command, Some(Command::Default | Command::View { .. })) { return Err(Error::Address); }
        Ok(Self { kind, selector: fields[0].to_vec(), search, command, supplied, memo_layout })
    }

    pub(super) fn with_command(&self, command: Command) -> Self {
        let mut result = self.clone();
        result.command = Some(command);
        result.supplied = None;
        result
    }

    pub(super) fn clean_url(&self, base: &Url) -> Result<Url, Error> {
        self.with_command(Command::Ask).url(base)
    }

    pub(super) fn without_supplied(&self) -> Self {
        let mut result = self.clone(); result.supplied = None; result
    }

    pub(super) fn needs_search(&self) -> bool {
        self.kind == b'7' && self.search.is_none() && !matches!(self.command, Some(Command::Attributes(_)) | Some(Command::Ask))
    }

    pub(super) fn with_search(&self, value: &str, url: &Url) -> Result<Url, Error> {
        let value = decoded(&percent_encoding::utf8_percent_encode(value, NON_ALPHANUMERIC).to_string())?;
        let mut request = self.clone();
        request.search = Some(value);
        request.url(url)
    }

    fn url(&self, base: &Url) -> Result<Url, Error> {
        let mut url = base.clone();
        url.set_query(None);
        let mut path = format!("/{}{}", self.kind as char, percent_encode(&self.selector, NON_ALPHANUMERIC));
        if self.search.is_some() || self.command.is_some() {
            path.push_str("%09");
            path.push_str(&percent_encode(self.search.as_deref().unwrap_or_default(), NON_ALPHANUMERIC).to_string());
        }
        if let Some(command) = &self.command {
            path.push_str("%09");
            path.push_str(&percent_encode(command.text().as_bytes(), NON_ALPHANUMERIC).to_string());
        }
        url.set_path(&path);
        if url.as_str().len() > MAX_REQUEST { return Err(Error::Address); }
        Ok(url)
    }

    fn wire(&self) -> Result<Vec<u8>, Error> {
        // A question mark is a client instruction to obtain ASK attributes;
        // it must never be sent as an arbitrary server command.
        if self.supplied.is_some() || matches!(self.command, Some(Command::Ask)) { return Err(Error::Unsupported); }
        let mut bytes = self.selector.clone();
        if self.kind == b'7' && self.search.is_some() || !self.memo_layout && (self.search.is_some() || self.command.is_some()) {
            bytes.push(b'\t');
            bytes.extend_from_slice(self.search.as_deref().unwrap_or_default());
        }
        if let Some(command) = &self.command {
            bytes.push(b'\t');
            bytes.extend_from_slice(command.text().as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
        if bytes.len() > MAX_REQUEST { return Err(Error::Address); }
        Ok(bytes)
    }
}

/// Decode only the data-block grammar, never a raw request or local filename.
fn uri_data(data: &[u8]) -> Result<Vec<u8>, Error> {
    let end = data.windows(2).position(|w| w == b"\r\n").ok_or(Error::Address)?;
    let size = data[..end].strip_prefix(b"+").ok_or(Error::Address)?;
    let body = &data[end+2..];
    match size {
        b"-2" => Ok(body.to_vec()),
        b"-1" => {
            let mut result = Vec::new(); let mut rest = body;
            loop {
                let end = rest.windows(2).position(|w| w == b"\r\n").ok_or(Error::Address)?;
                let line = &rest[..end]; rest = &rest[end+2..];
                if line == b"." { if !rest.is_empty() { return Err(Error::Address); } return Ok(result); }
                result.extend_from_slice(if line.starts_with(b"..") { &line[1..] } else { line });
                result.extend_from_slice(b"\r\n");
            }
        }
        _ => {
            if size.is_empty() || !size.iter().all(u8::is_ascii_digit) { return Err(Error::Address); }
            let count: usize = std::str::from_utf8(size).map_err(|_| Error::Address)?.parse().map_err(|_| Error::TooLarge)?;
            if count != body.len() || count > MAX_BYTES { return Err(Error::Address); }
            Ok(body.to_vec())
        }
    }
}

pub(super) fn command_url(url: &Url, command: Command) -> Option<Url> {
    Request::parse(url).ok()?.with_command(command).url(url).ok()
}

/// Convert descriptor-based Gopher+ references to RFC 4266 layout on TLS.
pub(super) fn secure_url(url: &Url) -> Result<Url, Error> {
    let request=Request::parse(url)?;
    if request.supplied.is_some() {return Err(Error::Address);}
    let mut base=url.clone();base.set_scheme("gophers").map_err(|_|Error::Address)?;
    request.url(&base)
}

type Completion=Pin<Box<dyn Future<Output=Result<(),Error>>+Send>>;
struct Connected {reader:BufReader<Box<dyn AsyncRead+Unpin+Send>>,completion:Option<Completion>}
async fn completed<T>(result:Result<T,Error>,completion:Option<Completion>)->Result<T,Error> {
    let value=result?;
    if let Some(finish)=completion {finish.await?;}
    Ok(value)
}

#[derive(Debug, PartialEq)]
pub(super) struct Reply { pub failed: bool, pub bytes: Vec<u8> }

pub(super) enum Opened { Buffered(Reply), Stream(super::transfer::Transfer) }

#[derive(Clone,Copy)]
enum Dot { Start, Leading, Carriage, EmitCarriage, FinalCarriage, Body, BodyCarriage, Done }
enum Frame { Eof, Count(u64), Dot {state:Dot,strict:bool} }
struct Framed<R> {inner:R,frame:Frame}
/// Shared strict dot framing for NNTP article source streams.
pub(super) struct DotReader<R>(Framed<R>);
impl<R> DotReader<R> {
    pub(super) fn new(inner:R)->Self {Self(Framed {inner,frame:Frame::Dot {state:Dot::Start,strict:true}})}
    pub(super) fn into_inner(self)->R {self.0.inner}
}
impl<R:AsyncBufRead+Unpin> AsyncRead for DotReader<R> {
    fn poll_read(self:Pin<&mut Self>,cx:&mut Context<'_>,output:&mut ReadBuf<'_>)->Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx,output)
    }
}
impl<R:AsyncBufRead+Unpin> AsyncRead for Framed<R> {
    fn poll_read(self:Pin<&mut Self>,cx:&mut Context<'_>,output:&mut ReadBuf<'_>)->Poll<std::io::Result<()>> {
        let this=self.get_mut();
        if output.remaining()==0{return Poll::Ready(Ok(()));}
        match &mut this.frame {
            Frame::Eof=>Pin::new(&mut this.inner).poll_read(cx,output),
            Frame::Count(remaining)=>{
                if *remaining==0{return Poll::Ready(Ok(()));}
                let limit=(*remaining).min(output.remaining() as u64)as usize;
                let mut part=ReadBuf::new(&mut output.initialize_unfilled()[..limit]);
                match Pin::new(&mut this.inner).poll_read(cx,&mut part) {
                    Poll::Ready(Ok(()))=>{
                        let count=part.filled().len();
                        if count==0{return Poll::Ready(Err(std::io::ErrorKind::UnexpectedEof.into()));}
                        *remaining-=count as u64;output.advance(count);Poll::Ready(Ok(()))
                    },
                    other=>other,
                }
            },
            Frame::Dot {state,strict}=>{
                let start=output.filled().len();
                loop {
                    match *state {
                        Dot::Done=>return Poll::Ready(Ok(())),
                        Dot::EmitCarriage|Dot::FinalCarriage=>{
                            output.put_slice(b"\r");*state=if matches!(*state,Dot::FinalCarriage){Dot::Done}else{Dot::BodyCarriage};
                            if output.remaining()==0{return Poll::Ready(Ok(()));}continue;
                        },
                        _=>{},
                    }
                    let bytes=match Pin::new(&mut this.inner).poll_fill_buf(cx) {
                        Poll::Pending=>return if output.filled().len()>start {Poll::Ready(Ok(()))}else{Poll::Pending},
                        Poll::Ready(Err(error))=>return Poll::Ready(Err(error)),
                        Poll::Ready(Ok(bytes))=>bytes,
                    };
                    if bytes.is_empty() {
                        if *strict{return Poll::Ready(Err(std::io::ErrorKind::UnexpectedEof.into()));}
                        // Basic Gopher historically also accepts peers that
                        // delimit text by EOF. Keep that compatibility, while
                        // Gopher+'s explicit dot framing requires its terminator.
                        match *state {
                            Dot::Leading=>{output.put_slice(b".");*state=Dot::Done;},
                            Dot::Carriage=>{output.put_slice(b".");*state=Dot::FinalCarriage;},
                            _=>*state=Dot::Done,
                        }
                        if output.remaining()==0{return Poll::Ready(Ok(()));}continue;
                    }
                    let byte=bytes[0];let mut consume=true;
                    match *state {
                        Dot::Start if byte==b'.'=>*state=Dot::Leading,
                        Dot::Leading if byte==b'\r'=>*state=Dot::Carriage,
                        Dot::Leading if byte==b'\n' && !*strict=>*state=Dot::Done,
                        Dot::Leading if byte==b'.'=>{output.put_slice(b".");*state=Dot::Body;},
                        Dot::Leading=>{output.put_slice(b".");*state=Dot::Body;consume=false;},
                        Dot::Carriage if byte==b'\n'=>*state=Dot::Done,
                        Dot::Carriage=>{output.put_slice(b".");*state=Dot::EmitCarriage;consume=false;},
                        _=>{
                            output.put_slice(&[byte]);
                            *state=if byte==b'\n' && (!*strict || matches!(*state,Dot::BodyCarriage)){Dot::Start}
                                else if byte==b'\r'{Dot::BodyCarriage}else{Dot::Body};
                        },
                    }
                    if consume{Pin::new(&mut this.inner).consume(1);}
                    if output.remaining()==0{return Poll::Ready(Ok(()));}
                }
            },
        }
    }
}

/// File transfers own their buffered/framed reader. Preview and metadata
/// retrieval still use the bounded fetch path below.
pub(super) async fn open(url:&Url,request:&Request,body:Option<&[u8]>,download:bool)->Result<Opened,Error> {
    open_inner(url,request,body,download,None).await
}

pub(super) async fn open_tls(url:&Url,request:&Request,body:Option<&[u8]>,download:bool,session:super::gemini::Session)->Result<Opened,Error> {
    open_inner(url,request,body,download,Some(session)).await
}

async fn open_inner(url:&Url,request:&Request,body:Option<&[u8]>,download:bool,session:Option<super::gemini::Session>)->Result<Opened,Error> {
    let mut request=request.clone();
    if let Some(body)=body {
        if request.command.is_none() || body.len()>MAX_BYTES{return Err(Error::Address);}
        if request.kind==b'7' && request.search.is_none(){request.search=Some(Vec::new());}
    }
    let mime=if let Some(Command::View {mime,..})=&request.command {mime.clone()}
        else {match request.kind {b'0'=>"text/plain",b'1'|b'7'=>"application/gopher-menu",b'h'=>"text/html",b'g'=>"image/gif",_=>"application/octet-stream"}.into()};
    let kind:mime::Mime=mime.parse().map_err(|_|Error::Response)?;
    let preview=kind.type_()==mime::TEXT || kind.type_()==mime::IMAGE || kind.essence_str()=="application/xhtml+xml" || kind.essence_str()=="application/gopher-menu" || (request.kind==b'I' && !matches!(request.command,Some(Command::View {..})));
    if matches!(request.command,Some(Command::Attributes(_))) || (!download && preview) {
        return fetch_inner(url,&request,body,session).await.map(Opened::Buffered);
    }
    let Connected {reader:mut stream,completion}=connect_request(url,&request,body,session).await?;
    let (frame,length)=if request.command.is_some() {
        let header=line(&mut stream,MAX_REQUEST).await?;
        if header.len()<4 || !matches!(header[0],b'+'|b'-'){return Err(Error::Response);}
        if header[0]==b'-' {
            // Error descriptions are bounded documents, never successful files.
            let mut original=std::io::Cursor::new(header).chain(stream);
            return completed(response(&mut original,false).await,completion).await.map(Opened::Buffered);
        }
        let size=&header[1..header.len()-2];
        match size {
            b"-1"=>(Frame::Dot {state:Dot::Start,strict:true},None),
            b"-2"=>(Frame::Eof,None),
            _=>{
                if size.is_empty() || !size.iter().all(u8::is_ascii_digit){return Err(Error::Response);}
                let count:u64=std::str::from_utf8(size).map_err(|_|Error::Response)?.parse().map_err(|_|Error::TooLarge)?;
                (Frame::Count(count),Some(count))
            },
        }
    } else if matches!(request.kind,b'0'|b'1'|b'7') {(Frame::Dot {state:Dot::Start,strict:false},None)}
        else {(Frame::Eof,None)};
    let mut reader=BufReader::new(Framed {inner:stream,frame});
    let mime=if request.kind==b'I' && !matches!(request.command,Some(Command::View {..})) {
        super::image_mime(url,reader.fill_buf().await.map_err(|_|Error::Transport)?)
    }else{mime};
    let mut transfer=super::transfer::Transfer::new(mime,length,reader);
    if let Some(completion)=completion {transfer=transfer.with_completion(completion);}
    Ok(Opened::Stream(transfer))
}

async fn line(stream: &mut (impl AsyncRead + Unpin), limit: usize) -> Result<Vec<u8>, Error> {
    let mut line = Vec::new();
    while line.len() < limit {
        line.push(stream.read_u8().await.map_err(|_| Error::Response)?);
        if line.ends_with(b"\r\n") { return Ok(line); }
    }
    Err(Error::TooLarge)
}

async fn until_eof(stream: impl AsyncRead + Unpin) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    stream.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes).await.map_err(|_| Error::Transport)?;
    if bytes.len() > MAX_BYTES { return Err(Error::TooLarge); }
    Ok(bytes)
}

async fn dot_block(stream: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    loop {
        let next = line(stream, MAX_BYTES.saturating_sub(bytes.len()) + 3).await?;
        if next == b".\r\n" { return Ok(bytes); }
        let next = if next.starts_with(b"..") { &next[1..] } else { &next };
        if bytes.len() + next.len() > MAX_BYTES { return Err(Error::TooLarge); }
        bytes.extend_from_slice(next);
    }
}

async fn response(stream: &mut (impl AsyncRead + Unpin), attributes: bool) -> Result<Reply, Error> {
    let header = line(stream, MAX_REQUEST).await?;
    // Original attribute replies are also found without a length prefix.
    // Only attribute retrieval accepts that grammar, never data transfers.
    if attributes && header.starts_with(b"+") && header.contains(&b':') {
        let mut bytes = header;
        // Older attribute servers use either EOF or a terminating dot. Stop
        // at the dot even when the peer keeps its connection open.
        let mut start = bytes.len();
        loop {
            match stream.read_u8().await {
                Ok(byte) => bytes.push(byte),
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(_) => return Err(Error::Transport),
            }
            if bytes.len() > MAX_BYTES { return Err(Error::TooLarge); }
            if bytes.ends_with(b"\r\n") {
                if &bytes[start..] == b".\r\n" { bytes.truncate(start); break; }
                start = bytes.len();
            }
        }
        return Ok(Reply { failed: false, bytes });
    }
    if header.len() < 4 || !matches!(header[0], b'+' | b'-') { return Err(Error::Response); }
    let size = &header[1..header.len()-2];
    let bytes = match size {
        b"-1" => dot_block(stream).await?,
        b"-2" => until_eof(stream).await?,
        _ => {
            if size.is_empty() || !size.iter().all(u8::is_ascii_digit) { return Err(Error::Response); }
            let count: usize = std::str::from_utf8(size).map_err(|_| Error::Response)?.parse().map_err(|_| Error::TooLarge)?;
            if count > MAX_BYTES { return Err(Error::TooLarge); }
            let mut bytes = vec![0; count];
            stream.read_exact(&mut bytes).await.map_err(|_| Error::Response)?;
            bytes
        }
    };
    Ok(Reply { failed: header[0] == b'-', bytes })
}

#[cfg(test)]
pub(super) async fn fetch(url: &Url, request: &Request) -> Result<Reply, Error> {
    fetch_inner(url, request, None, None).await
}

#[cfg(test)]
pub(super) async fn submit(url: &Url, request: &Request, body: &[u8]) -> Result<Reply, Error> {
    if request.command.is_none() || body.len() > MAX_BYTES { return Err(Error::Address); }
    let mut request = request.clone();
    // Index requests retain a search field even when an ASK form supplies all
    // input in its data block. Attribute retrieval itself has no such field.
    if request.kind == b'7' && request.search.is_none() { request.search = Some(Vec::new()); }
    fetch_inner(url, &request, Some(body), None).await
}

// Keep raw dot stuffing for the existing menu/text presentation layer, but
// recognize its terminator without waiting for the server to close. A TLS
// transport error remains an error; only an actual clean EOF is lenient.
async fn plain_block(stream:&mut(impl AsyncBufRead+Unpin))->Result<Vec<u8>,Error> {
    let mut stream=stream.take((MAX_BYTES+1)as u64);let mut bytes=Vec::new();
    loop {
        let start=bytes.len();
        let n=stream.read_until(b'\n',&mut bytes).await.map_err(|_|Error::Transport)?;
        if bytes.len()>MAX_BYTES {return Err(Error::TooLarge);}
        if n==0 || matches!(&bytes[start..],b".\r\n"|b".\n") {return Ok(bytes);}
    }
}

async fn fetch_inner(url:&Url,request:&Request,body:Option<&[u8]>,session:Option<super::gemini::Session>)->Result<Reply,Error> {
    let Connected {reader:mut stream,completion}=connect_request(url,request,body,session).await?;
    let result=if request.command.is_some() {
        response(&mut stream,matches!(request.command,Some(Command::Attributes(_)))).await
    } else {
        let bytes=if matches!(request.kind,b'0'|b'1'|b'7') {plain_block(&mut stream).await} else {until_eof(stream).await};
        bytes.map(|bytes|Reply {failed:false,bytes})
    };
    completed(result,completion).await
}

async fn write_request(stream:&mut(impl AsyncWrite+Unpin),wire:&[u8],body:Option<&[u8]>)->Result<(),Error> {
    stream.write_all(wire).await.map_err(|_|Error::Transport)?;
    if let Some(body)=body {
        stream.write_all(format!("+{}\r\n",body.len()).as_bytes()).await.map_err(|_|Error::Transport)?;
        stream.write_all(body).await.map_err(|_|Error::Transport)?;
    }
    Ok(())
}

async fn connect_request(url:&Url,request:&Request,body:Option<&[u8]>,session:Option<super::gemini::Session>)->Result<Connected,Error> {
    let mut wire=request.wire()?;
    if body.is_some() {
        wire.truncate(wire.len()-2);wire.extend_from_slice(b"\t1\r\n");
        if wire.len()>MAX_REQUEST {return Err(Error::Address);}
    }
    match (url.scheme(),session) {
        ("gophers",Some(session))=>{
            let mut stream=session.into_stream();write_request(&mut stream,&wire,body).await?;
            let (reader,mut writer)=tokio::io::split(stream);
            Ok(Connected {reader:BufReader::new(Box::new(reader)),completion:Some(Box::pin(async move {writer.shutdown().await.map_err(|_|Error::Transport)}))})
        },
        ("gopher"|"gopher+",None)=>{
            let host=url.host_str().ok_or(Error::Address)?.trim_matches(['[',']']);
            let mut stream=TcpStream::connect((host,url.port().unwrap_or(70))).await.map_err(|_|Error::Transport)?;
            write_request(&mut stream,&wire,body).await?;
            Ok(Connected {reader:BufReader::new(Box::new(stream)),completion:None})
        },
        _=>Err(Error::Address),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(text: &str) -> Request { Request::parse(&Url::parse(text).unwrap()).unwrap() }

    #[test]
    fn tls_alias_preserves_rfc_commands_queries_and_fragments() {
        for (source,expected) in [
            ("gopher+://host:70/0file#section","gophers://host:70/0file%09%09%2B#section"),
            ("gopher://host/7find?two%20words#result","gophers://host/7find%09two%20words#result"),
            ("gopher+://host/0form%09%09%3F","gophers://host/0form%09%09%3F"),
        ] {assert_eq!(secure_url(&Url::parse(source).unwrap()).unwrap().as_str(),expected);}
        assert!(secure_url(&Url::parse("https://host/file").unwrap()).is_err());
        assert_eq!(parse("gophers://host/0file%09%09%2B").wire().unwrap(),b"file\t\t+\r\n");
    }

    #[tokio::test(flavor="current_thread")]
    async fn plain_preview_stops_on_the_dot_and_keeps_raw_stuffing() {
        for wire in [b"one\r\n..two\r\n.\r\nignored".as_slice(),b"one\n..two\n.\nignored"] {
            let expected=&wire[..wire.len()-7];
            let mut reader=BufReader::with_capacity(2,wire);
            assert_eq!(plain_block(&mut reader).await.unwrap(),expected);
        }
        assert_eq!(plain_block(&mut BufReader::new(b"legacy EOF".as_slice())).await.unwrap(),b"legacy EOF");
        // A caller cannot accidentally open a TLS URI through the plain socket path.
        let url=Url::parse("gophers://127.0.0.1:70/0file").unwrap();
        assert!(matches!(open(&url,&Request::parse(&url).unwrap(),None,false).await,Err(Error::Address)));
    }

    #[tokio::test(flavor="current_thread")]
    async fn streamed_dot_framing_handles_every_small_input_and_output_boundary() {
        for capacity in 1..=17 {
            for size in 1..=17 {
                for (wire,expected,strict) in [
                    (b"one\r\n..two\r\n...three\r\nbare\n..literal\r\n.x\rY\r\n.\r\nignored".as_slice(),b"one\r\n.two\r\n..three\r\nbare\n..literal\r\n.x\rY\r\n".as_slice(),true),
                    (b"plain\n..dot\n.\nignored",b"plain\n.dot\n",false),
                    (b"unfinished\n.\r",b"unfinished\n.\r",false),
                ] {
                    let mut reader=Framed {inner:BufReader::with_capacity(capacity,wire),frame:Frame::Dot {state:Dot::Start,strict}};
                    let mut actual=Vec::new();let mut chunk=vec![0;size];
                    loop {let count=reader.read(&mut chunk).await.unwrap();if count==0{break;}actual.extend_from_slice(&chunk[..count]);}
                    assert_eq!(actual,expected,"input={capacity}, output={size}");
                }
            }
        }
    }
    #[tokio::test(flavor="current_thread")]
    async fn counted_and_explicit_dot_streams_reject_incomplete_bodies() {
        for (wire,frame) in [(b"short".as_slice(),Frame::Count(6)),(b"unclosed\r\n",Frame::Dot {state:Dot::Start,strict:true}),(b".\r",Frame::Dot {state:Dot::Start,strict:true})] {
            let mut reader=Framed {inner:BufReader::with_capacity(1,wire),frame};
            let error=reader.read_to_end(&mut Vec::new()).await.unwrap_err();assert_eq!(error.kind(),std::io::ErrorKind::UnexpectedEof);
        }
        let mut reader=Framed {inner:BufReader::new(b"binary\0\r\n.\r\ntrailing".as_slice()),frame:Frame::Count(13)};
        let mut bytes=Vec::new();reader.read_to_end(&mut bytes).await.unwrap();assert_eq!(bytes,b"binary\0\r\n.\r\nt");
    }

    #[test]
    fn rfc_uri_and_memo_alias_preserve_their_request_layouts() {
        for (url, expected) in [
            ("gopher://host/", b"\r\n".as_slice()),
            ("gopher://host/0/a%20b%25c", b"/a b%c\r\n"),
            ("gopher://host/0/%FF", b"/\xff\r\n"),
            ("gopher://host/7find%09two%20words", b"find\ttwo words\r\n"),
            ("gopher://host/7find?two%20words", b"find\ttwo words\r\n"),
            ("gopher://host/0file%09%09%2B", b"file\t\t+\r\n"),
            ("gopher+://host/0file", b"file\t+\r\n"),
            ("gopher+://host/7find%09words%09%2B", b"find\twords\t+\r\n"),
            ("gopher://host/0file%09%09!%2BABSTRACT", b"file\t\t!+ABSTRACT\r\n"),
            ("gopher+://host/0file%09%09!%2BABSTRACT", b"file\t!+ABSTRACT\r\n"),
            ("gopher+://host/0file%09%09%2Btext%2Fplain%20De_DE", b"file\t+text/plain De_DE\r\n"),
        ] { assert_eq!(parse(url).wire().unwrap(), expected, "{url}"); }
    }

    #[test]
    fn search_prompt_roundtrips_without_losing_plus_command() {
        let url = Url::parse("gopher+://host/7find").unwrap();
        let request = Request::parse(&url).unwrap();
        assert!(request.needs_search());
        let next = request.with_search("世界 & +", &url).unwrap();
        let next = Request::parse(&next).unwrap();
        assert!(!next.needs_search());
        assert_eq!(next.wire().unwrap(), "find\t世界 & +\t+\r\n".as_bytes());
        assert!(!parse("gopher://host/7find%09%09!").needs_search());
    }

    #[test]
    fn only_structured_commands_and_fields_are_accepted() {
        for url in ["gopher://host/0file%09words", "gopher://host/0file%09%09raw", "gopher://host/0file%09%09+%091", "gopher://host/0file%00", "gopher://host/7find%09words?again", "gopher://host/0file%09%09+text/plain%0D%0A"] {
            assert!(Request::parse(&Url::parse(url).unwrap()).is_err(), "{url}");
        }
        assert_eq!(parse("gopher://host/0form%09%09%3F").wire(), Err(Error::Unsupported));
    }

    #[test]
    fn filled_form_uris_decode_data_without_turning_it_into_a_request_line() {
        for (encoded, expected) in [
            ("%2B%091%0D%0A%2B%2D1%0D%0Aone%0D%0A%2E%2Etwo%0D%0A%2E%0D%0A", b"one\r\n.two\r\n".as_slice()),
            ("%2B%091%0D%0A%2B3%0D%0Aa%00b", b"a\0b"),
            ("%2B%091%0D%0A%2B%2D2%0D%0Adata", b"data"),
        ] {
            let url = Url::parse(&format!("gopher://host/0form%09%09{encoded}")).unwrap();
            let request = Request::parse(&url).unwrap();
            assert_eq!(request.supplied.as_deref(), Some(expected));
            assert_eq!(request.wire(), Err(Error::Unsupported));
            let clean = request.clean_url(&url).unwrap();
            assert_eq!(clean.as_str(), "gopher://host/0form%09%09%3F");
            assert!(Request::parse(&clean).unwrap().supplied.is_none());
            assert_eq!(request.without_supplied().wire().unwrap(), b"form\t\t+\r\n");
        }
        for data in [b"+-1\r\nnot closed\r\n".as_slice(), b"+-1\r\n.\r\nextra", b"+5\r\nshorter", b"--1\r\n.\r\n", b"+x\r\n"] { assert!(uri_data(data).is_err()); }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn framing_preserves_binary_data_and_dot_rules() {
        for (wire, expected, failed) in [
            (b"+6\r\na\0.\r\nbignored".as_slice(), b"a\0.\r\nb".as_slice(), false),
            (b"+-1\r\none\r\n..two\r\n.\r\nignored", b"one\r\n.two\r\n", false),
            (b"+-2\r\na\r\n.\r\nb\0", b"a\r\n.\r\nb\0", false),
            (b"--1\r\n2 <admin@example.com>\r\nLater\r\n.\r\n", b"2 <admin@example.com>\r\nLater\r\n", true),
            (b"+0\r\n", b"", false),
        ] {
            assert_eq!(response(&mut wire.as_ref(), false).await.unwrap(), Reply { failed, bytes: expected.to_vec() });
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_or_truncated_frames_are_not_successful_documents() {
        for wire in [b"+4\r\na".as_slice(), b"+-1\r\nunclosed\r\n", b"+x\r\n", b"+-3\r\n", b"+ 4\r\n", b"999\r\n", b"+8388609\r\n", b"+INFO: 0file\r\n"] {
            assert!(response(&mut wire.as_ref(), false).await.is_err());
        }
        let raw = b"+INFO: 0file\tfile\thost\t70\t+\r\n+ABSTRACT:\r\n Description\r\n.\r\n";
        assert_eq!(response(&mut raw.as_slice(), true).await.unwrap().bytes, &raw[..raw.len()-3]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn counted_and_dot_framed_transfers_finish_before_server_close() {
        for response in [b"+5\r\nHello".as_slice(), b"+-1\r\nHello\r\n.\r\n"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let response = response.to_vec();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                assert_eq!(line(&mut stream, MAX_REQUEST).await.unwrap(), b"file\t+\r\n");
                for fragment in response.chunks(2) {
                    stream.write_all(fragment).await.unwrap();
                    tokio::task::yield_now().await;
                }
                // Wait for the client's EOF, rather than closing to delimit.
                assert!(stream.read_u8().await.is_err());
            });
            let url = Url::parse(&format!("gopher+://127.0.0.1:{port}/0file")).unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), fetch(&url, &Request::parse(&url).unwrap())).await.unwrap().unwrap();
            assert!(result.bytes.starts_with(b"Hello"));
            server.await.unwrap();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_form_submission_uses_counted_binary_body_and_original_target() {
      for kind in ['0', '7'] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            assert_eq!(line(&mut stream, MAX_REQUEST).await.unwrap(), if kind == '7' { b"form\t\t+\t1\r\n".as_slice() } else { b"form\t+\t1\r\n".as_slice() });
            assert_eq!(line(&mut stream, MAX_REQUEST).await.unwrap(), b"+8\r\n");
            let mut body = [0; 8]; stream.read_exact(&mut body).await.unwrap();
            assert_eq!(&body, b"one\r\n\0\r\n");
            stream.write_all(b"+2\r\nOK").await.unwrap();
        });
        let url = Url::parse(&format!("gopher+://127.0.0.1:{port}/{kind}form%09%09%3F")).unwrap();
        let request = Request::parse(&url).unwrap().with_command(Command::Default);
        let result = submit(&url, &request, b"one\r\n\0\r\n").await.unwrap();
        assert_eq!(result.bytes, b"OK"); server.await.unwrap();
        assert!(url.as_str().ends_with("%3F"));
      }
    }
}
