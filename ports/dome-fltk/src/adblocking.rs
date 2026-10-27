// SPDX-License-Identifier: GPL-3.0-or-later
use adblock::{Engine, lists::{FilterSet, ParseOptions, RuleTypes}, request::Request};
use std::{collections::HashSet, io, path::{Path, PathBuf}, process::Command};
use crate::storage;

const EASYLIST: &str = include_str!("../assets/filters/easylist.txt");
const EASYPRIVACY: &str = include_str!("../assets/filters/easyprivacy.txt");
pub struct Blocker {
    engine: Engine,
    pub enabled: bool,
    pub blocked: u64,
    allowed: HashSet<String>,
    profile: PathBuf,
}
fn engine(lists: &[String], custom: &str) -> Engine {
    let mut filters=FilterSet::new(false);
    for list in lists.iter().map(String::as_str).chain(std::iter::once(custom)) {
        filters.add_filter_list(list.to_owned(), ParseOptions { rule_types: RuleTypes::All, ..Default::default() });
    }
    Engine::new_with_filter_set(filters)
}
impl Blocker {
    pub fn load(profile: &Path) -> io::Result<Self> {
        let lists=match storage::read(&profile.join("filterlists.json")) {
            Ok(text) => serde_json::from_str::<Vec<String>>(&text).map_err(io::Error::other)?,
            Err(e) if e.kind()==io::ErrorKind::NotFound => vec![EASYLIST.into(),EASYPRIVACY.into()],
            Err(e)=>return Err(e),
        };
        let custom=match storage::read(&profile.join("adblock.txt")) { Ok(s)=>s, Err(e) if e.kind()==io::ErrorKind::NotFound=>String::new(), Err(e)=>return Err(e) };
        let allowed=match storage::read(&profile.join("adblock-allowlist.txt")) { Ok(s)=>s.lines().map(str::to_owned).collect(), Err(e) if e.kind()==io::ErrorKind::NotFound=>HashSet::new(), Err(e)=>return Err(e) };
        let enabled=!profile.join("adblock-disabled").exists();
        Ok(Self { engine:engine(&lists,&custom),enabled,blocked:0,allowed,profile:profile.into() })
    }
    pub fn toggle(&mut self) -> io::Result<()> {
        let path=self.profile.join("adblock-disabled");
        if self.enabled { storage::write(&path,"disabled\n")?; } else if path.exists() { std::fs::remove_file(path)?; }
        self.enabled=!self.enabled;Ok(())
    }
    pub fn allow_site(&mut self, origin: &str) -> io::Result<()> {
        let mut allowed=self.allowed.clone();
        if !allowed.remove(origin) { allowed.insert(origin.into()); }
        let mut lines:Vec<_>=allowed.iter().cloned().collect();lines.sort();
        storage::write(&self.profile.join("adblock-allowlist.txt"),&(lines.join("\n")+"\n"))?;
        self.allowed=allowed;Ok(())
    }
    pub fn allowed(&self, origin: &str) -> bool { self.allowed.contains(origin) }
    pub fn cosmetic_filters(&self, request: servo::CosmeticFilterRequest) -> Vec<String> {
        if !self.enabled || request.top_level_origin.as_ref().is_some_and(|s| self.allowed.contains(s)) {
            return Vec::new();
        }
        let resources = self.engine.url_cosmetic_resources(request.url.as_str());
        let mut selectors = resources.hide_selectors;
        if !resources.generichide {
            selectors.extend(self.engine.hidden_class_id_selectors(
                request.classes, request.ids, &resources.exceptions,
            ));
        }
        let mut selectors: Vec<_> = selectors.into_iter().collect(); selectors.sort(); selectors
    }
    pub fn intercept(&mut self, load: servo::WebResourceLoad) {
        let r=load.request();
        // Exceptions belong to the top-level site. An allowlisted iframe or
        // worker must not disable filtering in a different site. If no top-level
        // context is known, continue filtering rather than trusting the initiator.
        if !self.enabled || r.top_level_origin.as_ref().is_some_and(|s|self.allowed.contains(s)) { return; }
        let destination=format!("{:?}",r.destination).to_ascii_lowercase();
        let kind=if r.is_websocket { "websocket" } else if r.is_beacon { "ping" } else { match destination.as_str() {
            "style"=>"stylesheet", "iframe"|"frame"=>"subdocument", "audio"|"video"=>"media",
            "worker"|"sharedworker"|"serviceworker"=>"script", "none"|"empty"|""=>"xmlhttprequest",
            "document"|"image"|"font"|"script"=>destination.as_str(), _=>"other",
        }};
        // Fetch represents WebSocket handshakes as HTTP(S), including HSTS
        // upgrades. Match the corresponding WS(S) URL without changing fetch.
        let mut filter_url=r.url.clone().into_url();
        if r.is_websocket {
            let scheme=match filter_url.scheme() { "http"=>"ws", "https"=>"wss", _=>return };
            if filter_url.set_scheme(scheme).is_err() { return; }
        }
        let Ok(request)=Request::new(filter_url.as_str(),r.initiator_origin.as_deref().unwrap_or(""),kind,r.method.as_str()) else { return; };
        let result = self.engine.check_network_request(&request);
        if result.should_block() {
            self.blocked+=1;
            let response=servo::WebResourceResponse::new(r.url.clone().into_url());
            load.intercept(response).cancel();
            return;
        }
        // Only actual URL queries are removable. A '?' inside a fragment is
        // never sent over HTTP and must not be treated as a tracking query.
        if r.url.query().is_none() { return; }
        let Some(rewritten) = result.rewritten_url else { return; };
        let rewritten = (|| {
            let mut url = url::Url::parse(&rewritten).ok()?;
            if r.is_websocket {
                let scheme = match url.scheme() { "ws"=>"http", "wss"=>"https", _=>return None };
                url.set_scheme(scheme).ok()?;
            }
            Some(url)
        })();
        if let Some(url) = rewritten {
            load.remove_query_parameters(url);
        } else {
            self.blocked += 1;
            let response = servo::WebResourceResponse::new(r.url.clone().into_url());
            load.intercept(response).cancel();
        }
    }
}
/// Fetch only the two official HTTPS lists. Updating never runs in a request callback.
pub fn download() -> Result<Vec<String>,String> {
    let mut lists=Vec::new();
    for name in ["easylist","easyprivacy"] {
        let url=format!("https://easylist.to/easylist/{name}.txt");
        let out=Command::new("curl").args(["--fail","--silent","--show-error","--location","--proto","=https","--proto-redir","=https","--max-time","45","--max-filesize","4194304",&url])
            .output().map_err(|_|"Cannot start curl for filter-list update")?;
        if !out.status.success() { return Err("Filter-list download failed; previous lists retained".into()); }
        let text=String::from_utf8(out.stdout).map_err(|_|"Filter list is not UTF-8")?;
        if !text.starts_with("[Adblock Plus") || text.len()<1000 { return Err("Invalid filter-list response".into()); }
        lists.push(text);
    }
    Ok(lists)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn cosmetic_blocker() -> Blocker {
        Blocker { engine: engine(&[], "##.generic\n###ad-id\n##.kept\nsite.example#@#.kept\nsite.example##.specific\nother.example##.other\n@@||site.example/generic-off$generichide\n"),
            enabled: true, blocked: 0, allowed: HashSet::from(["https://exempt.example".into()]), profile: PathBuf::new() }
    }
    fn cosmetic_request(url: &str, top: Option<&str>) -> servo::CosmeticFilterRequest {
        servo::CosmeticFilterRequest { url: servo::ServoUrl::parse(url).unwrap(),
            top_level_origin: top.map(str::to_owned), classes: vec!["generic".into(), "kept".into()], ids: vec!["ad-id".into()] }
    }
    #[test]
    fn cosmetic_rules_honor_hostname_exceptions_and_generic_hide() {
        let b = cosmetic_blocker();
        assert_eq!(b.cosmetic_filters(cosmetic_request("https://site.example/", Some("https://site.example"))), vec!["#ad-id", ".generic", ".specific"]);
        assert_eq!(b.cosmetic_filters(cosmetic_request("https://site.example/generic-off", Some("https://site.example"))), vec![".specific"]);
        assert_eq!(b.cosmetic_filters(cosmetic_request("https://other.example/", Some("https://other.example"))), vec!["#ad-id", ".generic", ".kept", ".other"]);
    }
    #[test]
    fn opaque_local_documents_receive_generic_rules_without_hostname_exceptions() {
        let b = cosmetic_blocker();
        for url in ["about:blank", "about:srcdoc"] {
            assert_eq!(b.cosmetic_filters(cosmetic_request(url, None)),
                vec!["#ad-id", ".generic", ".kept"]);
        }
    }
    #[test]
    fn cosmetic_site_exceptions_require_trusted_top_level_context() {
        let mut b = cosmetic_blocker();
        assert!(b.cosmetic_filters(cosmetic_request("https://exempt.example/", Some("https://exempt.example"))).is_empty());
        assert!(!b.cosmetic_filters(cosmetic_request("https://exempt.example/", None)).is_empty());
        assert!(!b.cosmetic_filters(cosmetic_request("https://exempt.example/", Some("https://site.example"))).is_empty());
        b.enabled = false;
        assert!(b.cosmetic_filters(cosmetic_request("https://site.example/", None)).is_empty());
    }
    #[test]
    fn removeparam_preserves_unrelated_raw_query_fields_and_fragment() {
        let e = engine(&[], "/privacy$removeparam=utm_source\n");
        let r = Request::new("https://site.example/privacy?keep=a%2Bb&utm_source=secret&keep=two#route", "https://site.example/", "xmlhttprequest", "GET").unwrap();
        let result = e.check_network_request(&r);
        assert!(!result.should_block());
        assert_eq!(result.rewritten_url.as_deref(), Some("https://site.example/privacy?keep=a%2Bb&keep=two#route"));
    }
    #[test]
    fn blocks_types_third_party_and_honors_exceptions() {
        let e=engine(&["||ads.example^$script,third-party\n@@||ads.example/allowed.js$script\n".into()], "");
        let check=|url,source,kind|e.check_network_request(&Request::new(url,source,kind,"GET").unwrap()).should_block();
        assert!(check("https://ads.example/ad.js","https://site.example","script"));
        assert!(!check("https://ads.example/ad.js","https://ads.example","script"));
        assert!(!check("https://ads.example/allowed.js","https://site.example","script"));
        assert!(!check("https://ads.example/a.png","https://site.example","image"));
    }
}
