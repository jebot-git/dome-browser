/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread;

use net_traits::request::RequestBuilder as Request;
use rusqlite::{Connection, OptionalExtension, params};
use servo_base::generic_channel;
use servo_url::ImmutableOrigin;
use storage_traits::cache_storage::{
    CacheEntry, CacheFailure, CacheIdentifier, CacheOperation, CacheQuery, CacheResult,
    CacheStorageThreadHandle, CacheStorageThreadMessage,
};
use storage_traits::client_storage::StorageProxyMap;

/// One database lives inside each origin's "caches" storage bottle. User supplied
/// cache names are SQL values, never paths. Lists keep their identities when a
/// name is deleted and subsequently opened again.
struct CacheDatabase {
    connection: Connection,
    bottle_id: i64,
}

#[derive(Debug)]
enum Error {
    Database(rusqlite::Error),
    Encoding(postcard::Error),
    InvalidState,
    Type(&'static str),
    Storage(String),
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}
impl From<postcard::Error> for Error {
    fn from(error: postcard::Error) -> Self {
        Self::Encoding(error)
    }
}
impl From<Error> for CacheFailure {
    fn from(error: Error) -> Self {
        match error {
            Error::InvalidState => Self::InvalidState,
            Error::Type(message) => Self::Type(message.into()),
            Error::Database(rusqlite::Error::SqliteFailure(code, _))
                if code.code == rusqlite::ErrorCode::DiskFull =>
            {
                Self::QuotaExceeded
            },
            error => Self::Storage(format!("Cache storage: {error:?}")),
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

/// Fetch header-list combined values. Header names are ASCII case insensitive;
fn encode_name(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

/// missing and present-but-empty values must remain distinguishable for Vary.
fn header_value(headers: &[(String, Vec<u8>)], name: &str) -> Option<Vec<u8>> {
    let mut result: Option<Vec<u8>> = None;
    for (_, value) in headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
    {
        match &mut result {
            None => result = Some(value.clone()),
            Some(combined) => {
                combined.extend_from_slice(b", ");
                combined.extend(value);
            },
        }
    }
    result
}

fn request_headers(request: &Request) -> Vec<(String, Vec<u8>)> {
    request
        .headers
        .iter()
        .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
        .collect()
}

/// <https://w3c.github.io/ServiceWorker/#request-matches-cached-item-algorithm>
fn matches(query: &Request, entry: &CacheEntry, options: CacheQuery) -> bool {
    if !options.ignore_method && query.method.as_str() != "GET" {
        return false;
    }
    let mut query_url = query.url.url();
    let mut cached_url = entry.request.url.url();
    query_url.set_fragment(None);
    cached_url.set_fragment(None);
    if options.ignore_search {
        query_url.as_mut_url().set_query(None);
        cached_url.as_mut_url().set_query(None);
    }
    if query_url != cached_url {
        return false;
    }
    if options.ignore_vary {
        return true;
    }
    let Some(vary) = header_value(&entry.response.metadata.headers, "vary") else {
        return true;
    };
    let query_headers = request_headers(query);
    let cached_headers = request_headers(&entry.request);
    for field in vary.split(|byte| *byte == b',') {
        let Ok(field) = std::str::from_utf8(field) else {
            return false;
        };
        let name = field.trim_matches([' ', '\t']);
        if name == "*" {
            return false;
        }
        if !name.is_empty()
            && header_value(&query_headers, name) != header_value(&cached_headers, name)
        {
            return false;
        }
    }
    true
}

// Version 1 did not retain Fetch's credential-inclusion flag. Keep its bodies,
// names and metadata; treat an unknown historical flag as credentialed.
#[derive(serde::Deserialize, serde::Serialize)]
struct LegacyCacheEntry {
    request: Request,
    response: LegacyCachedResponse,
}
#[derive(serde::Deserialize, serde::Serialize)]
struct LegacyCachedResponse {
    kind: storage_traits::cache_storage::CachedResponseType,
    metadata: LegacyCachedResponseMetadata,
    internal_metadata: Option<LegacyCachedResponseMetadata>,
    body: Option<Vec<u8>>,
}
#[derive(serde::Deserialize, serde::Serialize)]
struct LegacyCachedResponseMetadata {
    url: Option<servo_url::ServoUrl>,
    status: u16,
    status_text: Vec<u8>,
    headers: Vec<(String, Vec<u8>)>,
    redirected: bool,
}
impl LegacyCachedResponseMetadata {
    fn into_current(self) -> storage_traits::cache_storage::CachedResponseMetadata {
        storage_traits::cache_storage::CachedResponseMetadata {
            request_includes_credentials: true,
            url: self.url,
            status: self.status,
            status_text: self.status_text,
            headers: self.headers,
            redirected: self.redirected,
        }
    }
}
fn decode_entry(bytes: &[u8], version: i64) -> Result<CacheEntry> {
    match version {
        1 => {
            let old: LegacyCacheEntry = postcard::from_bytes(bytes)?;
            Ok(CacheEntry {
                request: old.request,
                response: storage_traits::cache_storage::CachedResponse {
                    kind: old.response.kind,
                    metadata: old.response.metadata.into_current(),
                    internal_metadata: old
                        .response
                        .internal_metadata
                        .map(LegacyCachedResponseMetadata::into_current),
                    body: old.response.body,
                },
            })
        },
        2 => Ok(postcard::from_bytes(bytes)?),
        _ => Err(Error::Storage(format!(
            "Unsupported cache record format {version}"
        ))),
    }
}

impl CacheDatabase {
    fn open(path: &Path, bottle_id: i64) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS cache_lists (
                id INTEGER PRIMARY KEY AUTOINCREMENT, name BLOB UNIQUE);
             CREATE TABLE IF NOT EXISTS cache_entries (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                cache_id INTEGER NOT NULL REFERENCES cache_lists(id) ON DELETE CASCADE,
                record BLOB NOT NULL, format_version INTEGER NOT NULL DEFAULT 2);
             CREATE INDEX IF NOT EXISTS cache_entry_list ON cache_entries(cache_id, sequence);
             DELETE FROM cache_lists WHERE name IS NULL;",
        )?;
        let has_format_version: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('cache_entries') WHERE name='format_version')",
            [], |row| row.get(0),
        )?;
        if !has_format_version {
            connection.execute_batch(
                "ALTER TABLE cache_entries ADD COLUMN format_version INTEGER NOT NULL DEFAULT 1;",
            )?;
        }
        Ok(Self {
            connection,
            bottle_id,
        })
    }

