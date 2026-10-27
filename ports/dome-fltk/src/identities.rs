// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared PEM identities. Existing application stores remain authoritative.
use rustls::pki_types::{CertificateDer, pem::PemObject};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs::{self, File, OpenOptions}, io::{Read, Write}, os::{fd::AsRawFd, unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt}}, path::{Path, PathBuf}, process::{Command, Stdio}, sync::atomic::{AtomicBool, Ordering}, time::{Duration, Instant}};

type Result<T> = std::result::Result<T, String>;
const STORE_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Pem { certificate: PathBuf, key: PathBuf, #[serde(default)] fingerprint: Option<String> },
    Penguin { store: PathBuf, id: String },
}
#[derive(Clone)]
pub struct Candidate { pub id: String, pub label: String, pub source: Source }
#[derive(Clone)]
pub struct Locations { pub data: PathBuf, pub config: PathBuf }
impl Locations {
    pub fn current() -> Result<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|path| path.is_absolute()).ok_or("No home directory for local identities")?;
        let location = |name, fallback| std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute()).unwrap_or(fallback);
        Ok(Self { data: location("XDG_DATA_HOME", home.join(".local/share")), config: location("XDG_CONFIG_HOME", home.join(".config")) })
    }
    fn root(&self) -> PathBuf { self.data.join("de-xr/identities") }
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry { id: String, name: String, certificate: PathBuf, key: PathBuf, #[serde(default)] address: String }
#[derive(Serialize, Deserialize)]
struct Catalog { version: u32, identities: Vec<Entry> }
impl Default for Catalog { fn default() -> Self { Self { version: 1, identities: Vec::new() } } }

fn read(path: &Path, limit: usize, private: bool) -> std::io::Result<Vec<u8>> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    let metadata = file.metadata()?;
    let forbidden = if private { 0o077 } else { 0o022 };
    if !metadata.is_file() || metadata.len() > limit as u64 || (private && metadata.uid() != unsafe { libc::getuid() }) || metadata.mode() & forbidden != 0 {
        return Err(std::io::Error::other("Invalid local identity file ownership or permissions"));
    }
    let mut bytes = Vec::new(); file.take((limit+1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit { return Err(std::io::Error::other("Local identity file exceeds its limit")); }
    Ok(bytes)
}
fn optional(path: &Path, private: bool) -> Result<Option<Vec<u8>>> {
    match read(path, STORE_LIMIT, private) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("Cannot read an existing local identity store".into()),
    }
}
fn fingerprint(certificate: &[u8]) -> Result<String> {
    let certificate = CertificateDer::from_pem_slice(certificate).map_err(|_| "Invalid certificate PEM")?;
    Ok(Sha256::digest(certificate.as_ref()).iter().map(|byte| format!("{byte:02x}")).collect())
}
fn valid_id(id: &str) -> bool { id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) }

impl Source {
    pub fn valid(&self) -> bool {
        match self {
            Self::Pem { certificate, key, fingerprint } => certificate.is_absolute() && key.is_absolute() && fingerprint.as_ref().is_none_or(|id| valid_id(id)),
            Self::Penguin { store, id } => store.is_absolute() && valid_id(id),
        }
    }
    pub fn load(&self) -> Result<(Vec<u8>, Vec<u8>)> {
        if !self.valid() { return Err("Invalid identity reference".into()); }
        let (certificate, key, expected) = match self {
            Self::Pem { certificate, key, fingerprint } => (read(certificate, 65536, false).map_err(|_| "Cannot read the selected certificate")?, read(key, 65536, true).map_err(|_| "Cannot read the private key")?, fingerprint.as_deref()),
            Self::Penguin { store, id } => {
                let bytes = read(store, STORE_LIMIT, true).map_err(|_| "Cannot read the Penguin Mail identity store")?;
                let state: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid Penguin Mail identity store")?;
                let item = state["identities"].as_array().and_then(|items| items.iter().find(|item| item["id"].as_str() == Some(id))).ok_or("The selected Penguin Mail identity is no longer available")?;
                (item["certificate"].as_str().ok_or("Missing certificate")?.as_bytes().to_vec(), item["key"].as_str().ok_or("Missing key")?.as_bytes().to_vec(), Some(id.as_str()))
            }
        };
        if certificate.len() > 65536 || key.len() > 65536 { return Err("Identity exceeds 64 KiB".into()); }
        if expected.is_some_and(|id| fingerprint(&certificate).as_deref() != Ok(id)) { return Err("The selected identity certificate changed".into()); }
        Ok((certificate, key))
    }
}
fn catalog(root: &Path) -> Result<Catalog> {
    let Some(bytes) = optional(&root.join("index.json"), true)? else { return Ok(Catalog::default()); };
    if bytes.len() > 1024 * 1024 { return Err("Shared identity catalog exceeds 1 MiB".into()); }
    let catalog: Catalog = serde_json::from_slice(&bytes).map_err(|_| "Invalid shared identity catalog")?;
    let mut ids = HashSet::new();
    if catalog.version != 1 || catalog.identities.len() > 256 || catalog.identities.iter().any(|item| !valid_id(&item.id) || !ids.insert(item.id.clone()) || !item.certificate.is_absolute() || !item.key.is_absolute() || item.name.trim().is_empty() || item.name.len() > 256 || item.name.chars().any(char::is_control) || item.address.len() > 320 || item.address.chars().any(char::is_control)) { return Err("Invalid shared identity catalog".into()); }
    Ok(catalog)
}

