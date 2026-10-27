// SPDX-License-Identifier: GPL-3.0-or-later
//! Hand mail and torrent links to the user's selected desktop application.
use std::{ffi::CString, sync::mpsc::{self, Receiver, Sender}};
use url::Url;

unsafe extern "C" { fn browser_launch_default_uri(uri: *const std::ffi::c_char) -> i32; }

pub fn handles(url: &Url) -> bool { matches!(url.scheme(),"mailto"|"misfin"|"magnet") }
pub fn label(url: &Url) -> &'static str { if url.scheme()=="magnet" {"torrent"} else {"mail"} }

fn target(url: &Url) -> Result<CString, &'static str> {
    let address = if url.scheme()=="misfin" {
        url.has_host() && !url.username().is_empty() && url.password().is_none() &&
            matches!(url.path(), ""|"/") && url.query().is_none() && url.fragment().is_none() && url.port()!=Some(0)
    } else { url.cannot_be_a_base() };
    if !handles(url) || !address || url.as_str().len()>65536 ||
        url.as_str().chars().any(char::is_control) || percent_encoding::percent_decode_str(url.as_str()).any(|byte| byte==0) {
        return Err("Invalid external application address");
    }
    CString::new(url.as_str()).map_err(|_| "Invalid external application address")
}
pub fn launch_once(url: &Url) -> Result<(), &'static str> {
    let uri=target(url)?;
    match unsafe { browser_launch_default_uri(uri.as_ptr()) } {
        0=>Ok(()),
        1=>Err("No default application is configured for this link; choose one in DomeD Default Applications"),
        2=>Err("Cannot read DomeD default application preferences"),
        3=>Err("The selected default application could not open this link"),
        _=>Err("Invalid external application address"),
    }
}
pub struct Handoff { sender: Sender<Result<(), &'static str>>, pub receiver: Receiver<Result<(), &'static str>> }
impl Handoff {
    pub fn new() -> Self { let (sender,receiver)=mpsc::channel();Self {sender,receiver} }
    pub fn launch(&self,url: Url) {
        let sender=self.sender.clone();
        std::thread::spawn(move || {let _=sender.send(launch_once(&url));});
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handler_uris_preserve_mail_fields_and_magnet_parameters() {
        for text in ["mailto:person@example.invalid?subject=Hello%20there&body=Line%201%0ALine%202", "magnet:?xt=urn:btih:0123456789012345678901234567890123456789&dn=Local%20fixture&tr=https%3A%2F%2Fexample.invalid%2Fannounce"] {
            assert_eq!(target(&Url::parse(text).unwrap()).unwrap().to_str().unwrap(),text);
        }
        for text in ["https://example.invalid/", "file:///tmp/fixture", "mailto://example.invalid/", "magnet://example.invalid/", "mailto:user%00@example.invalid"] {
            assert!(target(&Url::parse(text).unwrap()).is_err());
        }
    }
    #[test]
    fn misfin_addresses_are_handed_to_a_mail_client_without_rewriting() {
        for text in ["misfin://Mailbox@example.invalid", "misfin://alice@localhost:1959/", "misfin://bob@[::1]:1958"] {
            let url=Url::parse(text).unwrap();
            assert!(handles(&url));
            assert_eq!(label(&url),"mail");
            assert_eq!(target(&url).unwrap().to_str().unwrap(),text);
        }
        for text in ["misfin:alice@example.invalid", "misfin://example.invalid/", "misfin://alice:secret@example.invalid", "misfin://alice@example.invalid/path", "misfin://alice@example.invalid?body=Hello", "misfin://alice@example.invalid#fragment", "misfin://alice@localhost:0", "misfin://alice%00@example.invalid"] {
            assert!(target(&Url::parse(text).unwrap()).is_err(),"{text}");
        }
    }
}
