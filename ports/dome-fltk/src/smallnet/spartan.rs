// SPDX-License-Identifier: GPL-3.0-or-later
//! https://github.com/michael-lazar/spartan/blob/main/public/specification.gmi
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use url::Url;

use super::{Error, MAX_BYTES, MAX_REQUEST, normalize_address};

pub(super) enum Reply {
    Success(String, Vec<u8>),
    Stream(super::transfer::Transfer),
    Redirect(Url),
    Failure(u16, String),
}

pub(super) fn request_bytes(url: &Url) -> Result<Vec<u8>, Error> {
    let mut url = url.clone();
    normalize_address(&mut url)?;
    let host = url.host_str().ok_or(Error::Address)?;
    let path = if url.path().is_empty() { "/" } else { url.path() };
    if !path.starts_with('/') || !path.is_ascii() || path.bytes().any(|b| b <= 32 || b == 127) {
        return Err(Error::Address);
    }
    let body: Vec<u8> = percent_encoding::percent_decode_str(url.query().unwrap_or("")).collect();
    if body.len() > MAX_BYTES { return Err(Error::TooLarge); }
    let mut request = format!("{host} {path} {}\r\n", body.len()).into_bytes();
    if request.len() > MAX_REQUEST { return Err(Error::Address); }
    request.extend_from_slice(&body);
    Ok(request)
}

fn redirect_url(current: &Url, path: &str) -> Result<Url, Error> {
    if !path.starts_with('/') || path.starts_with("//") || path.contains(['?', '#']) ||
        !path.is_ascii() || path.bytes().any(|b| b <= 32 || b == 127) {
        return Err(Error::Response);
    }
    let target = current.join(path).map_err(|_| Error::Response)?;
    if target.scheme() != current.scheme() || target.host_str() != current.host_str() || target.port() != current.port() {
        return Err(Error::Response);
    }
    Ok(target)
}

#[cfg(test)]
pub(super) async fn fetch(url:&Url)->Result<Reply,Error>{fetch_inner(url,None).await}
pub(super) async fn open(url:&Url,download:bool)->Result<Reply,Error>{fetch_inner(url,Some(download)).await}
async fn fetch_inner(url:&Url,download:Option<bool>) -> Result<Reply, Error> {
    let request = request_bytes(url)?;
    let host = url.host_str().ok_or(Error::Address)?.trim_matches(['[', ']']);
    let mut stream = TcpStream::connect((host, url.port().unwrap_or(300))).await.map_err(|_| Error::Transport)?;
    stream.write_all(&request).await.map_err(|_| Error::Transport)?;
    let mut stream = BufReader::new(stream);
    let mut header = Vec::new();
    while header.len() < MAX_REQUEST {
        header.push(stream.read_u8().await.map_err(|_| Error::Response)?);
        if header.ends_with(b"\r\n") { break; }
    }
    if !header.ends_with(b"\r\n") || header.len() < 5 || header[1] != b' ' { return Err(Error::Response); }
    let meta = &header[2..header.len()-2];
    if meta.is_empty() || meta.iter().any(|b| (*b < 32 && *b != b'\t') || *b >= 127) { return Err(Error::Response); }
    let meta = std::str::from_utf8(meta).map_err(|_| Error::Response)?.to_owned();
    match header[0] {
        b'2' => {
            let kind:mime::Mime=meta.parse().map_err(|_|Error::Response)?;
            if download.is_some_and(|download|download || !(kind.type_()==mime::TEXT || kind.type_()==mime::IMAGE || kind.essence_str()=="application/xhtml+xml")) {
                return Ok(Reply::Stream(super::transfer::Transfer::new(kind.to_string(),None,stream)));
            }
            let mut bytes = Vec::new();
            stream.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes).await.map_err(|_| Error::Transport)?;
            if bytes.len() > MAX_BYTES { return Err(Error::TooLarge); }
            Ok(Reply::Success(meta, bytes))
        }
        b'3' => Ok(Reply::Redirect(redirect_url(url, &meta)?)),
        b'4' => Ok(Reply::Failure(400, format!("Spartan client error\n\n{meta}"))),
        b'5' => Ok(Reply::Failure(500, format!("Spartan server error\n\n{meta}"))),
        _ => Err(Error::Response),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_mapping_preserves_paths_and_counts_body_octets() {
        for (url, expected) in [
            ("spartan://example.com", b"example.com / 0\r\n".as_slice()),
            ("spartan://anon@example.com:3000/#about", b"example.com / 0\r\n"),
            ("spartan://example.com/my%20file.txt", b"example.com /my%20file.txt 0\r\n"),
            ("spartan://example.com?hello%20world", b"example.com / 11\r\nhello world"),
            ("spartan://example.com?a=1&b=2", b"example.com / 7\r\na=1&b=2"),
            ("spartan://[::1]/", b"[::1] / 0\r\n"),
            ("spartan://examplé.com/", b"xn--exampl-gva.com / 0\r\n"),
            ("spartan://example.com/?%00%0A", b"example.com / 2\r\n\0\n"),
        ] {
            assert_eq!(request_bytes(&Url::parse(url).unwrap()).unwrap(), expected, "{url}");
        }
    }

    #[test]
    fn redirects_remain_absolute_paths_on_the_same_server() {
        let url = Url::parse("spartan://example.com:3000/old?input").unwrap();
        assert_eq!(redirect_url(&url, "/new").unwrap().as_str(), "spartan://example.com:3000/new");
        for path in ["relative", "//elsewhere/", "https://elsewhere/", "/path?query", "/path#fragment", "/bad path"] {
            assert_eq!(redirect_url(&url, path), Err(Error::Response));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn split_header_and_binary_body_are_preserved() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n") { request.push(stream.read_u8().await.unwrap()); }
            assert_eq!(request, b"127.0.0.1 / 4\r\n");
            let mut body = [0; 4];
            stream.read_exact(&mut body).await.unwrap();
            assert_eq!(body, [0, 10, 195, 169]);
            for fragment in [b"2 text/".as_slice(), b"plain\r", b"\nHello\0world"] {
                stream.write_all(fragment).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let url = Url::parse(&format!("spartan://127.0.0.1:{port}/?%00%0A%C3%A9")).unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), fetch(&url)).await.unwrap().unwrap();
        match result { Reply::Success(mime, body) => { assert_eq!(mime, "text/plain"); assert_eq!(body, b"Hello\0world"); }, _ => panic!("Expected successful content") }
        server.await.unwrap();
    }
}
