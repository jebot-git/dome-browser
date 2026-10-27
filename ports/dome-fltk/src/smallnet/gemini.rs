// SPDX-License-Identifier: GPL-3.0-or-later
//! Gemini transport, TLS certificate pins and explicitly selected identities.
use super::{Error, MAX_BYTES, normalize_address};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme, client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier}, pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::OpenOptions, io::Read, os::unix::fs::{MetadataExt, OpenOptionsExt}, path::{Path, PathBuf}, sync::{Arc, Mutex}};
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader}, net::TcpStream};
use tokio_rustls::{TlsConnector, client::TlsStream};
use url::Url;
use x509_cert::der::Decode;

const MAX_HEADER: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Pin { pub fingerprint: String, pub not_before: u64, pub expires: u64 }
#[derive(Clone, Debug)]
pub(super) struct Certificate { pub pin: Pin, pub subject: String, pub validity: String }

impl Certificate {
    fn parse(bytes: &[u8], now: u64) -> Result<Self, rustls::Error> {
        use rustls::CertificateError;
        if bytes.len() > 128 * 1024 { return Err(CertificateError::BadEncoding.into()); }
        let cert = x509_cert::Certificate::from_der(bytes).map_err(|_| CertificateError::BadEncoding)?;
        let validity = cert.tbs_certificate.validity;
        let not_before = validity.not_before.to_unix_duration().as_secs();
        let expires = validity.not_after.to_unix_duration().as_secs();
        if now < not_before { return Err(CertificateError::NotValidYet.into()); }
        if now > expires { return Err(CertificateError::Expired.into()); }
        let fingerprint = Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self { pin: Pin { fingerprint, not_before, expires }, subject: cert.tbs_certificate.subject.to_string(),
            validity: format!("{} — {}", validity.not_before.to_date_time(), validity.not_after.to_date_time()) })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Binding {
    pub authority: String, pub path: String, pub server_pin: String,
    pub identity: crate::identities::Source,
}

impl Binding {
    pub(super) fn matches(&self, url: &Url, pin: &Pin) -> bool {
        self.authority == authority(url) && self.server_pin == pin.fingerprint &&
            (url.path() == self.path || if self.path.ends_with('/') { url.path().starts_with(&self.path) }
             else { url.path().strip_prefix(&self.path).is_some_and(|rest| rest.starts_with('/')) })
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Records { version: u32, pins: BTreeMap<String, Pin>, identities: Vec<Binding> }
impl Default for Records { fn default() -> Self { Self { version: 1, pins: BTreeMap::new(), identities: Vec::new() } } }

pub(super) struct Store { path: Option<PathBuf>, memory: Mutex<Records> }
impl Store {
    pub(super) fn new(path: Option<PathBuf>) -> Self { Self { path, memory: Mutex::new(Records::default()) } }
    fn records(&self) -> Result<std::sync::MutexGuard<'_, Records>, Error> {
        let mut records = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = &self.path {
            *records = match read_private(path) {
                Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| Error::TrustStore)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Records::default(),
                Err(_) => return Err(Error::TrustStore),
            };
        }
        if records.version != 1 || records.pins.len() > 4096 || records.identities.len() > 256 || records.pins.values().any(|pin| pin.fingerprint.len() != 64 || !pin.fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) || pin.not_before > pin.expires) {
            return Err(Error::TrustStore);
        }
        if records.identities.iter().any(|item| !item.identity.valid() || !item.path.starts_with('/') || item.server_pin.len() != 64) { return Err(Error::TrustStore); }
        Ok(records)
    }
    fn save(&self, records: &Records) -> Result<(), Error> {
        if let Some(path) = &self.path {
            let json = serde_json::to_string_pretty(records).map_err(|_| Error::TrustStore)?;
            crate::storage::write(path, &(json + "\n")).map_err(|_| Error::TrustStore)?;
        }
        Ok(())
    }
    pub(super) fn pin(&self, url: &Url) -> Result<Option<Pin>, Error> { Ok(self.records()?.pins.get(&authority(url)).cloned()) }
    pub(super) fn accept(&self, url: &Url, pin: Pin, previous: Option<&Pin>) -> Result<bool, Error> {
        let mut records = self.records()?;
        let current = records.pins.get(&authority(url));
        if current.is_some_and(|current| current == &pin) { return Ok(true); }
        if current != previous { return Ok(false); }
        if current.is_none() && records.pins.len() >= 4096 { return Err(Error::TrustStore); }
        records.pins.insert(authority(url), pin); self.save(&records)?; Ok(true)
    }
    pub(super) fn identity(&self, url: &Url, pin: &Pin) -> Result<Option<Binding>, Error> {
        Ok(self.records()?.identities.iter().filter(|item| item.matches(url, pin)).max_by_key(|item| item.path.len()).cloned())
    }
    pub(super) fn remember(&self, binding: Binding) -> Result<(), Error> {
        let mut records = self.records()?;
        records.identities.retain(|item| item.authority != binding.authority || item.path != binding.path);
        if records.identities.len() >= 256 { return Err(Error::TrustStore); }
        records.identities.push(binding); self.save(&records)
    }
}

