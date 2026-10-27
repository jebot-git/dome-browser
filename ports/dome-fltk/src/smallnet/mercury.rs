// SPDX-License-Identifier: GPL-3.0-or-later
//! Mercury uses Gemini-style requests and responses over plain TCP, port 1961.
//! Reference: https://github.com/reiver/go-hg (request.go and consts.go).
use super::{Error, gemini};
use tokio::{io::{AsyncWriteExt, BufReader}, net::TcpStream};
use url::Url;

pub(super) fn request_bytes(url: &Url) -> Result<Vec<u8>, Error> {
    // The reference implementation accepts 2048 bytes including CRLF.
    gemini::uri_request_bytes(url, "mercury", 2046)
}

pub(super) async fn open(url: &Url, download: bool) -> Result<gemini::Reply, Error> {
    let request = request_bytes(url)?;
    let host = url.host_str().ok_or(Error::Address)?.trim_matches(['[', ']']);
    let port = url.port().unwrap_or(1961);
    if port == 0 { return Err(Error::Address); }
    let mut stream = TcpStream::connect((host, port)).await.map_err(|_| Error::Transport)?;
    stream.write_all(&request).await.map_err(|_| Error::Transport)?;
    let (reader, writer) = stream.into_split();
    gemini::receive(BufReader::new(reader), writer, url, false, download).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_request_limit_normalization_and_fragment_omission() {
        let url = Url::parse("mercury://EXAMPLE.com/find?two%20words#section").unwrap();
        assert_eq!(request_bytes(&url).unwrap(), b"mercury://example.com/find?two%20words\r\n");
        assert_eq!(request_bytes(&Url::parse("mercury://host#fragment").unwrap()).unwrap(), b"mercury://host/\r\n");
        let prefix = "mercury://host/";
        for (length, accepted) in [(1025, true), (2046, true), (2047, false)] {
            let url = Url::parse(&format!("{prefix}{}", "x".repeat(length-prefix.len()))).unwrap();
            assert_eq!(request_bytes(&url).is_ok(), accepted);
        }
        for text in ["gemini://host/", "mercury://user@host/", "mercury://host:0/", "mercury://host:25/"] {
            assert!(request_bytes(&Url::parse(text).unwrap()).is_err(), "{text}");
        }
        assert!(gemini::request_bytes(&url).is_err());
    }
}
