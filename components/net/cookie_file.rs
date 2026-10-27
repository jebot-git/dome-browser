/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Private, editable cookie persistence. Never log file contents or parse errors
//! (serde errors can contain cookie values).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::cookie_storage::CookieStorage;

const LIMIT: usize = 4 * 1024 * 1024;
pub(crate) const HEADER: &str = "# Dome cookies v1: one JSON object per line. Delete a line to remove a cookie.\n# cookie is Set-Cookie text; expiry_time is the stored absolute expiry (Max-Age is not restarted).\n";

#[derive(Clone, Debug, PartialEq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

fn stamp(path: &Path) -> io::Result<Option<Stamp>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Some(metadata_stamp(&meta))),
        Ok(_) => Err(invalid()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn metadata_stamp(meta: &fs::Metadata) -> Stamp {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        #[cfg(unix)]
        identity: (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid cookie file; contents preserved")
}

fn read_private(path: &Path) -> io::Result<String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > LIMIT as u64 {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Editors may replace a private file with one created under their umask.
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let mut text = String::new();
    file.take(LIMIT as u64 + 1).read_to_string(&mut text)?;
    if text.len() > LIMIT { return Err(invalid()); }
    Ok(text)
}

fn write_private(path: &Path, text: &str) -> io::Result<()> {
    if text.len() > LIMIT { return Err(invalid()); }
    let parent = path.parent().ok_or_else(invalid)?;
    let mut temporary = tempfile::Builder::new().prefix(".ck-").tempfile_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(text.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()
}

#[derive(Clone, Debug)]
pub(crate) struct CookieFile {
    path: PathBuf,
    observed: Option<Stamp>,
    loaded: bool,
    writable: bool,
    warned: bool,
}

impl CookieFile {
    pub(crate) fn attach(directory: &Path, storage: &mut CookieStorage) -> Self {
        let path = directory.join("ck.txt");
        let marker = directory.join(".ck-initialized");
        let legacy = directory.join("cookie_jar.json");
        let mut file = Self { path, observed: None, loaded: false, writable: false, warned: false };
        let result = (|| -> io::Result<()> {
            // Mark the format as initialized even for a malformed ck.txt. Its
            // later deletion must never cause a fallback to old cookie values.
            let initialized = fs::symlink_metadata(&marker).is_ok();
            let existing = fs::symlink_metadata(&file.path).is_ok();
            if !initialized && !existing && fs::symlink_metadata(&legacy).is_ok() {
                let text = read_private(&legacy)?;
                let old: CookieStorage = serde_json::from_str(&text).map_err(|_| invalid())?;
                let imported = CookieStorage::from_file_records(old.file_records(), storage.file_capacity())?;
                write_private(&file.path, &imported.file_text()?)?;
                // Only after the replacement is durable, retire the legacy
                // source. Do not retain a second live copy of cookie secrets.
                fs::remove_file(&legacy)?;
            }
            if !initialized { write_private(&marker, "ck.txt is authoritative; do not import cookie_jar.json\n")?; }
            if stamp(&file.path)?.is_none() { write_private(&file.path, HEADER)?; }
            file.refresh(storage);
            Ok(())
        })();
        if result.is_err() { file.failure(storage); }
        file
    }

    fn failure(&mut self, storage: &mut CookieStorage) {
        storage.replace_file_records(CookieStorage::new(storage.file_capacity()));
        self.writable = false;
        if !self.warned {
            log::warn!("Cookie file unavailable or invalid; disk contents preserved and persisted cookies withheld. Repair ck.txt to resume cookie persistence.");
            self.warned = true;
        }
    }

    pub(crate) fn refresh(&mut self, storage: &mut CookieStorage) {
        let Ok(current) = stamp(&self.path) else { self.failure(storage); return; };
        if self.loaded && current == self.observed { return; }
        let result = if current.is_none() {
            Ok(CookieStorage::new(storage.file_capacity()))
        } else {
            read_private(&self.path).and_then(|text| CookieStorage::parse_file(&text, storage.file_capacity()))
        };
        // Re-stat after reading/chmod. If an editor replaces the file while it
        // is being read, retry next time rather than associating old values with
        // the replacement's stamp.
        let after = stamp(&self.path);
        self.loaded = true;
        self.observed = current.clone();
        match (result, after) {
            (Ok(records), Ok(after)) if after.as_ref().map(|s| (&s.len, &s.modified, stamp_identity(s))) ==
                current.as_ref().map(|s| (&s.len, &s.modified, stamp_identity(s))) => {
                self.observed = after;
                storage.replace_file_records(records);
                self.writable = true;
                self.warned = false;
            },
            _ => self.failure(storage),
        }
    }

    pub(crate) fn flush(&mut self, storage: &mut CookieStorage) {
        if !storage.file_dirty() || !self.writable { return; }
        if stamp(&self.path).ok().as_ref() != Some(&self.observed) {
            self.refresh(storage);
            return;
        }
        let result = storage.file_text().and_then(|text| write_private(&self.path, &text));
        match result {
            Ok(()) => {
                self.observed = stamp(&self.path).ok().flatten();
                storage.file_clean();
                self.warned = false;
            },
            Err(_) => {
                if !self.warned { log::warn!("Could not save ck.txt; existing disk contents preserved"); }
                self.warned = true;
            },
        }
    }
}

// ctime changes when read_private tightens permissions; inode must still match.
#[cfg(unix)]
fn stamp_identity(stamp: &Stamp) -> (u64, u64) { (stamp.identity.0, stamp.identity.1) }
#[cfg(not(unix))]
fn stamp_identity(_: &Stamp) {}
