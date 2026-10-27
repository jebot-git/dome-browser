// SPDX-License-Identifier: GPL-3.0-or-later
//! Native cookie-file editing. Page scripts cannot invoke this dialog.
use std::{fs::OpenOptions, io::{self, Read}, os::unix::fs::OpenOptionsExt, path::Path};

use crate::{storage, ui};

/// An allow marker makes blocking the default, including for a new profile or
/// after deleting the setting. Reject symlinks and malformed settings.
pub fn third_party_blocked(profile: &Path) -> io::Result<bool> {
    let path = profile.join("cookie-policy.json");
    match read(&path) {
        Ok(text) => {
            let value: serde_json::Value = serde_json::from_str(&text)
                .map_err(|_| io::Error::other("Invalid cookie-policy.json"))?;
            value.get("block_third_party").and_then(|value| value.as_bool())
                .ok_or_else(|| io::Error::other("cookie-policy.json needs block_third_party"))
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

pub fn set_third_party_blocked(profile: &Path, blocked: bool) -> io::Result<()> {
    storage::write(&profile.join("cookie-policy.json"),
        &format!("{{\"block_third_party\":{blocked}}}\n"))
}

fn read(path: &Path) -> io::Result<String> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    if !file.metadata()?.is_file() { return Err(io::Error::other("Cookie settings must be a regular file")); }
    let mut text = String::new();
    file.take(4 * 1024 * 1024 + 1).read_to_string(&mut text)?;
    if text.len() > 4 * 1024 * 1024 { return Err(io::Error::other("Cookie settings exceed 4 MiB")); }
    Ok(text)
}

pub fn edit(profile: &Path) -> io::Result<bool> {
    let path = profile.join("ck.txt");
    let original = read(&path)?;
    let Some(edited) = ui::string(unsafe { ui::browser_ui_edit_cookies(ui::c(&original).as_ptr()) }) else { return Ok(false); };
    if edited == original { return Ok(false); }
    if read(&path)? != original {
        return Err(io::Error::other("Cookies changed while the editor was open; reopen ck.txt before editing"));
    }
    storage::write(&path,&edited)?;
    Ok(true)
}
