// SPDX-License-Identifier: GPL-3.0-or-later
use std::{fs::{File, OpenOptions}, io::{self, Write}, os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf}, sync::mpsc::{self, Receiver, Sender}};
use servo::{ResourceDownloadEvent, WebView};
use url::Url;

pub enum Completion {
    Saved(PathBuf),
    Image(image::RgbaImage),
    Error(String),
}
pub struct Downloads {
    sender: Sender<Completion>,
    pub receiver: Receiver<Completion>,
}
impl Downloads {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self { sender, receiver }
    }
    pub fn start(&self, view: &WebView, url: Url, copy_image: bool, context: Option<servo::EmbedderControlId>) {
        let sender = self.sender.clone();
        let directory = if copy_image { None } else {
            match downloads_directory() {
                Ok(dir) => Some(dir),
                Err(e) => { let _ = sender.send(Completion::Error(e.to_string())); return; }
            }
        };
        let mut file: Option<PendingFile> = None;
        let mut bytes = Vec::new();
        let result = view.download_resource_in_context(url, context, move |event| {
            let result: Result<(), String> = (|| {
                match event {
                    ResourceDownloadEvent::Response { url, content_type, content_disposition } => {
                        if let Some(dir) = &directory {
                            file = Some(PendingFile::new(dir, &filename(&url, &content_disposition, &content_type))
                                .map_err(|e| e.to_string())?);
                        }
                    },
                    ResourceDownloadEvent::Data(chunk) => {
                        if copy_image {
                            if bytes.len().saturating_add(chunk.len()) > 64 * 1024 * 1024 {
                                return Err("Image is too large to copy (64 MiB limit)".into());
                            }
                            bytes.extend_from_slice(&chunk);
                        } else {
                            file.as_mut().ok_or("Download has no response")?.file.write_all(&chunk)
                                .map_err(|e| e.to_string())?;
                        }
                    },
                    ResourceDownloadEvent::Finished(result) => {
                        result?;
                        let completion = if copy_image {
                            let mut reader = image::ImageReader::new(io::Cursor::new(&bytes))
                                .with_guessed_format().map_err(|e| e.to_string())?;
                            let mut limits = image::Limits::default();
                            limits.max_image_width = Some(16384);
                            limits.max_image_height = Some(16384);
                            limits.max_alloc = Some(128 * 1024 * 1024);
                            reader.limits(limits);
                            Completion::Image(reader.decode().map_err(|e| format!("Cannot copy image: {e}"))?.into_rgba8())
                        } else {
                            Completion::Saved(file.take().ok_or("Download has no response")?.finish().map_err(|e| e.to_string())?)
                        };
                        let _ = sender.send(completion);
                    },
                }
                Ok(())
            })();
            if let Err(error) = result {
                file.take();
                let _ = sender.send(Completion::Error(error));
                false
            } else { true }
        });
        if let Err(error) = result { let _ = self.sender.send(Completion::Error(error)); }
    }
}

