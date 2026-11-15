// SPDX-License-Identifier: GPL-3.0-or-later
use std::{fs::{File, OpenOptions}, io::{self, Write, Seek, SeekFrom}, os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf}, sync::mpsc::{self, Receiver, Sender}};
use servo::{ResourceDownloadEvent, WebView};
use url::Url;

pub enum Completion {
    Saved(PathBuf),
    Opened(PathBuf),
    Ready(ReadyDownload, bool),
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
        self.start_named(view, url, copy_image, context, String::new());
    }
    pub fn start_named(&self, view: &WebView, url: Url, copy_image: bool,
        context: Option<servo::EmbedderControlId>, hint: String) {
        let sender = self.sender.clone();
        let mut file: Option<PendingFile> = None;
        let mut bytes = Vec::new();
        let mut metadata = None;
        let result = view.download_resource_in_context(url, context, move |event| {
            let result: Result<(), String> = (|| {
                match event {
                    ResourceDownloadEvent::Response { url, content_type, content_disposition } => {
                        if !copy_image {
                            let disposition = if !hint.is_empty() && !content_disposition.to_ascii_lowercase().contains("filename") {
                                format!("attachment; filename*=UTF-8''{}", percent_encoding::utf8_percent_encode(&hint, percent_encoding::NON_ALPHANUMERIC))
                            } else { content_disposition };
                            metadata = Some((filename(&url, &disposition, &content_type), content_type));
                            file = Some(PendingFile::staged().map_err(|e| e.to_string())?);
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
                            let (name, mime) = metadata.take().ok_or("Download has no response")?;
                            Completion::Ready(ReadyDownload { file: file.take().ok_or("Download has no response")?, name, mime }, false)
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
    pub fn ready(&self, file: ReadyDownload, handoff: bool) {
        let _ = self.sender.send(Completion::Ready(file, handoff));
    }
    /// Dialogs run outside Servo callbacks; filesystem work and application
    /// startup run on a worker after the user chooses an action.
    pub fn present(&self, download: ReadyDownload, handoff: bool) {
        let mime = mime_type(&download.mime);
        let action = if handoff {
            unsafe { browser_ui_download_action(crate::ui::c(&download.name).as_ptr(), crate::ui::c(&mime).as_ptr()) }
        } else { 1 };
        let destination = match action {
            1 => {
                let directory = match downloads_directory() { Ok(path) => path, Err(error) => {
                    let _ = self.sender.send(Completion::Error(error.to_string())); return;
                }};
                let suggested = directory.join(&download.name);
                let chosen = unsafe { browser_ui_save_as(crate::ui::c(&suggested.to_string_lossy()).as_ptr()) };
                if chosen.is_null() { None } else {
                    Some(PathBuf::from(unsafe { std::ffi::CStr::from_ptr(chosen) }.to_string_lossy().into_owned()))
                }
            },
            2 => match downloads_directory() { Ok(directory) => Some(directory), Err(error) => {
                let _ = self.sender.send(Completion::Error(error.to_string())); return;
            }},
            _ => None,
        };
        let Some(destination) = destination else { crate::ui::status("Download cancelled"); return; };
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<Completion, String> {
                let path = if action == 2 { download.save_unique(&destination) } else { download.save_to(&destination) }
                    .map_err(|error| error.to_string())?;
                if action == 2 {
                    let path_c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| "Invalid saved path")?;
                    match unsafe { browser_launch_default_file(path_c.as_ptr(), crate::ui::c(&mime).as_ptr()) } {
                        0 => Ok(Completion::Opened(path)),
                        _ => Err(format!("Saved to {}. The default application for {mime} could not open it; check DomeD Default Applications", path.display())),
                    }
                } else { Ok(Completion::Saved(path)) }
            })();
            let _ = sender.send(result.unwrap_or_else(Completion::Error));
        });
    }

}

unsafe extern "C" {
    fn browser_ui_save_as(suggested: *const std::ffi::c_char) -> *const std::ffi::c_char;
    fn browser_ui_download_action(name: *const std::ffi::c_char, mime: *const std::ffi::c_char) -> i32;
    fn browser_launch_default_file(path: *const std::ffi::c_char, mime: *const std::ffi::c_char) -> i32;
}

pub fn mime_type(value: &str) -> String {
    value.parse::<mime::Mime>().map(|kind| kind.essence_str().to_ascii_lowercase())
        .unwrap_or_else(|_| "application/octet-stream".into())
}

pub struct ReadyDownload { pub file: PendingFile, pub name: String, pub mime: String }
impl ReadyDownload {
    pub fn save_to(mut self, path: &Path) -> io::Result<PathBuf> {
        let parent = path.parent().filter(|parent| parent.is_dir())
            .ok_or_else(|| io::Error::other("Choose an existing destination folder"))?;
        // A sibling temporary file makes overwriting atomic, including when
        // the download cache and destination are on different filesystems.
        let mut target = PendingFile::new(parent, &format!(".dome-download-{}.part", uuid::Uuid::new_v4()))?;
        self.file.file.seek(SeekFrom::Start(0))?;
        io::copy(&mut self.file.file, &mut target.file)?;
        target.file.sync_all()?;
        std::fs::rename(&target.path, path)?;
        target.committed = true;
        Ok(path.to_path_buf())
    }
    pub fn save_unique(mut self, directory: &Path) -> io::Result<PathBuf> {
        let mut target = PendingFile::new(directory, &self.name)?;
        self.file.file.seek(SeekFrom::Start(0))?;
        io::copy(&mut self.file.file, &mut target.file)?;
        target.finish()
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
pub(crate) fn downloads_directory() -> io::Result<PathBuf> {
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
pub(crate) fn filename(url: &Url, disposition: &str, mime: &str) -> String {
    let mut plain = None;
    let mut extended = None;
    let mut parts = Vec::new();
    let mut part = String::new();
    let (mut quoted, mut escaped) = (false, false);
    for character in disposition.chars() {
        if character == ';' && !quoted { parts.push(std::mem::take(&mut part)); continue; }
        part.push(character);
        if escaped { escaped = false; }
        else if character == '\\' && quoted { escaped = true; }
        else if character == '"' { quoted = !quoted; }
    }
    parts.push(part);
    for part in parts.iter().skip(1) {
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
    if Path::new(&name).extension().is_none() && let Some(extension) =
        mime_guess::get_mime_extensions_str(&mime_type(mime)).and_then(|extensions| extensions.first())
    { name.push('.'); name.push_str(extension); }
    name
}
pub(crate) struct PendingFile { pub(crate) file: File, path: PathBuf, committed: bool }
impl PendingFile {
    pub(crate) fn staged() -> io::Result<Self> {
        let root = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .ok_or_else(|| io::Error::other("Download cache is unavailable"))?.join("dome-browser/downloads");
        std::fs::create_dir_all(&root)?;
        Self::new(&root, &format!(".download-{}.part", uuid::Uuid::new_v4()))
    }

    pub(crate) fn new(dir: &Path, name: &str) -> io::Result<Self> {
        for index in 0..10000 {
            let path = dir.join(if index == 0 { name.to_owned() } else {
                let p = Path::new(name);
                let stem = p.file_stem().unwrap_or_default().to_string_lossy();
                match p.extension() { Some(ext) => format!("{stem} ({index}).{}", ext.to_string_lossy()), None => format!("{stem} ({index})") }
            });
            match OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&path) {
                Ok(file) => return Ok(Self { file, path, committed: false }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("Too many files with the same name"))
    }
    pub(crate) fn finish(mut self) -> io::Result<PathBuf> {
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
    fn response_names_and_mime_parameters() {
        let url = Url::parse("https://example.invalid/").unwrap();
        assert_eq!(filename(&url, "attachment; filename=\"report; final.zip\"", "application/zip"), "report; final.zip");
        assert_eq!(filename(&url, "attachment; filename*=UTF-8''%2Ftmp%2Fcaf%C3%A9", "application/pdf"), "café.pdf");
        assert_eq!(mime_type("Application/PDF; charset=binary"), "application/pdf");
        assert_eq!(mime_type("not a mime"), "application/octet-stream");
    }
    #[test]
    fn chosen_paths_are_replaced_atomically_and_cancelled_staging_is_removed() {
        let dir = std::env::temp_dir().join(format!("dome-save-as-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let destination = dir.join("chosen ünicode.bin");
        std::fs::write(&destination, b"original").unwrap();
        let mut file = PendingFile::new(&dir, "staging").unwrap();
        file.file.write_all(b"complete\0binary").unwrap();
        let download = ReadyDownload { file, name: "ignored.bin".into(), mime: "application/octet-stream".into() };
        download.save_to(&destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"complete\0binary");
        assert!(!dir.join("staging").exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let file = PendingFile::new(&dir, "cancelled").unwrap();
        drop(ReadyDownload { file, name: "cancel".into(), mime: "".into() });
        assert!(!dir.join("cancelled").exists());
        let mut file = PendingFile::new(&dir, "failed").unwrap();
        file.file.write_all(b"failed").unwrap();
        let download = ReadyDownload { file, name: "ignored".into(), mime: "".into() };
        assert!(download.save_to(&dir.join("missing/target")).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"complete\0binary");
        assert!(!dir.join("failed").exists());
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
