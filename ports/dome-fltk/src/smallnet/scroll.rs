// SPDX-License-Identifier: GPL-3.0-or-later
//! Scroll wire format: the author's archived speculative specification.
use super::{Error,gemini,normalize_address};
use tokio::io::{AsyncRead,AsyncReadExt};
use url::Url;

const HEADER_LIMIT:usize=16*1024;
#[derive(Clone,Default)]
pub(super) struct Metadata {pub author:String,pub published:String,pub modified:String}

fn language_tag(text:&str)->Result<language_tags::LanguageTag,Error> {
    if text.split('-').any(|part|part.is_empty() || part.len()>8 || !part.bytes().all(|b|b.is_ascii_alphanumeric())) {return Err(Error::Address);}
    language_tags::LanguageTag::parse(text).map_err(|_|Error::Address)
}
pub(super) fn validate_host(url:&Url)->Result<(),Error> {
    let host=percent_encoding::percent_decode_str(url.host_str().ok_or(Error::Address)?).decode_utf8().map_err(|_|Error::Address)?;
    if host.chars().any(|c|c.is_control() || c.is_whitespace() || matches!(c,'\u{00ad}'|'\u{034f}'|'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2060}'..='\u{206f}'|'\u{feff}')) {return Err(Error::Address);}
    Ok(())
}

fn locale_languages(values:&[String])->String {
    let mut tags=Vec::new();
    for value in values {
        for value in value.split(':') {
            let script=value.rsplit_once('@').and_then(|(_,modifier)|match modifier {"latin"=>Some("Latn"),"cyrillic"=>Some("Cyrl"),"arabic"=>Some("Arab"),_=>None});
            let mut value=value.split(['.','@']).next().unwrap_or("").replace('_',"-");
            if let Some(script)=script {
                let mut parts=value.split('-').collect::<Vec<_>>();
                if parts.len()<2 || parts[1].len()!=4 {parts.insert(1,script);value=parts.join("-");}
            }
            if value.len()>64 || matches!(value.as_str(),"C"|"POSIX") {continue;}
            if let Ok(tag)=language_tag(&value) {
                let tag=tag.to_string();
                if !tags.contains(&tag) {tags.push(tag);}
            }
            if tags.len()==8 {return tags.join(",");}
        }
    }
    if tags.is_empty(){"en".into()}else{tags.join(",")}
}
pub(super) fn languages()->String {
    let mut locales=Vec::new();
    if let Ok(value)=std::env::var("LANGUAGE") {locales.push(value);}
    for name in ["LC_ALL","LC_MESSAGES","LANG"] {
        if let Some(value)=std::env::var(name).ok().filter(|value|!value.is_empty()) {locales.push(value);break;}
    }
    locale_languages(&locales)
}
pub(super) fn request_bytes(url:&Url,languages:&str,metadata:bool)->Result<Vec<u8>,Error> {
    if url.scheme()!="scroll" || !url.username().is_empty() || url.password().is_some() || url.path().contains(';') {return Err(Error::Address);}
    if languages.is_empty() || languages.len()>512 || languages.split(',').any(|tag|language_tag(tag).is_err()) {return Err(Error::Address);}
    let mut url=url.clone();normalize_address(&mut url)?;url.set_fragment(None);
    if url.path().is_empty(){url.set_path("/");}
    if url.as_str().len()>8192 || url.as_str().chars().any(|c|c.is_control() || c==' ') {return Err(Error::Address);}
    Ok(format!("{} {}{}\r\n",url.as_str(),if metadata{"+"}else{""},languages).into_bytes())
}
async fn line(stream:&mut(impl AsyncRead+Unpin))->Result<Vec<u8>,Error> {
    let mut bytes=Vec::new();
    while bytes.len()<HEADER_LIMIT {
        bytes.push(stream.read_u8().await.map_err(|_|Error::Response)?);
        if bytes.ends_with(b"\r\n") {return Ok(bytes);}
    }
    Err(Error::TooLarge)
}
async fn metadata_line(stream:&mut(impl AsyncRead+Unpin))->Result<String,Error> {
    let bytes=line(stream).await?;
    let text=std::str::from_utf8(&bytes[..bytes.len()-2]).map_err(|_|Error::Response)?;
    if text.chars().any(char::is_control) {return Err(Error::Response);}
    Ok(text.into())
}
pub(super) async fn response_header(stream:&mut(impl AsyncRead+Unpin),url:&Url)->Result<gemini::Reply,Error> {
    let first=line(stream).await?;
    if first.get(2)!=Some(&b' ') {return Err(Error::Response);}
    match gemini::header(&first,url)? {
        gemini::Reply::Success(mime,_)=>{
            let metadata=Metadata {author:metadata_line(stream).await?,published:metadata_line(stream).await?,modified:metadata_line(stream).await?};
            Ok(gemini::Reply::ScrollSuccess {mime,bytes:Vec::new(),metadata})
        },
        other=>Ok(other),
    }
}