fn read_private(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::getuid() } || metadata.mode() & 0o077 != 0 || metadata.len() > 4 * 1024 * 1024 {
        return Err(std::io::Error::other("Invalid private Gemini settings"));
    }
    let mut bytes = Vec::new(); file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 { return Err(std::io::Error::other("Gemini settings too large")); }
    Ok(bytes)
}

pub(super) fn tls_port(url: &Url) -> u16 { url.port().unwrap_or(match url.scheme() {"scroll"=>5699,"gophers"=>70,"scorpions"|"scorpion"=>1517,_=>1965}) }
pub(super) fn authority(url: &Url) -> String { format!("{}://{}:{}", url.scheme(), url.host_str().unwrap_or(""), tls_port(url)) }

#[derive(Debug)]
struct Verifier {
    expected: Option<Pin>, observed: Arc<Mutex<Option<Certificate>>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}
impl ServerCertVerifier for Verifier {
    fn verify_server_cert(&self, cert: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        let cert = Certificate::parse(cert.as_ref(), now.as_secs())?;
        *self.observed.lock().unwrap_or_else(|e| e.into_inner()) = Some(cert.clone());
        if self.expected.as_ref().is_some_and(|pin| pin.fingerprint != cert.pin.fingerprint) { return Err(rustls::CertificateError::ApplicationVerificationFailure.into()); }
        // A first-use certificate remains provisional until the TLS handshake
        // signature is verified and it is pinned before any Gemini request.
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> { self.provider.signature_verification_algorithms.supported_schemes() }
}

pub(super) struct Identity { certificates: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static> }
impl Identity {
    fn validate_scorpion_chain(&self) -> Result<(), Error> {
        use x509_cert::der::{AnyRef, Encode};
        // Scorpion requires a complete, leaf-first chain. Validate the supplied
        // chain before a connection can disclose it. A self-signed identity is
        // also its own root; it need not be a WebPKI CA certificate.
        let parsed = self.certificates.iter().map(|der| x509_cert::Certificate::from_der(der.as_ref()))
            .collect::<Result<Vec<_>, _>>().map_err(|_| Error::Certificate)?;
        if parsed.is_empty() { return Err(Error::Certificate); }
        let provider = rustls::crypto::aws_lc_rs::default_provider();
        for (index, certificate) in parsed.iter().enumerate() {
            let issuer = parsed.get(index + 1).unwrap_or(certificate);
            if certificate.tbs_certificate.issuer != issuer.tbs_certificate.subject ||
                certificate.signature_algorithm != certificate.tbs_certificate.signature ||
                parsed[..index].contains(certificate) {
                return Err(Error::Certificate);
            }
            let algorithm = certificate.signature_algorithm.to_der().map_err(|_| Error::Certificate)?;
            let key_algorithm = issuer.tbs_certificate.subject_public_key_info.algorithm.to_der().map_err(|_| Error::Certificate)?;
            let algorithm = AnyRef::from_der(&algorithm).map_err(|_| Error::Certificate)?;
            let key_algorithm = AnyRef::from_der(&key_algorithm).map_err(|_| Error::Certificate)?;
            let key = issuer.tbs_certificate.subject_public_key_info.subject_public_key.as_bytes().ok_or(Error::Certificate)?;
            let signature = certificate.signature.as_bytes().ok_or(Error::Certificate)?;
            let message = certificate.tbs_certificate.to_der().map_err(|_| Error::Certificate)?;
            if !provider.signature_verification_algorithms.all.iter().any(|verifier|
                verifier.signature_alg_id().as_ref() == algorithm.value() &&
                verifier.public_key_alg_id().as_ref() == key_algorithm.value() &&
                verifier.verify_signature(key, &message, signature).is_ok()) {
                return Err(Error::Certificate);
            }
        }
        Ok(())
    }
    pub(super) fn fingerprint(&self) -> String {
        Sha256::digest(self.certificates[0].as_ref()).iter().map(|byte| format!("{byte:02x}")).collect()
    }
    pub(super) fn load(binding: &Binding) -> Result<Self, Error> {
        let (certificates, key) = binding.identity.load().map_err(|_| Error::Certificate)?;
        if certificates.len() > 1024 * 1024 || key.len() > 1024 * 1024 { return Err(Error::TooLarge); }
        let certificates: Vec<_> = CertificateDer::pem_slice_iter(&certificates).collect::<Result<_, _>>().map_err(|_| Error::Certificate)?;
        if certificates.is_empty() { return Err(Error::Certificate); }
        for cert in &certificates { Certificate::parse(cert.as_ref(), UnixTime::now().as_secs()).map_err(|_| Error::Certificate)?; }
        let key = PrivateKeyDer::from_pem_slice(&key).map_err(|_| Error::Certificate)?;
        rustls::sign::CertifiedKey::from_der(certificates.clone(), key.clone_key(), &rustls::crypto::aws_lc_rs::default_provider()).map_err(|_| Error::Certificate)?;
        Ok(Self { certificates, key })
    }
}

pub(super) enum Connection { Ready(Session), Changed }
pub(super) struct Session { stream: TlsStream<TcpStream>, pub certificate: Certificate }

pub(super) async fn connect(url: &Url, expected: Option<Pin>, identity: Option<Identity>) -> Result<Connection, Error> {
    match url.scheme() {
        "gemini"=>{request_bytes(url)?;},
        "titan"=>{uri_request_bytes(url,"titan",1022)?;},
        "scorpions"=>{super::scorpion::request_bytes(url)?;if let Some(identity)=&identity {identity.validate_scorpion_chain()?;}},
        "scroll"=>{super::scroll::request_bytes(url,&super::scroll::languages(),false)?;},
        "gophers"=>{if identity.is_some(){return Err(Error::Certificate);} super::gopher::Request::parse(url)?;},
        _=>return Err(Error::Address),
    }
    if identity.is_some() && expected.is_none() { return Err(Error::Certificate); }
    let observed = Arc::new(Mutex::new(None));
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = Arc::new(Verifier { expected: expected.clone(), observed: observed.clone(), provider: provider.clone() });
    let config = ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().map_err(|_| Error::Certificate)?
        .dangerous().with_custom_certificate_verifier(verifier);
    let mut config = if let Some(identity) = identity { config.with_client_auth_cert(identity.certificates, identity.key).map_err(|_| Error::Certificate)? }
        else { config.with_no_client_auth() };
    config.resumption = rustls::client::Resumption::disabled();
    let host = url.host_str().ok_or(Error::Address)?.trim_matches(['[', ']']);
    let name = ServerName::try_from(host.to_owned()).map_err(|_| Error::Address)?;
    let stream = TcpStream::connect((host, tls_port(url))).await.map_err(|_| Error::Transport)?;
    let connected = TlsConnector::from(Arc::new(config)).connect(name, stream).await;
    let cert = observed.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match connected {
        Ok(stream) => Ok(Connection::Ready(Session { stream, certificate: cert.ok_or(Error::Certificate)? })),
        Err(_) if cert.as_ref().is_some_and(|cert| expected.as_ref().is_some_and(|pin| pin.fingerprint != cert.pin.fingerprint)) => Ok(Connection::Changed),
        Err(_) => Err(Error::Certificate),
    }
}

pub(super) fn request_bytes(url: &Url) -> Result<Vec<u8>, Error> {
    uri_request_bytes(url, "gemini", 1024)
}

pub(super) fn uri_request_bytes(url: &Url, scheme: &str, limit: usize) -> Result<Vec<u8>, Error> {
    if url.scheme() != scheme || !url.username().is_empty() || url.password().is_some() { return Err(Error::Address); }
    let mut url = url.clone(); normalize_address(&mut url)?;
    url.set_fragment(None);
    if url.path().is_empty() { url.set_path("/"); }
    if url.as_str().len() > limit || !url.as_str().is_ascii() || url.as_str().bytes().any(|byte| byte <= 32 || byte == 127) { return Err(Error::Address); }
    let mut bytes = url.to_string().into_bytes(); bytes.extend_from_slice(b"\r\n"); Ok(bytes)
}

pub(super) enum Reply { Success(String, Vec<u8>), ScrollSuccess {mime:String,bytes:Vec<u8>,metadata:super::scroll::Metadata}, Stream(super::transfer::Transfer), Input { sensitive: bool, prompt: String }, Redirect(Url), Failure(u8, String), Identity(u8, String) }

pub(super) fn header(bytes: &[u8], url: &Url) -> Result<Reply, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Response)?;
    let text = text.strip_suffix("\r\n").ok_or(Error::Response)?;
    if text.len() < 2 || !text.as_bytes()[0].is_ascii_digit() || !text.as_bytes()[1].is_ascii_digit() { return Err(Error::Response); }
    let status = (text.as_bytes()[0]-b'0')*10 + text.as_bytes()[1]-b'0';
    if !(10..=69).contains(&status) { return Err(Error::Response); }
    let meta = if text.len() == 2 { "" } else { text[2..].strip_prefix(' ').ok_or(Error::Response)? };
    if meta.chars().any(char::is_control) { return Err(Error::Response); }
    match status / 10 {
        1 if !meta.is_empty() || url.scheme()=="scroll" => Ok(Reply::Input { sensitive: status == 11, prompt: meta.into() }),
        2 => {
            let mime: mime::Mime = meta.parse().map_err(|_| Error::Response)?;
            Ok(Reply::Success(mime.to_string(), Vec::new()))
        }
        3 if !meta.is_empty() => {
            let mut base = url.clone(); base.set_query(None);
            let target = base.join(meta).map_err(|_| Error::Response)?;
            if !matches!(target.scheme(), "titan" | "scorpion" | "scorpions" | "gemini" | "mercury" | "guppy" | "dict" | "cso" | "ph" | "news" | "nntp" | "gopher" | "gopher+" | "gophers" | "nex" | "spartan" | "scroll" | "finger" | "ftp" | "http" | "https") || !target.username().is_empty() || target.password().is_some() { return Err(Error::Response); }
            Ok(Reply::Redirect(target))
        }
        4 | 5 => Ok(Reply::Failure(status, meta.into())),
        6 => Ok(Reply::Identity(status, meta.into())),
        _ => Err(Error::Response),
    }
}

