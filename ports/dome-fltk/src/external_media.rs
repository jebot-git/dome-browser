// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit, native-UI handoff of unsupported media to a local player.
use std::{env, fs, io, os::unix::fs::PermissionsExt, path::{Path, PathBuf},
    process::{Command, Stdio}, sync::mpsc::{self, Receiver, Sender}};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Deserialize)]
pub struct Candidate {
    pub url: String,
    #[serde(default)] pub mime: String,
    #[serde(default)] pub reason: String,
}
impl Candidate {
    pub fn target(&self) -> io::Result<Url> { target(&self.url) }
    pub fn label(&self) -> String {
        let Ok(url) = self.target() else { return "Unavailable media".into(); };
        let name = url.path_segments().and_then(|mut segments| segments.next_back())
            .filter(|name| !name.is_empty()).unwrap_or("media");
        let kind = if self.mime.is_empty() { "unsupported media" } else { &self.mime };
        clean(&format!("{kind}: {} / {name}", url.host_str().unwrap_or("")), 160)
    }
}
fn clean(text: &str, limit: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(limit).collect()
}
fn invalid(message: &str) -> io::Error { io::Error::new(io::ErrorKind::InvalidInput, message) }
/// Never pass page-controlled local paths, options or arbitrary URI schemes to a player.
pub fn target(text: &str) -> io::Result<Url> {
    if text.len() > 16 * 1024 || text.chars().any(char::is_control) {
        return Err(invalid("Invalid media address"));
    }
    let mut url = Url::parse(text).map_err(|_| invalid("Invalid media address"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() ||
        !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("External playback requires an HTTP or HTTPS address without embedded credentials"));
    }
    url.set_fragment(None);
    Ok(url)
}
pub fn candidates(text: &str) -> io::Result<Vec<Candidate>> {
    if text.len() > 512 * 1024 { return Err(invalid("Too many media addresses")); }
    let values: Vec<Candidate> = serde_json::from_str(text).map_err(|_| invalid("Cannot read page media"))?;
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for mut candidate in values.into_iter().take(24) {
        if let Ok(url) = candidate.target() {
            candidate.url = url.to_string();
            candidate.mime = clean(&candidate.mime, 128);
            candidate.reason = clean(&candidate.reason, 80);
            if seen.insert(candidate.url.clone()) { result.push(candidate); }
        }
    }
    Ok(result)
}

#[derive(Clone, Serialize, Deserialize)]
struct Settings { version: u32, executable: PathBuf }
fn executable(path: &Path) -> bool {
    path.is_absolute() && fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}
fn find(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .filter(|dir| dir.is_absolute()).map(|dir| dir.join(name)).find(|path| executable(path))
}

pub struct Handoff {
    settings: PathBuf,
    sender: Sender<bool>,
    pub receiver: Receiver<bool>,
}
impl Handoff {
    pub fn new(profile: &Path) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self { settings: profile.join("external-player.json"), sender, receiver }
    }
    pub fn player(&self) -> io::Result<PathBuf> {
        let configured = if let Some(path) = env::var_os("DOME_BROWSER_EXTERNAL_PLAYER") {
            Some(PathBuf::from(path))
        } else if self.settings.exists() {
            let settings: Settings = serde_json::from_str(&fs::read_to_string(&self.settings)?)
                .map_err(|_| invalid("Cannot read external player settings"))?;
            if settings.version != 1 { return Err(invalid("Unsupported external player settings")); }
            Some(settings.executable)
        } else { None };
        if let Some(path) = configured {
            if !executable(&path) { return Err(invalid("The external player must be an executable file with an absolute path")); }
            return Ok(path);
        }
        ["mpv", "vlc", "celluloid"].into_iter().find_map(find)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Choose a player in Tools / External player"))
    }
    pub fn configure(&self, path: &str) -> io::Result<()> {
        let path = PathBuf::from(path.trim());
        if !executable(&path) { return Err(invalid("Choose an executable file using its absolute path")); }
        let data = serde_json::to_string_pretty(&Settings { version:1, executable:path }).map_err(io::Error::other)?;
        super::storage::write(&self.settings, &(data + "\n"))
    }
    pub fn launch(&self, text: &str) -> io::Result<()> {
        let url = target(text)?;
        spawn_player(&self.player()?, &url, self.sender.clone())
    }
}

fn spawn_player(player: &Path, url: &Url, sender: Sender<bool>) -> io::Result<()> {
    let mut child = Command::new(player).arg("--").arg(url.as_str())
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    std::thread::spawn(move || { let _ = sender.send(child.wait().is_ok_and(|status| status.success())); });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_remote_media_can_be_handed_off() {
        for text in ["file:///etc/passwd", "javascript:alert(1)", "data:video/mp4,test", "blob:https://example.test/123", "--help", "https://name:secret@example.test/video", "https://example.test/\nvideo"] {
            assert!(target(text).is_err(), "{text:?}");
        }
        assert_eq!(target("https://example.test/a.mkv?token=secret#ignored").unwrap().as_str(), "https://example.test/a.mkv?token=secret");
    }
    #[test]
    fn media_choices_drop_invalid_targets_and_do_not_display_query_tokens() {
        let choices = candidates(r#"[{"url":"https://example.test/a.mkv?token=secret","mime":"video/x-matroska"},{"url":"https://example.test/a.mkv?token=secret"},{"url":"file:///etc/passwd"}]"#).unwrap();
        assert_eq!(choices.len(), 1);
        assert!(!choices[0].label().contains("secret"));
        assert!(choices[0].target().unwrap().query().unwrap().contains("secret"));
    }
    #[test]
    fn executable_paths_are_data_not_shell_commands() {
        let folder = env::temp_dir().join(format!("dome-player-test-{}", std::process::id()));
        fs::create_dir(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
        let player = folder.join("fake-player.py");
        fs::write(&player, "#!/usr/bin/python3\nimport json,pathlib,sys\npathlib.Path(__file__).with_suffix('.argv').write_text(json.dumps(sys.argv[1:]))\n").unwrap();
        fs::set_permissions(&player, fs::Permissions::from_mode(0o700)).unwrap();
        let handoff = Handoff::new(&folder);
        assert!(handoff.configure("mpv --some-option").is_err());
        handoff.configure(player.to_str().unwrap()).unwrap();
        let settings: Settings = serde_json::from_str(&fs::read_to_string(&handoff.settings).unwrap()).unwrap();
        assert_eq!(settings.executable, player);
        assert_eq!(fs::metadata(&handoff.settings).unwrap().permissions().mode() & 0o777, 0o600);
        let url = target("https://example.test/video.mkv?name=$(false)&x=1").unwrap();
        spawn_player(&player, &url, handoff.sender.clone()).unwrap();
        assert!(handoff.receiver.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        let argv: Vec<String> = serde_json::from_str(&fs::read_to_string(player.with_extension("argv")).unwrap()).unwrap();
        assert_eq!(argv, ["--", url.as_str()]);
        fs::remove_dir_all(folder).unwrap();
    }
}