pub fn discover(locations: &Locations) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    let store = locations.data.join("penguin-mail/misfin/state.json");
    if let Some(bytes) = optional(&store, true)? {
        let state: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid Penguin Mail identity store")?;
        let items = state["identities"].as_array().ok_or("Invalid Penguin Mail identity list")?;
        if items.len() > 4096 { return Err("Too many identities".into()); }
        for item in items {
            let id = item["id"].as_str().ok_or("Missing identity fingerprint")?;
            if !valid_id(id) { return Err("Invalid identity fingerprint".into()); }
            let label = item["address"].as_str().unwrap_or("Mailbox identity");
            candidates.push(Candidate { id: id.into(), label: format!("Penguin Mail: {label}"), source: Source::Penguin { store: store.clone(), id: id.into() } });
        }
    }
    let editor = locations.config.join("xnedit-xr/remote.json");
    if let Some(bytes) = optional(&editor, false)? {
        let state: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid XNedit identity settings")?;
        let item = &state["identity"];
        let id = item["penguin_id"].as_str().unwrap_or("");
        if !id.is_empty() {
            let store = PathBuf::from(item["penguin_store"].as_str().ok_or("Missing Penguin Mail identity store")?);
            let source = Source::Penguin { store, id: id.into() };
            if !source.valid() { return Err("Invalid XNedit identity reference".into()); }
            candidates.push(Candidate { id: id.into(), label: "XNedit XR: Penguin Mail identity".into(), source });
        } else if let Some(path) = item["certificate"].as_str().filter(|path| !path.is_empty()) {
            let certificate = PathBuf::from(path); let key = PathBuf::from(item["key"].as_str().ok_or("Missing identity key reference")?);
            let id = fingerprint(&read(&certificate, 65536, false).map_err(|_| "Cannot read the XNedit certificate")?)?;
            let source = Source::Pem { certificate, key, fingerprint: Some(id.clone()) };
            if !source.valid() { return Err("Invalid XNedit identity reference".into()); }
            candidates.push(Candidate { id, label: "XNedit XR: PEM identity".into(), source });
        }
    }
    for entry in catalog(&locations.root())?.identities {
        candidates.push(Candidate { id: entry.id.clone(), label: format!("Shared: {}{}", entry.name, if entry.address.is_empty() { String::new() } else { format!(" ({})",entry.address) }), source: Source::Pem { certificate: entry.certificate, key: entry.key, fingerprint: Some(entry.id) } });
    }
    let mut seen = HashSet::new(); candidates.retain(|item| seen.insert(item.id.clone()));
    Ok(candidates)
}

struct Lock(File);
impl Drop for Lock { fn drop(&mut self) { unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN); } } }
fn lock(root: &Path, cancelled: &AtomicBool) -> Result<Lock> {
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(root.join("index.lock")).map_err(|_| "Cannot lock shared identities")?;
    let metadata = file.metadata().map_err(|_| "Cannot inspect identity lock")?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::getuid() } || metadata.mode() & 0o077 != 0 { return Err("Invalid shared identity lock".into()); }
    let deadline = Instant::now()+Duration::from_secs(30);
    loop {
        if cancelled.load(Ordering::Acquire) { return Err("Shared identity update canceled".into()); }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 { return Ok(Lock(file)); }
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline { return Err("Shared identity update canceled or busy".into()); }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(path).map_err(|_| "Cannot create private identity directory")?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "Cannot inspect identity directory")?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::getuid() } || metadata.mode() & 0o077 != 0 { return Err("Shared identity directory must be private and owned by you".into()); }
    Ok(())
}
fn subject_value(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() { if matches!(ch, '\\' | '/' | '+' | '=' | '#') { escaped.push('\\'); } escaped.push(ch); }
    escaped
}
fn address(value: &str) -> Result<(String, String)> {
    let (mailbox, host) = value.trim().split_once('@').ok_or("Enter a mailbox@hostname address")?;
    if mailbox.is_empty() || mailbox.len() > 64 || !mailbox.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte)) { return Err("Invalid mailbox name".into()); }
    let host = match url::Host::parse(host).map_err(|_| "Invalid identity hostname")? { url::Host::Domain(host) => host.to_ascii_lowercase(), _ => return Err("Use a DNS hostname for this identity".into()) };
    if host.len() > 253 || host.split('.').any(|label| label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') || !label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')) { return Err("Invalid identity hostname".into()); }
    Ok((mailbox.into(), host))
}