// Parse the XDG assignment as data, never source it as a shell script.
fn configured_directory(text: &str, home: &Path) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let value = line.trim().strip_prefix("XDG_DOWNLOAD_DIR=")?.trim();
        let value = value.strip_prefix('"')?.strip_suffix('"')?;
        let mut decoded = String::new();
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            if c == '\\' { decoded.push(chars.next()?); } else { decoded.push(c); }
        }
        let path = if decoded == "$HOME" || decoded == "${HOME}" { home.to_path_buf() }
            else if let Some(rest) = decoded.strip_prefix("$HOME/").or_else(|| decoded.strip_prefix("${HOME}/")) { home.join(rest) }
            else { PathBuf::from(decoded) };
        path.is_absolute().then_some(path)
    })
}
fn downloads_directory() -> io::Result<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?);
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        .filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".config"));
    let path = match std::fs::read_to_string(config.join("user-dirs.dirs")) {
        Ok(text) => configured_directory(&text, &home).unwrap_or_else(|| home.join("Downloads")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => home.join("Downloads"),
        Err(e) => return Err(e),
    };
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
fn decode_percent(text: &str) -> String {
    let mut decoded = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = ((bytes[i+1] as char).to_digit(16), (bytes[i+2] as char).to_digit(16)) {
                decoded.push((hi * 16 + lo) as u8); i += 3; continue;
            }
        }
        decoded.push(bytes[i]); i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
fn filename(url: &Url, disposition: &str, mime: &str) -> String {
    let mut plain = None;
    let mut extended = None;
    for part in disposition.split(';').skip(1) {
        if let Some((key, value)) = part.trim().split_once('=') {
            let value = value.trim().trim_matches('"');
            if key.eq_ignore_ascii_case("filename") { plain = Some(value.to_owned()); }
            if key.eq_ignore_ascii_case("filename*") {
                if let Some((charset, rest)) = value.split_once('\'') {
                    if charset.eq_ignore_ascii_case("utf-8") {
                        if let Some((_, name)) = rest.split_once('\'') { extended = Some(decode_percent(name)); }
                    }
                }
            }
        }
    }
    let fallback = || crate::smallnet::download_filename(url).unwrap_or_else(||url.path_segments().and_then(|mut s| s.next_back())
        .filter(|s| !s.is_empty()).map(decode_percent).unwrap_or_default());
    let candidate = extended.or(plain).unwrap_or_else(fallback);
    let candidate = candidate.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = candidate.chars().filter(|c| !c.is_control()).take(160).collect();
    name = name.trim().trim_start_matches('.').to_owned();
    if name.is_empty() {
        name = match mime.split(';').next().unwrap_or("").trim() {
            "text/html" => "page.html", "image/png" => "image.png", "image/jpeg" => "image.jpg",
            "image/gif" => "image.gif", "image/webp" => "image.webp", _ => "download",
        }.into();
    }
    name
}
struct PendingFile { file: File, path: PathBuf, committed: bool }
impl PendingFile {
    fn new(dir: &Path, name: &str) -> io::Result<Self> {
        for index in 0..10000 {
            let path = dir.join(if index == 0 { name.to_owned() } else {
                let p = Path::new(name);
                let stem = p.file_stem().unwrap_or_default().to_string_lossy();
                match p.extension() { Some(ext) => format!("{stem} ({index}).{}", ext.to_string_lossy()), None => format!("{stem} ({index})") }
            });
            match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
                Ok(file) => return Ok(Self { file, path, committed: false }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("Too many files with the same name"))
    }
    fn finish(mut self) -> io::Result<PathBuf> {
        self.file.sync_all()?;
        self.committed = true;
        Ok(self.path.clone())
    }
}
impl Drop for PendingFile {
    fn drop(&mut self) { if !self.committed { let _ = std::fs::remove_file(&self.path); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xdg_and_untrusted_names() {
        assert_eq!(configured_directory("XDG_DOWNLOAD_DIR=\"$HOME/My Downloads\"", Path::new("/home/test")), Some(PathBuf::from("/home/test/My Downloads")));
        assert_eq!(configured_directory("XDG_DOWNLOAD_DIR=\"relative\"", Path::new("/home/test")), None);
        let url = Url::parse("https://example.org/file").unwrap();
        assert_eq!(filename(&url, "attachment; filename=\"../../secret.txt\"", ""), "secret.txt");
        assert_eq!(filename(&url, "attachment; filename=old; filename*=UTF-8''caf%C3%A9.txt", ""), "café.txt");
        assert_eq!(filename(&Url::parse("https://example.org/").unwrap(), "", "text/html"), "page.html");
    }
    #[test]
    fn collisions_symlinks_and_failed_download_cleanup() {
        let dir = std::env::temp_dir().join(format!("dome-download-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        std::os::unix::fs::symlink("/dev/null", dir.join("file.txt")).unwrap();
        let mut pending = PendingFile::new(&dir, "file.txt").unwrap();
        pending.file.write_all(b"content").unwrap();
        let path = pending.finish().unwrap();
        assert_eq!(path.file_name().unwrap(), "file (1).txt");
        assert_eq!(std::fs::read(path).unwrap(), b"content");
        let pending = PendingFile::new(&dir, "partial").unwrap();
        drop(pending);
        assert!(!dir.join("partial").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn native_download_names_use_protocol_resource_paths() {
        for (url,expected) in [
            ("gopher://host/9report%20test.bin","report test.bin"),
            ("gopher+://host/0/report.txt%09%09%2Btext%2Fplain","report.txt"),
            ("ftp://host/pub/report.txt;type=a","report.txt"),
            ("gemini://host/pub/caf%C3%A9.txt","café.txt"),
            ("scroll://host/pub/picture.png","picture.png"),
            ("nex://host/pub/picture.png","picture.png"),
            ("spartan://host/pub/picture.png","picture.png"),
        ] {assert_eq!(filename(&Url::parse(url).unwrap(),"","application/octet-stream"),expected);}
    }
}
