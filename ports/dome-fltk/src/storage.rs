// SPDX-License-Identifier: GPL-3.0-or-later
//! Profile files: tab sessions and Dillo-compatible plain-text bookmarks.
use serde::{Deserialize, Serialize};
use std::{fs::{self, OpenOptions}, io::{self, Read, Write}, os::unix::fs::OpenOptionsExt, path::Path};
use url::Url;

pub fn read(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    std::fs::File::open(path)?.take(4 * 1024 * 1024 + 1).read_to_string(&mut text)?;
    if text.len() > 4 * 1024 * 1024 { return Err(invalid("Profile file exceeds 4 MiB")); }
    Ok(text)
}
fn invalid(message: &str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message) }
pub fn write(path: &Path, text: &str) -> io::Result<()> {
    if text.len() > 4 * 1024 * 1024 { return Err(invalid("Profile file exceeds 4 MiB")); }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut created = false;
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
        created = true;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        std::fs::File::open(path.parent().unwrap())?.sync_all()
    })();
    if result.is_err() && created { let _ = fs::remove_file(temporary); }
    result
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub url: String, pub zoom: f32,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub protocol_state: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session { pub version: u32, pub tabs: Vec<SavedTab>, pub active: usize }
impl Session {
    pub fn parse(text: &str) -> io::Result<Self> {
        let mut session: Self = serde_json::from_str(text).map_err(|e| invalid(&e.to_string()))?;
        if session.version != 1 || session.tabs.is_empty() || session.tabs.len() > 100 || session.active >= session.tabs.len() {
            return Err(invalid("Unsupported or invalid session"));
        }
        for tab in &mut session.tabs {
            tab.url=persistent_url(&tab.url).ok_or_else(||invalid("Invalid saved tab URL"))?;
            if tab.protocol_state.as_deref().is_some_and(|state|state!="abstract" || !tab.url.starts_with("scroll://")) {
                return Err(invalid("Invalid saved protocol view"));
            }
            if !tab.zoom.is_finite() || !(0.25..=5.).contains(&tab.zoom) {
                return Err(invalid("Invalid saved tab"));
            }
        }
        Ok(session)
    }
    pub fn load(path: &Path) -> io::Result<Self> { Self::parse(&read(path)?) }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let normalized=Self::parse(&serde_json::to_string(self).map_err(io::Error::other)?)?;
        let text = serde_json::to_string_pretty(&normalized).map_err(io::Error::other)? + "\n";
        write(path, &text)
    }
}
pub fn restorable(text: &str) -> bool {
    persistent_url(text).is_some()
}
pub fn persistent_url(text:&str)->Option<String> {
    let url=Url::parse(text).ok()?;
    if matches!(url.scheme(),"http"|"https"|"file") || text=="about:blank" {
        return (url.username().is_empty() && url.password().is_none()).then(||text.to_owned());
    }
    crate::smallnet::persistent_url(&url).map(|url|url.to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark { pub section: usize, pub url: String, pub title: String }
#[derive(Clone, Debug, PartialEq)]
pub struct Bookmarks { pub sections: Vec<String>, pub entries: Vec<Bookmark> }
impl Default for Bookmarks {
    fn default() -> Self { Self { sections: vec!["Unclassified".into()], entries: vec![] } }
}
fn clean(text: &str) -> String { text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect() }
fn unescape(text: &str) -> String {
    let mut result = String::new(); let mut rest = text;
    while let Some(pos) = rest.find('&') {
        result.push_str(&rest[..pos]); rest = &rest[pos..];
        if let Some(end) = rest.find(';') {
            let entity = &rest[1..end];
            let value = match entity {
                "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'), "apos" => Some('\''),
                n if n.starts_with("#x") => u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32),
                n if n.starts_with('#') => n[1..].parse::<u32>().ok().and_then(char::from_u32), _ => None,
            };
            if let Some(value) = value { result.push(value); rest = &rest[end+1..]; continue; }
        }
        result.push('&'); rest = &rest[1..];
    }
    result.push_str(rest); clean(&result)
}
impl Bookmarks {
    pub fn parse(text: &str) -> io::Result<Self> {
        let mut sections = Vec::<(usize, String)>::new(); let mut entries = vec![];
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            if let Some(line) = line.strip_prefix(":s") {
                let (id, title) = line.split_once(':').ok_or_else(|| invalid("Invalid bm.txt section"))?;
                let id = id.parse().map_err(|_| invalid("Invalid section number"))?;
                if sections.iter().any(|(old, _)| *old == id) { return Err(invalid("Duplicate section")); }
                sections.push((id, clean(title.trim())));
            } else if let Some(line) = line.strip_prefix('s') {
                let (id, rest) = line.split_once(char::is_whitespace).ok_or_else(|| invalid("Invalid bookmark"))?;
                let (url, title) = rest.trim_start().split_once(char::is_whitespace).ok_or_else(|| invalid("Bookmark needs a title"))?;
                let section = id.parse().map_err(|_| invalid("Invalid section number"))?;
                let url=persistent_url(url).ok_or_else(||invalid("Unsupported bookmark URL"))?;
                entries.push(Bookmark { section, url, title: unescape(title.trim()) });
            } else { return Err(invalid("Unrecognized bm.txt line")); }
        }
        if sections.is_empty() { sections.push((0, "Unclassified".into())); }
        for entry in &mut entries {
            entry.section = sections.iter().position(|(id, _)| *id == entry.section).ok_or_else(|| invalid("Bookmark refers to an unknown section"))?;
        }
        Ok(Self { sections: sections.into_iter().map(|(_, name)| name).collect(), entries })
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        match read(path) { Ok(text) => Self::parse(&text), Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()), Err(e) => Err(e) }
    }
    pub fn text(&self) -> String {
        let mut text = String::new();
        for (id, name) in self.sections.iter().enumerate() { text += &format!(":s{id}: {}\n", clean(name)); }
        for b in &self.entries { text += &format!("s{} {} {}\n", b.section, b.url, clean(&b.title)); }
        text
    }
    pub fn add(&mut self, section: &str, url: &str, title: &str) -> io::Result<()> {
        let url=persistent_url(url).ok_or_else(||invalid("This URL cannot be bookmarked"))?;
        let section = clean(section); let section = if section.trim().is_empty() { "Unclassified" } else { section.trim() };
        let id = self.sections.iter().position(|s| s == section).unwrap_or_else(|| { self.sections.push(section.into()); self.sections.len()-1 });
        if let Some(b) = self.entries.iter_mut().find(|b| b.section == id && b.url == url) { b.title = clean(title); }
        else { self.entries.push(Bookmark { section: id, url, title: clean(title) }); }
        Ok(())
    }
    pub fn merge(&mut self, other: Self) -> io::Result<()> {
        for name in &other.sections {
            if !self.sections.contains(name) { self.sections.push(name.clone()); }
        }
        for entry in other.entries { self.add(&other.sections[entry.section], &entry.url, &entry.title)?; }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> io::Result<()> { write(path, &self.text()) }
}