    fn identity(&self, id: i64) -> CacheIdentifier {
        CacheIdentifier {
            bottle_id: self.bottle_id,
            id,
        }
    }

    fn check_identity(&self, cache: CacheIdentifier) -> Result<()> {
        if cache.bottle_id != self.bottle_id
            || !self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM cache_lists WHERE id=?1)",
                [cache.id],
                |row| row.get::<_, bool>(0),
            )?
        {
            return Err(Error::InvalidState);
        }
        Ok(())
    }

    fn named(&self, name: &[u16]) -> Result<Option<i64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT id FROM cache_lists WHERE name=?1",
                [encode_name(name)],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn entries(
        &self,
        id: i64,
        query: Option<&Request>,
        options: CacheQuery,
    ) -> Result<Vec<(i64, CacheEntry)>> {
        let mut statement = self.connection.prepare(
            "SELECT sequence,record,format_version FROM cache_entries WHERE cache_id=?1 ORDER BY sequence",
        )?;
        let rows = statement.query_map([id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (sequence, bytes, version) = row?;
            let entry = decode_entry(&bytes, version)?;
            if query.is_none_or(|query| matches(query, &entry, options)) {
                entries.push((sequence, entry));
            }
        }
        Ok(entries)
    }

    fn run(&mut self, operation: CacheOperation) -> Result<CacheResult> {
        match operation {
            CacheOperation::Open(name) => {
                if let Some(id) = self.named(&name)? {
                    return Ok(CacheResult::Opened(self.identity(id)));
                }
                self.connection.execute(
                    "INSERT INTO cache_lists(name) VALUES (?1)",
                    [encode_name(&name)],
                )?;
                Ok(CacheResult::Opened(
                    self.identity(self.connection.last_insert_rowid()),
                ))
            },
            CacheOperation::Has(name) => Ok(CacheResult::Boolean(self.named(&name)?.is_some())),
            CacheOperation::DeleteCache(name) => Ok(CacheResult::Boolean(
                self.connection.execute(
                    "UPDATE cache_lists SET name=NULL WHERE name=?1",
                    [encode_name(&name)],
                )? != 0,
            )),
            CacheOperation::StorageKeys => {
                let mut statement = self
                    .connection
                    .prepare("SELECT name FROM cache_lists WHERE name IS NOT NULL ORDER BY id")?;
                let names = statement
                    .query_map([], |row| row.get::<_, Vec<u8>>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let names = names
                    .into_iter()
                    .map(|bytes| {
                        if bytes.len() % 2 != 0 {
                            return Err(Error::Storage("Invalid cache name encoding".into()));
                        }
                        Ok(bytes
                            .chunks_exact(2)
                            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                            .collect())
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(CacheResult::Names(names))
            },
            CacheOperation::StorageMatch {
                request,
                options,
                cache_name,
            } => {
                let ids: Vec<i64> = if let Some(name) = cache_name {
                    self.named(&name)?.into_iter().collect()
                } else {
                    let mut statement = self
                        .connection
                        .prepare("SELECT id FROM cache_lists WHERE name IS NOT NULL ORDER BY id")?;
                    statement
                        .query_map([], |row| row.get(0))?
                        .collect::<std::result::Result<_, _>>()?
                };
                for id in ids {
                    if let Some((_, entry)) = self
                        .entries(id, Some(&request), options)?
                        .into_iter()
                        .next()
                    {
                        return Ok(CacheResult::Entries(vec![entry]));
                    }
                }
                Ok(CacheResult::Entries(Vec::new()))
            },
            CacheOperation::Query {
                cache,
                request,
                options,
            } => {
                self.check_identity(cache)?;
                Ok(CacheResult::Entries(
                    self.entries(cache.id, request.as_ref(), options)?
                        .into_iter()
                        .map(|(_, entry)| entry)
                        .collect(),
                ))
            },
            CacheOperation::DeleteEntries {
                cache,
                request,
                options,
            } => {
                self.check_identity(cache)?;
                let entries = self.entries(cache.id, Some(&request), options)?;
                let tx = self.connection.transaction()?;
                for (sequence, _) in &entries {
                    tx.execute("DELETE FROM cache_entries WHERE sequence=?1", [sequence])?;
                }
                tx.commit()?;
                Ok(CacheResult::Boolean(!entries.is_empty()))
            },
            CacheOperation::Put { cache, entries } => {
                self.check_identity(cache)?;
                // Validate the whole batch before changing the database. Duplicate
                // addAll requests and failures must leave the previous list intact.
                for (index, entry) in entries.iter().enumerate() {
                    if !matches!(entry.request.url.url().scheme(), "http" | "https")
                        || entry.request.method.as_str() != "GET"
                    {
                        return Err(Error::Type("Cache writes require an HTTP(S) GET request"));
                    }
                    if entry.response.metadata.status == 206
                        || header_value(&entry.response.metadata.headers, "vary").is_some_and(
                            |value| {
                                value
                                    .split(|byte| *byte == b',')
                                    .any(|field| field.trim_ascii() == b"*")
                            },
                        )
                    {
                        return Err(Error::Type("Partial responses and Vary:* cannot be cached"));
                    }
                    if entries[..index]
                        .iter()
                        .any(|previous| matches(&entry.request, previous, CacheQuery::default()))
                    {
                        return Err(Error::InvalidState);
                    }
                }
                let mut existing = self.entries(cache.id, None, CacheQuery::default())?;
                let mut remove = Vec::new();
                let mut records = Vec::new();
                for entry in entries {
                    existing.retain(|(sequence, previous)| {
                        if matches(&entry.request, previous, CacheQuery::default()) {
                            remove.push(*sequence);
                            false
                        } else {
                            true
                        }
                    });
                    records.push(postcard::to_stdvec(&entry)?);
                }
                let tx = self.connection.transaction()?;
                for sequence in remove {
                    tx.execute("DELETE FROM cache_entries WHERE sequence=?1", [sequence])?;
                }
                for bytes in records {
                    tx.execute(
                        "INSERT INTO cache_entries(cache_id,record,format_version) VALUES (?1,?2,2)",
                        params![cache.id, bytes],
                    )?;
                }
                tx.commit()?;
                Ok(CacheResult::Done)
            },
        }
    }
}

struct CacheStorageEngine {
    databases: HashMap<(ImmutableOrigin, i64), CacheDatabase>,
}

impl CacheStorageEngine {
    fn run(
        &mut self,
        origin: ImmutableOrigin,
        proxy: StorageProxyMap,
        operation: CacheOperation,
    ) -> Result<CacheResult> {
        if matches!(origin, ImmutableOrigin::Opaque(_)) {
            return Err(Error::Storage(
                "Opaque origins have no cache storage".into(),
            ));
        }
        let key = (origin, proxy.bottle_id);
        if !self.databases.contains_key(&key) {
            let (directory, _) = proxy
                .handle
                .create_database(proxy.bottle_id, "servo-cache-lists-v2".into())
                .recv()
                .map_err(|_| Error::Storage("Storage service unavailable".into()))?
                .map_err(Error::Storage)?;
            let database = CacheDatabase::open(&directory.join("cache.sqlite3"), proxy.bottle_id)?;
            self.databases.insert(key.clone(), database);
        }
        self.databases.get_mut(&key).unwrap().run(operation)
    }
}

pub trait CacheStorageThreadFactory {
    fn new(config_dir: Option<PathBuf>, temporary_storage: bool) -> Self;
}

impl CacheStorageThreadFactory for CacheStorageThreadHandle {
    fn new(_config_dir: Option<PathBuf>, _temporary_storage: bool) -> Self {
        // The client-storage service owns persistent/private directory selection.
        let (sender, receiver) = generic_channel::channel().unwrap();
        thread::Builder::new()
            .name("CacheStorageThread".into())
            .spawn(move || {
                let mut engine = CacheStorageEngine {
                    databases: HashMap::new(),
                };
                while let Ok(message) = receiver.recv() {
                    match message {
                        CacheStorageThreadMessage::Run {
                            origin,
                            proxy,
                            operation,
                            callback,
                        } => {
                            let _ = callback.send(
                                engine
                                    .run(origin, proxy, operation)
                                    .map_err(CacheFailure::from),
                            );
                        },
                        CacheStorageThreadMessage::Exit(reply) => {
                            let _ = reply.send(());
                            break;
                        },
                    }
                }
            })
            .expect("Cache storage thread spawning failed");
        Self::new(sender)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net_traits::request::Referrer;
    use servo_url::ServoUrl;
    use storage_traits::cache_storage::{
        CachedResponse, CachedResponseMetadata, CachedResponseType,
    };

    fn request(url: &str) -> Request {
        Request::new(
            None,
            net_traits::blob_url_store::UrlWithBlobClaim::new(ServoUrl::parse(url).unwrap(), None),
            Referrer::NoReferrer,
        )
    }
    fn entry(url: &str, bytes: &[u8]) -> CacheEntry {
        CacheEntry {
            request: request(url),
            response: CachedResponse {
                kind: CachedResponseType::Default,
                metadata: CachedResponseMetadata {
                    request_includes_credentials: true,
                    url: None,
                    status: 200,
                    status_text: b"OK".to_vec(),
                    headers: Vec::new(),
                    redirected: false,
                },
                internal_metadata: None,
                body: Some(bytes.to_vec()),
            },
        }
    }
    fn open(db: &mut CacheDatabase, name: &str) -> CacheIdentifier {
        match db
            .run(CacheOperation::Open(name.encode_utf16().collect()))
            .unwrap()
        {
            CacheResult::Opened(cache) => cache,
            _ => panic!("wrong result"),
        }
    }
    fn query(
        db: &mut CacheDatabase,
        cache: CacheIdentifier,
        url: &str,
        options: CacheQuery,
    ) -> Vec<CacheEntry> {
        match db
            .run(CacheOperation::Query {
                cache,
                request: Some(request(url)),
                options,
            })
            .unwrap()
        {
            CacheResult::Entries(entries) => entries,
            _ => panic!("wrong result"),
        }
    }
    #[test]
    fn cache_persists_binary_bodies_and_name_order_after_reopening() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.sqlite3");
        let mut db = CacheDatabase::open(&path, 7).unwrap();
        let cache = open(&mut db, "../first\0name");
        open(&mut db, "second");
        db.run(CacheOperation::Put {
            cache,
            entries: vec![entry(
                "https://example.org/a?q=1#fragment",
                &[0, 255, 13, 0],
            )],
        })
        .unwrap();
        drop(db);
        let mut db = CacheDatabase::open(&path, 7).unwrap();
        let entries = query(
            &mut db,
            cache,
            "https://example.org/a?q=1#different",
            CacheQuery::default(),
        );
        assert_eq!(
            entries[0].response.body.as_deref(),
            Some(&[0, 255, 13, 0][..])
        );
        match db.run(CacheOperation::StorageKeys).unwrap() {
            CacheResult::Names(names) => assert_eq!(
                names,
                ["../first\0name", "second"].map(|name| name.encode_utf16().collect::<Vec<_>>())
            ),
            _ => panic!("wrong result"),
        }
        assert!(
            query(
                &mut db,
                cache,
                "https://example.org/a?q=2",
                CacheQuery::default()
            )
            .is_empty()
        );
        assert_eq!(
            query(
                &mut db,
                cache,
                "https://example.org/a?q=2",
                CacheQuery {
                    ignore_search: true,
                    ..Default::default()
                }
            )
            .len(),
            1
        );
    }
    #[test]
    fn deleting_name_does_not_retarget_old_handles_and_orphans_are_collected_on_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.sqlite3");
        let mut db = CacheDatabase::open(&path, 8).unwrap();
        let old = open(&mut db, "same");
        db.run(CacheOperation::DeleteCache("same".encode_utf16().collect()))
            .unwrap();
        let new = open(&mut db, "same");
        assert_ne!(old, new);
        db.run(CacheOperation::Put {
            cache: old,
            entries: vec![entry("https://example.org/a", b"old")],
        })
        .unwrap();
        assert_eq!(
            query(&mut db, old, "https://example.org/a", CacheQuery::default()).len(),
            1
        );
        assert!(query(&mut db, new, "https://example.org/a", CacheQuery::default()).is_empty());
        drop(db);
        let mut db = CacheDatabase::open(&path, 8).unwrap();
        assert!(matches!(
            db.run(CacheOperation::Query {
                cache: old,
                request: None,
                options: Default::default()
            }),
            Err(Error::InvalidState)
        ));
        assert!(
            db.check_identity(CacheIdentifier {
                bottle_id: 99,
                id: new.id
            })
            .is_err()
        );
    }
    #[test]
    fn batches_are_atomic_and_replacement_moves_to_end() {
        let temp = tempfile::tempdir().unwrap();
        let mut db = CacheDatabase::open(&temp.path().join("cache.sqlite3"), 1).unwrap();
        let cache = open(&mut db, "batch");
        db.run(CacheOperation::Put {
            cache,
            entries: vec![
                entry("https://example.org/a", b"before"),
                entry("https://example.org/b", b"b"),
            ],
        })
        .unwrap();
        assert!(matches!(
            db.run(CacheOperation::Put {
                cache,
                entries: vec![
                    entry("https://example.org/a", b"new"),
                    entry("https://example.org/a#same", b"duplicate")
                ]
            }),
            Err(Error::InvalidState)
        ));
        assert_eq!(
            query(&mut db, cache, "https://example.org/a", Default::default())[0]
                .response
                .body
                .as_deref(),
            Some(&b"before"[..])
        );
        db.run(CacheOperation::Put {
            cache,
            entries: vec![entry("https://example.org/a", b"after")],
        })
        .unwrap();
        match db
            .run(CacheOperation::Query {
                cache,
                request: None,
                options: Default::default(),
            })
            .unwrap()
        {
            CacheResult::Entries(entries) => assert_eq!(
                entries
                    .iter()
                    .map(|e| e.request.url.url().to_string())
                    .collect::<Vec<_>>(),
                ["https://example.org/b", "https://example.org/a"]
            ),
            _ => panic!("wrong result"),
        }
    }
    #[test]
    fn vary_combines_headers_preserves_missing_values_and_respects_query_options() {
        let mut stored = entry("https://example.org/a", b"data");
        stored
            .response
            .metadata
            .headers
            .push(("Vary".into(), b"Accept-Language, X-Empty".to_vec()));
        stored
            .request
            .headers
            .insert("accept-language", "en".parse().unwrap());
        let mut query = request("https://example.org/a");
        query
            .headers
            .insert("accept-language", "en".parse().unwrap());
        assert!(matches(&query, &stored, Default::default()));
        query.headers.insert("x-empty", "".parse().unwrap());
        assert!(!matches(&query, &stored, Default::default()));
        assert!(matches(
            &query,
            &stored,
            CacheQuery {
                ignore_vary: true,
                ..Default::default()
            }
        ));
        query.method = "POST".parse().unwrap();
        assert!(!matches(
            &query,
            &stored,
            CacheQuery {
                ignore_vary: true,
                ..Default::default()
            }
        ));
        assert!(matches(
            &query,
            &stored,
            CacheQuery {
                ignore_vary: true,
                ignore_method: true,
                ..Default::default()
            }
        ));
        stored.response.metadata.headers[0].1 = b"*".to_vec();
        assert!(!matches(
            &request("https://example.org/a"),
            &stored,
            Default::default()
        ));
    }
    #[test]
    fn cache_names_preserve_unpaired_surrogates_across_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.sqlite3");
        let mut db = CacheDatabase::open(&path, 17).unwrap();
        let names = vec![
            vec![0xd800],
            vec![0xfffd],
            vec![0xdc00],
            vec![0, 0xd800, 65],
        ];
        for name in &names {
            db.run(CacheOperation::Open(name.clone())).unwrap();
        }
        drop(db);
        let mut db = CacheDatabase::open(&path, 17).unwrap();
        match db.run(CacheOperation::StorageKeys).unwrap() {
            CacheResult::Names(found) => assert_eq!(found, names),
            _ => panic!("wrong result"),
        }
        assert!(matches!(
            db.run(CacheOperation::Has(names[0].clone())).unwrap(),
            CacheResult::Boolean(true)
        ));
        db.run(CacheOperation::DeleteCache(names[0].clone()))
            .unwrap();
        assert!(matches!(
            db.run(CacheOperation::Has(names[0].clone())).unwrap(),
            CacheResult::Boolean(false)
        ));
        assert!(matches!(
            db.run(CacheOperation::Has(names[1].clone())).unwrap(),
            CacheResult::Boolean(true)
        ));
    }
    #[test]
    fn old_cache_records_keep_names_bodies_and_metadata_after_format_upgrade() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE cache_lists(id INTEGER PRIMARY KEY AUTOINCREMENT,name BLOB UNIQUE); CREATE TABLE cache_entries(sequence INTEGER PRIMARY KEY AUTOINCREMENT, cache_id INTEGER NOT NULL REFERENCES cache_lists(id) ON DELETE CASCADE, record BLOB NOT NULL);").unwrap();
        let name = vec![65, 0xd800];
        connection
            .execute(
                "INSERT INTO cache_lists(name) VALUES (?1)",
                [encode_name(&name)],
            )
            .unwrap();
        let old = LegacyCacheEntry {
            request: request("https://example.org/legacy"),
            response: LegacyCachedResponse {
                kind: CachedResponseType::Default,
                metadata: LegacyCachedResponseMetadata {
                    url: None,
                    status: 201,
                    status_text: b"Legacy".to_vec(),
                    headers: vec![("x-legacy".into(), b"preserved".to_vec())],
                    redirected: false,
                },
                internal_metadata: None,
                body: Some(vec![0, 255, 128]),
            },
        };
        connection
            .execute(
                "INSERT INTO cache_entries(cache_id,record) VALUES (1,?1)",
                [postcard::to_stdvec(&old).unwrap()],
            )
            .unwrap();
        drop(connection);
        let mut db = CacheDatabase::open(&path, 17).unwrap();
        match db.run(CacheOperation::StorageKeys).unwrap() {
            CacheResult::Names(names) => assert_eq!(names, vec![name]),
            _ => panic!("wrong result"),
        };
        let identity = db.identity(1);
        let records = query(
            &mut db,
            identity,
            "https://example.org/legacy",
            CacheQuery::default(),
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].response.body, Some(vec![0, 255, 128]));
        assert_eq!(records[0].response.metadata.status, 201);
        assert_eq!(records[0].response.metadata.status_text, b"Legacy");
        assert_eq!(
            records[0].response.metadata.headers,
            vec![("x-legacy".into(), b"preserved".to_vec())]
        );
        assert!(records[0].response.metadata.request_includes_credentials);
        db.run(CacheOperation::Put {
            cache: identity,
            entries: records,
        })
        .unwrap();
        let version: i64 = db
            .connection
            .query_row("SELECT format_version FROM cache_entries", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, 2);
        drop(db);
        let mut db = CacheDatabase::open(&path, 17).unwrap();
        assert_eq!(
            query(
                &mut db,
                identity,
                "https://example.org/legacy",
                CacheQuery::default()
            )[0]
            .response
            .body,
            Some(vec![0, 255, 128])
        );
    }
}