pub(super) async fn response_header(stream: &mut (impl AsyncRead + Unpin), url: &Url) -> Result<Reply, Error> {
    let mut bytes = Vec::new();
    while bytes.len() < MAX_HEADER {
        bytes.push(stream.read_u8().await.map_err(|_| Error::Response)?);
        if bytes.ends_with(b"\r\n") { break; }
    }
    if !bytes.ends_with(b"\r\n") { return Err(Error::TooLarge); }
    header(&bytes, url)
}

pub(super) async fn complete_response(reply:Reply,stream:&mut(impl AsyncRead+Unpin))->Result<Reply,Error> {
    let mut reply=reply;
    let body=match &mut reply {Reply::Success(_,bytes)|Reply::ScrollSuccess {bytes,..}=>bytes,_=>return Ok(reply)};
    stream.take((MAX_BYTES+1)as u64).read_to_end(body).await.map_err(|_|Error::Transport)?;
    if body.len()>MAX_BYTES{return Err(Error::TooLarge);}
    Ok(reply)
}

#[cfg(test)]
async fn response(stream:&mut(impl AsyncRead+Unpin),url:&Url)->Result<Reply,Error> {
    let reply=response_header(stream,url).await?;
    complete_response(reply,stream).await
}

pub(super) async fn receive<R,W>(mut reader:R,mut writer:W,url:&Url,metadata:bool,download:bool)->Result<Reply,Error>
where R:AsyncRead+Unpin+Send+'static,W:AsyncWrite+Unpin+Send+'static {
    let header=if url.scheme()=="scroll" {super::scroll::response_header(&mut reader,url).await}
        else {response_header(&mut reader,url).await};
    let result=match header {
        Ok(reply)=>{
            let mime=match &reply {Reply::Success(mime,_)|Reply::ScrollSuccess {mime,..}=>Some(mime.clone()),_=>None};
            if let Some(mime)=mime {
                let kind:mime::Mime=mime.parse().map_err(|_|Error::Response)?;
                let preview=kind.type_()==mime::TEXT || kind.type_()==mime::IMAGE || kind.essence_str()=="application/xhtml+xml" || (url.scheme()=="scroll" && metadata);
                if download || !preview {
                    // Keep the buffered reader: the TLS record containing the
                    // header can also contain the first part of the body.
                    let transfer=super::transfer::Transfer::new(mime,None,reader)
                        .with_completion(async move {writer.shutdown().await.map_err(|_|Error::Transport)});
                    return Ok(Reply::Stream(transfer));
                }
            }
            complete_response(reply,&mut reader).await
        },
        Err(error)=>Err(error),
    };
    let _=writer.shutdown().await;
    result
}