#[cfg(test)]
async fn response(stream:&mut(impl AsyncRead+Unpin),url:&Url)->Result<gemini::Reply,Error> {
    let reply=response_header(stream,url).await?;
    gemini::complete_response(reply,stream).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_languages_and_metadata_mode_preserve_the_resource_url() {
        let url=Url::parse("scroll://examplé.org/search?two%20words#section").unwrap();
        assert_eq!(request_bytes(&url,"cs-CZ,en",false).unwrap(),b"scroll://xn--exampl-gva.org/search?two%20words cs-CZ,en\r\n");
        assert_eq!(request_bytes(&url,"cs-CZ,en",true).unwrap(),b"scroll://xn--exampl-gva.org/search?two%20words +cs-CZ,en\r\n");
        assert_eq!(locale_languages(&["cs_CZ.UTF-8:en_US:cs_CZ:zh_Hans_CN".into()]),"cs-CZ,en-US,zh-Hans-CN");
        assert_eq!(locale_languages(&["C.UTF-8".into()]),"en");
        assert_eq!(locale_languages(&["sr_RS.UTF-8@latin".into()]),"sr-Latn-RS");
        for tags in ["","en,","en\r\nextra","not a tag"] {assert!(request_bytes(&url,tags,false).is_err());}
        assert!(request_bytes(&Url::parse("scroll://host/file;param=1").unwrap(),"en",false).is_err());
        assert!(request_bytes(&Url::parse("scroll://ho%E2%80%8Bst/").unwrap(),"en",false).is_err());
        assert!(request_bytes(&url,"x--bad",false).is_err());
    }
    #[tokio::test(flavor="current_thread")]
    async fn scroll_success_metadata_and_status_groups_keep_the_body_separate() {
        let url=Url::parse("scroll://host/page").unwrap();
        match response(&mut b"24 text/scroll; lang=en\r\nAuthor\r\n2026-10-25T12:00:00Z\r\n\r\n# Title\nBody\n".as_slice(),&url).await.unwrap() {
            gemini::Reply::ScrollSuccess {mime,bytes,metadata}=>{
                assert_eq!(mime,"text/scroll; lang=en");assert_eq!(metadata.author,"Author");assert_eq!(metadata.published,"2026-10-25T12:00:00Z");assert!(metadata.modified.is_empty());assert_eq!(bytes,b"# Title\nBody\n");
            },_=>panic!("Expected Scroll success"),
        }
        assert!(matches!(response(&mut b"10 \r\n".as_slice(),&url).await.unwrap(),gemini::Reply::Input {sensitive:false,..}));
        for raw in [b"24 text/scroll\r\nMissing metadata".as_slice(),b"50\r\n",b"24 text/scroll\r\nBad\0author\r\n\r\n\r\n"] {
            assert!(response(&mut &*raw,&url).await.is_err());
        }
    }
}