/// Most recently visited URLs first, one URL per line, with duplicates removed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History { pub urls: Vec<String> }
impl History {
    pub fn load(path: &Path) -> io::Result<Self> {
        let text = match read(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let mut history = Self::default();
        for url in text.lines().filter(|line| !line.is_empty()).rev() {
            if !restorable(url) || url == "about:blank" { return Err(invalid("Invalid hs.txt URL")); }
            history.visit(url);
        }
        Ok(history)
    }
    pub fn visit(&mut self, url: &str) -> bool {
        let Some(url)=persistent_url(url) else{return false;};
        if url == "about:blank" || self.urls.first().is_some_and(|s| *s == url) { return false; }
        self.urls.retain(|old| *old != url);
        self.urls.insert(0, url);
        self.urls.truncate(2000);
        let mut size = 0;
        self.urls.retain(|url| { size += url.len() + 1; size <= 4 * 1024 * 1024 });
        true
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = if self.urls.is_empty() { String::new() } else { self.urls.join("\n") + "\n" };
        write(path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_view_roundtrips_and_old_sessions_default_to_resource() {
        let old=r#"{"version":1,"tabs":[{"url":"scroll://host/resource?lookup#section","zoom":1.25}],"active":0}"#;
        let mut session=Session::parse(old).unwrap();assert_eq!(session.tabs[0].protocol_state,None);
        session.tabs[0].protocol_state=Some("abstract".into());
        assert_eq!(Session::parse(&serde_json::to_string(&session).unwrap()).unwrap(),session);
        for (url,state) in [("https://host/resource","abstract"),("gemini://host/resource","abstract"),("scroll://host/resource","unknown")] {
            let invalid=serde_json::json!({"version":1,"tabs":[{"url":url,"zoom":1.,"protocol_state":state}],"active":0});
            assert!(Session::parse(&invalid.to_string()).is_err());
        }
    }
    #[test]
    fn dillo_sections_unicode_entities_and_roundtrip() {
        let text = ":s0: News & blogs\n:s1: 日本語\ns0 https://example.org/a?a=1&b=2 A &lt; B &amp; C\ns1 https://example.jp/ 日本語 &#x2665;\n";
        let b = Bookmarks::parse(text).unwrap();
        assert_eq!(b.entries[0].title, "A < B & C");
        assert_eq!(b.entries[1].title, "日本語 ♥");
        assert_eq!(Bookmarks::parse(&b.text()).unwrap(), b);
    }
    #[test]
    fn malformed_files_are_rejected_and_imports_are_deduplicated() {
        assert!(Bookmarks::parse(":s0: A\ns4 https://example.org X\n").is_err());
        let mut b = Bookmarks::default();
        b.add("News", "https://example.org", "Title\n:s9: Injected").unwrap();
        b.merge(b.clone()).unwrap(); assert_eq!(b.entries.len(), 1);
        assert!(!b.text().contains("\n:s9:"));
    }
    #[test]
    fn sessions_validate_active_tab_zoom_and_urls() {
        let s = Session { version: 1, active: 1, tabs: vec![SavedTab { url: "https://example.org".into(), zoom: 1.25, protocol_state:None }, SavedTab { url: "about:blank".into(), zoom: 1., protocol_state:None }] };
        let text = serde_json::to_string(&s).unwrap(); assert_eq!(Session::parse(&text).unwrap(), s);
        for url in ["javascript:alert(1)", "data:text/html,x", "https://user:password@example.org"] { assert!(!restorable(url)); }
        let mut s = s; s.active = 2; assert!(Session::parse(&serde_json::to_string(&s).unwrap()).is_err());
        assert!(Session::parse("{broken").is_err());
    }
    #[test]
    fn writes_are_atomic_private_and_leave_previous_file_on_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("dome-profile-test-{}",std::process::id()));
        fs::create_dir_all(&dir).unwrap(); let path = dir.join("bm.txt");
        write(&path, "old").unwrap(); write(&path, "new").unwrap();
        assert_eq!(read(&path).unwrap(), "new"); assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        fs::write(path.with_extension(format!("tmp-{}",std::process::id())), "busy").unwrap();
        assert!(write(&path, "bad").is_err()); assert_eq!(read(&path).unwrap(), "new");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn history_tracks_recent_unique_urls_and_excludes_credentials() {
        let mut h = History::default();
        assert!(h.visit("https://example.org/a"));
        assert!(h.visit("https://example.org/b"));
        assert!(h.visit("https://example.org/a"));
        assert!(!h.visit("https://example.org/a"));
        assert!(!h.visit("https://user:secret@example.org/"));
        assert!(!h.visit("about:blank"));
        assert_eq!(h.urls, ["https://example.org/a", "https://example.org/b"]);
    }
    #[test]
    fn native_protocols_roundtrip_across_all_profile_formats() {
        let urls=["whois://example.org/?q=Smith","rwhois://example.org/auth-area=example.org?q=domain+example.org","fsp://example.org/archive/readme.txt","titan://example.org/resource?name=Alice#part","scorpion://example.org/file.scorp?find%20text#section","scorpions://example.org:1517/file.scorp#part","gopher://example.org/0/readme%20file#part","gopher+://example.org/0/form%09%09%3F","gophers://example.org:70/0/readme%20file#part",
            "gemini://example.org/find?two%20words#part","mercury://example.org/find?two%20words#part","guppy://example.org/find?two%20words#part","dict://example.org/m:two%20words:wn:prefix:2#part","scroll://example.org/find?two%20words#1.2",
            "cso://example.org/?q=Ren%C3%A9e&field=name&return=name%2Cemail#part","ph://example.org/?q=fixture","news:comp.test","news://example.org/Case%2FId@Example.invalid#part","nntp://example.org/comp.test/7?part=1","news://example.org/comp.test?before=51","nex://example.org/readme.txt","spartan://example.org/page","finger://alice@example.org/",
            "ftp://example.org/pub/readme.txt;type=a#part"];
        let session=Session {version:1,active:3,tabs:urls.iter().enumerate().map(|(i,url)|SavedTab {url:(*url).into(),zoom:1.+i as f32/10.,protocol_state:None}).collect()};
        assert_eq!(Session::parse(&serde_json::to_string(&session).unwrap()).unwrap(),session);
        let mut bookmarks=Bookmarks::default();let mut history=History::default();
        for url in urls {
            bookmarks.add("Smallnet 世界",url,"Native <title> & text").unwrap();assert!(history.visit(url));
        }
        assert_eq!(Bookmarks::parse(&bookmarks.text()).unwrap(),bookmarks);
        let root=std::env::temp_dir().join(format!("dome-native-profile-{}",uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        session.save(&root.join("session.json")).unwrap();history.save(&root.join("hs.txt")).unwrap();bookmarks.save(&root.join("bm.txt")).unwrap();
        assert_eq!(Session::load(&root.join("session.json")).unwrap(),session);
        assert_eq!(History::load(&root.join("hs.txt")).unwrap(),history);
        assert_eq!(Bookmarks::load(&root.join("bm.txt")).unwrap(),bookmarks);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn restored_resources_keep_lookup_inputs_without_repeating_submissions() {
        for (input,expected) in [
            ("spartan://anon@example.org/submit?one%20load#part","spartan://example.org/submit#part"),
            ("ftp://fixture:one-load@example.org/pub/readme.txt","ftp://example.org/pub/readme.txt"),
            ("gopher+://example.org/0/form%09%09%2B%091%0D%0A%2B3%0D%0Aone","gopher+://example.org/0%2Fform%09%09%3F"),
            ("finger://alice@example.org/","finger://alice@example.org/"),
            ("gemini://example.org/find?two%20words","gemini://example.org/find?two%20words"),
            ("scroll://example.org/find?two%20words","scroll://example.org/find?two%20words"),
        ] {
            assert_eq!(persistent_url(input).as_deref(),Some(expected));
            let mut bookmarks=Bookmarks::default();bookmarks.add("Native",input,"Title").unwrap();assert_eq!(bookmarks.entries[0].url,expected);
            let mut history=History::default();assert!(history.visit(input));assert_eq!(history.urls,[expected]);
            let session=format!(r#"{{"version":1,"active":0,"tabs":[{{"url":"{input}","zoom":1.0}}]}}"#);
            assert_eq!(Session::parse(&session).unwrap().tabs[0].url,expected);
        }
        for url in ["dome-smallnet-input:temporary","mailto:person@example.org","magnet:?xt=fixture","gopher://user@example.org/", "nex://example.org/?input", "finger://alice:password@example.org/", "scroll://example.org/file;parameters", "gemini://example.org:0/"] {
            assert!(!restorable(url));
        }
    }
}