impl Session {
    pub(super) fn into_stream(self) -> TlsStream<TcpStream> { self.stream }
    pub(super) fn tls12(&self) -> bool { self.stream.get_ref().1.protocol_version() == Some(rustls::ProtocolVersion::TLSv1_2) }
    pub(super) async fn close(mut self) { let _ = self.stream.shutdown().await; }
    pub(super) async fn request(mut self, url: &Url, metadata:bool,download:bool) -> Result<Reply, Error> {
        let bytes=if url.scheme()=="scroll" {super::scroll::request_bytes(url,&super::scroll::languages(),metadata)?}else{request_bytes(url)?};
        self.stream.write_all(&bytes).await.map_err(|_| Error::Transport)?;
        let (reader,writer)=tokio::io::split(self.stream);
        receive(BufReader::new(reader),writer,url,metadata,download).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn selected_identity_dates_key_pair_and_certificate_reference_are_checked() {
        let root = std::env::temp_dir().join(format!("dome-gemini-identity-{}",uuid::Uuid::new_v4()));
        let locations = crate::identities::Locations { data: root.join("data"), config: root.join("config") };
        let canceled = std::sync::atomic::AtomicBool::new(false);
        let first = crate::identities::create(&locations,"First fixture","first@localhost",&canceled).unwrap();
        let second = crate::identities::create(&locations,"Second fixture","second@localhost",&canceled).unwrap();
        let (pem,_) = first.source.load().unwrap();
        let der = CertificateDer::from_pem_slice(&pem).unwrap();
        let cert = Certificate::parse(der.as_ref(),UnixTime::now().as_secs()).unwrap();
        assert!(Certificate::parse(der.as_ref(),cert.pin.not_before-1).is_err());
        assert!(Certificate::parse(der.as_ref(),cert.pin.expires+1).is_err());
        let mut binding = Binding { authority:"gemini://localhost:1965".into(),path:"/".into(),server_pin:"a".repeat(64),identity:first.source.clone() };
        assert_eq!(Identity::load(&binding).unwrap().fingerprint(),first.id);
        let crate::identities::Source::Pem { key: other_key,.. } = &second.source else { panic!("Expected PEM reference") };
        let crate::identities::Source::Pem { key,.. } = &mut binding.identity else { panic!("Expected PEM reference") };
        *key=other_key.clone();
        assert!(Identity::load(&binding).is_err());
        let crate::identities::Source::Pem { certificate,.. } = &binding.identity else { unreachable!() };
        fs::write(certificate,second.source.load().unwrap().0).unwrap();
        assert!(binding.identity.load().is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn requests_normalize_host_path_and_fragment_and_enforce_the_uri_limit() {
        assert_eq!(request_bytes(&Url::parse("gemini://example.com#section").unwrap()).unwrap(), b"gemini://example.com/\r\n");
        assert_eq!(request_bytes(&Url::parse("gemini://example.com/search?two%20words#section").unwrap()).unwrap(), b"gemini://example.com/search?two%20words\r\n");
        assert!(request_bytes(&Url::parse("gemini://user@example.com/").unwrap()).is_err());
        assert!(request_bytes(&Url::parse(&format!("gemini://example.com/{}", "x".repeat(1024))).unwrap()).is_err());
    }
    #[test]
    fn status_groups_and_redirects_follow_gemini_rules() {
        let url = Url::parse("gemini://host/old?private").unwrap();
        assert!(matches!(header(b"11 Password\r\n", &url), Ok(Reply::Input { sensitive: true, .. })));
        assert!(matches!(header(b"14 Input\r\n", &url), Ok(Reply::Input { sensitive: false, .. })));
        assert!(matches!(header(b"50\r\n", &url), Ok(Reply::Failure(50, _))));
        assert!(matches!(header(b"60 Identity needed\r\n", &url), Ok(Reply::Identity(60, _))));
        match header(b"30 /new\r\n", &url).unwrap() { Reply::Redirect(url) => assert_eq!(url.as_str(), "gemini://host/new"), _ => panic!("Expected redirect") }
        for bytes in [b"09 Invalid\r\n".as_slice(), b"70 Invalid\r\n", b"20 text/plain\n", b"20 text/plain\0\r\n", b"30 \r\n", b"30 file:///tmp/item\r\n", b"10 \r\n"] { assert!(header(bytes, &url).is_err()); }
    }
    #[test]
    fn scorpion_identity_chains_are_complete_ordered_and_signed() {
        use std::{fs, process::Command};
        use std::os::unix::fs::PermissionsExt;
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {fn drop(&mut self) {let _=fs::remove_dir_all(&self.0);}}
        let directory=std::env::temp_dir().join(format!("scorpion-chain-{}-{}",std::process::id(),UnixTime::now().as_secs()));
        fs::create_dir(&directory).unwrap();fs::set_permissions(&directory,fs::Permissions::from_mode(0o700)).unwrap();let fixture=Fixture(directory);
        let run=|args:&[&str]| {let output=Command::new("openssl").args(args).current_dir(&fixture.0).output().unwrap();assert!(output.status.success(),"Fixture certificate generation failed: {}",String::from_utf8_lossy(&output.stderr));};
        run(&["req","-x509","-newkey","rsa:2048","-nodes","-days","2","-subj","/CN=Fixture root","-addext","basicConstraints=critical,CA:TRUE","-keyout","root.key","-out","root.pem"]);
        run(&["req","-new","-newkey","rsa:2048","-nodes","-subj","/CN=Fixture identity","-keyout","client.key","-out","client.csr"]);
        fs::write(fixture.0.join("client.ext"),"basicConstraints=critical,CA:FALSE\nextendedKeyUsage=clientAuth\nkeyUsage=critical,digitalSignature\n").unwrap();
        run(&["x509","-req","-in","client.csr","-CA","root.pem","-CAkey","root.key","-set_serial","2","-days","2","-extfile","client.ext","-out","client.pem"]);
        run(&["x509","-req","-in","client.csr","-signkey","client.key","-set_serial","3","-days","2","-extfile","client.ext","-out","self.pem"]);
        let read=|name| CertificateDer::from_pem_slice(&fs::read(fixture.0.join(name)).unwrap()).unwrap().into_owned();
        let root=read("root.pem");let leaf=read("client.pem");let standalone=read("self.pem");
        let key=PrivateKeyDer::from_pem_slice(&fs::read(fixture.0.join("client.key")).unwrap()).unwrap();
        let validate=|certificates| Identity {certificates,key:key.clone_key()}.validate_scorpion_chain();
        assert!(validate(vec![standalone]).is_ok());
        assert!(validate(vec![root.clone()]).is_ok());
        assert!(validate(vec![leaf.clone(),root.clone()]).is_ok());
        assert!(validate(vec![leaf.clone()]).is_err());
        assert!(validate(vec![root.clone(),leaf.clone()]).is_err());
        assert!(validate(vec![leaf.clone(),root.clone(),root.clone()]).is_err());
        let mut changed=leaf.as_ref().to_vec();*changed.last_mut().unwrap()^=1;
        assert!(validate(vec![CertificateDer::from(changed),root]).is_err());
    }

    #[test]
    fn identity_scope_matches_exact_authority_path_segments_and_server_pin() {
        assert_eq!(authority(&Url::parse("gophers://host/").unwrap()),"gophers://host:70");
        assert_ne!(authority(&Url::parse("gophers://host/").unwrap()),authority(&Url::parse("gemini://host:70/").unwrap()));
        let pin = Pin { fingerprint: "a".repeat(64), not_before: 0, expires: u64::MAX };
        let binding = Binding { authority: "gemini://host:1965".into(), path: "/private".into(), server_pin: pin.fingerprint.clone(), identity: crate::identities::Source::Pem { certificate: "/tmp/public.pem".into(), key: "/tmp/local.key".into(), fingerprint: None } };
        for url in ["gemini://host/private", "gemini://host/private/item"] { assert!(binding.matches(&Url::parse(url).unwrap(), &pin)); }
        for url in ["gemini://other/private", "gemini://host:2000/private", "gemini://host/", "gemini://host/private-other"] { assert!(!binding.matches(&Url::parse(url).unwrap(), &pin)); }
        assert!(!binding.matches(&Url::parse("gemini://host/private").unwrap(), &Pin { fingerprint: "b".repeat(64), ..pin }));
    }
    #[test]
    fn scroll_and_gemini_keep_independent_certificate_and_identity_scopes() {
        let g=Url::parse("gemini://host:1965/private").unwrap();let s=Url::parse("scroll://host:1965/private").unwrap();
        let pin=Pin {fingerprint:"a".repeat(64),not_before:1,expires:u64::MAX};let store=Store::new(None);
        assert!(store.accept(&g,pin.clone(),None).unwrap());assert!(store.pin(&s).unwrap().is_none());
        store.remember(Binding {authority:authority(&g),path:"/private".into(),server_pin:pin.fingerprint.clone(),identity:crate::identities::Source::Pem {certificate:"/tmp/cert.pem".into(),key:"/tmp/key.pem".into(),fingerprint:None}}).unwrap();
        assert!(store.identity(&s,&pin).unwrap().is_none());
        assert!(store.accept(&s,pin.clone(),None).unwrap());assert_eq!(store.pin(&g).unwrap(),Some(pin));
    }

    #[test]
    fn private_pin_store_is_atomic_and_compares_before_replacing_trust() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("dome-gemini-{}", uuid::Uuid::new_v4())); fs::create_dir(&dir).unwrap();
        let path = dir.join("gemini-trust.json"); let store = Store::new(Some(path.clone())); let url = Url::parse("gemini://host/").unwrap();
        let pin = Pin { fingerprint: "a".repeat(64), not_before: 1, expires: u64::MAX };
        assert!(store.accept(&url, pin.clone(), None).unwrap());
        assert_eq!(store.pin(&url).unwrap(), Some(pin.clone()));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let next = Pin { fingerprint: "b".repeat(64), ..pin.clone() };
        assert!(!store.accept(&url, next.clone(), None).unwrap());
        assert!(store.accept(&url, next.clone(), Some(&pin)).unwrap());
        assert_eq!(store.pin(&url).unwrap(), Some(next));
        fs::write(&path, b"malformed local settings").unwrap(); assert!(store.pin(&url).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"malformed local settings"); fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn response_status_and_binary_body_do_not_require_text_decoding() {
        let url = Url::parse("gemini://host/").unwrap();
        match response(&mut b"20 application/octet-stream\r\n\0binary\r\n.\r\n".as_slice(), &url).await.unwrap() {
            Reply::Success(mime, bytes) => { assert_eq!(mime, "application/octet-stream"); assert_eq!(bytes, b"\0binary\r\n.\r\n"); }, _ => panic!("Expected binary content")
        }
    }
}
