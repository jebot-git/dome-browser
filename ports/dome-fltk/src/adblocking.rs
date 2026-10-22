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
        filters.add_filter_list(list.to_owned(), ParseOptions { rule_types: RuleTypes::NetworkOnly, ..Default::default() });
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
    pub fn intercept(&mut self, load: servo::WebResourceLoad, top: Option<&str>) {
        let r=load.request();
        if !self.enabled || top.is_some_and(|s|self.allowed.contains(s)) ||
            r.initiator_origin.as_ref().is_some_and(|s|self.allowed.contains(s)) { return; }
        let destination=format!("{:?}",r.destination).to_ascii_lowercase();
        let kind=match destination.as_str() {
            "style"=>"stylesheet", "iframe"|"frame"=>"subdocument", "audio"|"video"=>"media",
            "worker"|"sharedworker"|"serviceworker"=>"script", "none"|"empty"|""=>"xmlhttprequest",
            "document"|"image"|"font"|"script"=>destination.as_str(), _=>"other",
        };
        let Ok(request)=Request::new(r.url.as_str(),r.initiator_origin.as_deref().unwrap_or(""),kind,r.method.as_str()) else { return; };
        if self.engine.check_network_request(&request).should_block() {
            self.blocked+=1;
            let response=servo::WebResourceResponse::new(r.url.clone().into_url());
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