pub fn create(locations: &Locations, name: &str, mailbox: &str, cancelled: &AtomicBool) -> Result<Candidate> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) { return Err("Enter an identity name of 1 to 64 characters".into()); }
    let (mailbox, host) = address(mailbox)?;
    let root = locations.root(); private_dir(&root)?;
    { let _lock = lock(&root, cancelled)?; let _ = catalog(&root)?; }
    let temporary = root.join(format!(".pending-{}",uuid::Uuid::new_v4())); private_dir(&temporary)?;
    let result = (|| {
        if cancelled.load(Ordering::Acquire) { return Err("Identity creation canceled".into()); }
        let certificate = temporary.join("certificate.pem"); let key = temporary.join("key.pem");
        let subject = format!("/UID={}/CN={}",subject_value(&mailbox),subject_value(name));
        let serial = format!("0x{}",uuid::Uuid::new_v4().simple());
        let mut child = Command::new("/usr/bin/openssl").args(["req","-x509","-newkey","rsa:2048","-nodes","-utf8","-sha256","-days","1825","-subj",&subject,"-set_serial",&serial,"-addext",&format!("subjectAltName=DNS:{host}"),"-addext","basicConstraints=critical,CA:FALSE","-addext","keyUsage=critical,digitalSignature,keyEncipherment","-addext","extendedKeyUsage=clientAuth","-out"]).arg(&certificate).arg("-keyout").arg(&key).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|_| "OpenSSL is required to create a TLS identity")?;
        let deadline = Instant::now()+Duration::from_secs(30);
        loop {
            if let Some(status) = child.try_wait().map_err(|_| "Cannot wait for identity creation")? { if !status.success() { return Err("Cannot create the TLS identity".into()); } break; }
            if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline { let _ = child.kill(); let _ = child.wait(); return Err("Identity creation canceled or timed out".into()); }
            std::thread::sleep(Duration::from_millis(20));
        }
        fs::set_permissions(&certificate,fs::Permissions::from_mode(0o600)).map_err(|_| "Cannot protect certificate")?;
        fs::set_permissions(&key,fs::Permissions::from_mode(0o600)).map_err(|_| "Cannot protect private key")?;
        let id = fingerprint(&read(&certificate,65536,true).map_err(|_| "Cannot read new certificate")?)?;
        if cancelled.load(Ordering::Acquire) { return Err("Identity creation canceled".into()); }
        let destination = root.join(&id);
        let _lock = lock(&root,cancelled)?;
        let mut index = catalog(&root)?;
        if index.identities.len() >= 256 { return Err("Shared identity catalog is full".into()); }
        if destination.exists() || index.identities.iter().any(|entry| entry.id == id) { return Err("This shared identity already exists".into()); }
        fs::rename(&temporary,&destination).map_err(|_| "Cannot publish identity files")?;
        let certificate = destination.join("certificate.pem"); let key = destination.join("key.pem");
        let entry = Entry { id: id.clone(), name: name.into(), address: format!("{mailbox}@{host}"), certificate: certificate.clone(), key: key.clone() };
        index.identities.push(entry);
        let json = serde_json::to_vec_pretty(&index).map_err(|_| "Cannot serialize identity catalog")?;
        if json.len() > 1024 * 1024 { return Err("Shared identity catalog exceeds 1 MiB".into()); }
        let stage = root.join(format!(".index-{}.tmp",uuid::Uuid::new_v4()));
        let written = (|| {
            let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&stage).map_err(|_| "Cannot save identity catalog")?;
            file.write_all(&json).map_err(|_| "Cannot write identity catalog")?;file.sync_all().map_err(|_| "Cannot sync identity catalog")?;
            fs::rename(&stage,root.join("index.json")).map_err(|_| "Cannot publish identity catalog")?;
            File::open(&root).and_then(|dir|dir.sync_all()).map_err(|_| "Cannot sync identity directory")
        })();
        if written.is_err() { let _ = fs::remove_file(&stage); } written?;
        Ok(Candidate { id: id.clone(), label: format!("Shared: {name}"), source: Source::Pem { certificate,key,fingerprint:Some(id) } })
    })();
    if temporary.exists() { let _ = fs::remove_dir_all(&temporary); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use x509_cert::der::Decode;
    #[test]
    fn mailbox_names_and_subject_values_remain_data() {
        assert_eq!(address("user+tag@EXAMPLE.COM").unwrap(), ("user+tag".into(),"example.com".into()));
        for value in ["missing", "bad/name@host", "user@127.0.0.1", "user@host/other"] { assert!(address(value).is_err()); }
        assert_eq!(subject_value("A/B+CN=C"), "A\\/B\\+CN\\=C");
    }
    #[test]
    fn shared_creation_and_existing_application_precedence_preserve_the_same_pem() {
        let root = std::env::temp_dir().join(format!("dome-identities-{}",uuid::Uuid::new_v4()));
        let locations = Locations { data:root.join("data"),config:root.join("config") };
        let candidate = create(&locations,"Fixture / identity","fixture@localhost",&AtomicBool::new(false)).unwrap();
        let (certificate,key) = candidate.source.load().unwrap();
        assert_eq!(fingerprint(&certificate).unwrap(),candidate.id);assert!(!key.is_empty());
        let found = discover(&locations).unwrap();assert_eq!(found.len(),1);assert_eq!(found[0].id,candidate.id);
        let store = locations.data.join("penguin-mail/misfin/state.json");private_dir(store.parent().unwrap()).unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({"identities":[{"id":candidate.id,"address":"fixture@localhost","certificate":String::from_utf8(certificate.clone()).unwrap(),"key":String::from_utf8(key.clone()).unwrap()}]})).unwrap();
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&store).unwrap();file.write_all(&bytes).unwrap();drop(file);
        let found = discover(&locations).unwrap();assert_eq!(found.len(),1);assert!(found[0].label.starts_with("Penguin Mail:"));assert_eq!(found[0].source.load().unwrap(),(certificate.clone(),key.clone()));
        assert_eq!(fs::read(&store).unwrap(),bytes);
        fs::remove_file(&store).unwrap();
        let editor = locations.config.join("xnedit-xr/remote.json");private_dir(editor.parent().unwrap()).unwrap();
        let Source::Pem { certificate:cert_path,key:key_path,.. } = &candidate.source else { panic!("Expected generated PEM files") };
        let editor_bytes = serde_json::to_vec(&serde_json::json!({"identity":{"certificate":cert_path,"key":key_path,"penguin_store":"","penguin_id":""}})).unwrap();
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&editor).unwrap();file.write_all(&editor_bytes).unwrap();drop(file);
        let found = discover(&locations).unwrap();assert_eq!(found.len(),1);assert!(found[0].label.starts_with("XNedit XR:"));assert_eq!(found[0].source.load().unwrap(),(certificate.clone(),key.clone()));assert_eq!(fs::read(&editor).unwrap(),editor_bytes);
        for path in [locations.root(),locations.root().join(&candidate.id)] { assert_eq!(fs::metadata(path).unwrap().mode() & 0o777,0o700); }
        for path in [locations.root().join("index.json"),locations.root().join(&candidate.id).join("certificate.pem"),locations.root().join(&candidate.id).join("key.pem")] { assert_eq!(fs::metadata(path).unwrap().mode() & 0o777,0o600); }
        let pem = CertificateDer::from_pem_slice(&certificate).unwrap();let cert = x509_cert::Certificate::from_der(pem.as_ref()).unwrap();
        assert!(cert.tbs_certificate.subject.0.iter().flat_map(|rdn|rdn.0.iter()).any(|value|value.oid.to_string()=="0.9.2342.19200300.100.1.1"));
        assert!(cert.tbs_certificate.extensions.unwrap().iter().any(|value|value.extn_id.to_string()=="2.5.29.17"));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cancellation_does_not_generate_or_publish_an_identity() {
        let root = std::env::temp_dir().join(format!("dome-identities-{}",uuid::Uuid::new_v4()));
        let locations = Locations { data:root.join("data"),config:root.join("config") };
        assert!(create(&locations,"Canceled","fixture@localhost",&AtomicBool::new(true)).is_err());
        assert!(!locations.root().join("index.json").exists());fs::remove_dir_all(root).unwrap();
    }
}
