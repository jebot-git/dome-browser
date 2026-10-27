/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::ops::{Deref, DerefMut};

use malloc_size_of_derive::MallocSizeOf;
use net_traits::request::RequestBuilder as Request;
use serde::{Deserialize, Serialize};
use servo_base::generic_channel::{GenericCallback, GenericSender};
use servo_url::{ImmutableOrigin, ServoUrl};

use crate::client_storage::StorageProxyMap;

#[derive(Debug, Deserialize, Serialize)]
pub enum CacheStorageError<T> {
    Internal(T),
}

#[derive(Clone, Debug, Deserialize, MallocSizeOf, Serialize)]
pub struct CacheStorageThreadHandle {
    sender: GenericSender<CacheStorageThreadMessage>,
}

impl CacheStorageThreadHandle {
    pub fn new(sender: GenericSender<CacheStorageThreadMessage>) -> Self {
        CacheStorageThreadHandle { sender }
    }
}

impl From<CacheStorageThreadHandle> for GenericSender<CacheStorageThreadMessage> {
    fn from(handle: CacheStorageThreadHandle) -> Self {
        handle.sender
    }
}

impl From<GenericSender<CacheStorageThreadMessage>> for CacheStorageThreadHandle {
    fn from(sender: GenericSender<CacheStorageThreadMessage>) -> Self {
        CacheStorageThreadHandle::new(sender)
    }
}

impl Deref for CacheStorageThreadHandle {
    type Target = GenericSender<CacheStorageThreadMessage>;

    fn deref(&self) -> &Self::Target {
        &self.sender
    }
}

impl DerefMut for CacheStorageThreadHandle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sender
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum CacheStorageThreadMessage {
    Run {
        origin: ImmutableOrigin,
        proxy: StorageProxyMap,
        operation: CacheOperation,
        callback: GenericCallback<Result<CacheResult, CacheFailure>>,
    },
    Exit(GenericSender<()>),
}

/// A cache is a request/response list. Names can be removed while existing
/// objects continue to reference this list, so a name is not its identity.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, MallocSizeOf, PartialEq, Eq)]
pub struct CacheIdentifier {
    pub bottle_id: i64,
    pub id: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct CacheQuery {
    pub ignore_search: bool,
    pub ignore_method: bool,
    pub ignore_vary: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, MallocSizeOf, PartialEq, Eq)]
pub enum CachedResponseType {
    Default,
    Basic,
    Cors,
    Opaque,
    OpaqueRedirect,
    Error,
}

#[derive(Clone, Debug, Deserialize, Serialize, MallocSizeOf)]
pub struct CachedResponseMetadata {
    pub request_includes_credentials: bool,
    pub url: Option<ServoUrl>,
    pub status: u16,
    pub status_text: Vec<u8>,
    pub headers: Vec<(String, Vec<u8>)>,
    pub redirected: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CachedResponse {
    pub kind: CachedResponseType,
    pub metadata: CachedResponseMetadata,
    /// Filtered response metadata is the public view. Keep the internal
    /// metadata/body separately without exposing it through DOM Headers/Body.
    pub internal_metadata: Option<CachedResponseMetadata>,
    pub body: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CacheEntry {
    pub request: Request,
    pub response: CachedResponse,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum CacheOperation {
    Open(Vec<u16>),
    Has(Vec<u16>),
    DeleteCache(Vec<u16>),
    StorageKeys,
    StorageMatch {
        request: Request,
        options: CacheQuery,
        cache_name: Option<Vec<u16>>,
    },
    Query {
        cache: CacheIdentifier,
        request: Option<Request>,
        options: CacheQuery,
    },
    DeleteEntries {
        cache: CacheIdentifier,
        request: Request,
        options: CacheQuery,
    },
    Put {
        cache: CacheIdentifier,
        entries: Vec<CacheEntry>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum CacheResult {
    Opened(CacheIdentifier),
    Boolean(bool),
    Names(Vec<Vec<u16>>),
    Entries(Vec<CacheEntry>),
    Done,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum CacheFailure {
    Storage(String),
    Type(String),
    InvalidState,
    QuotaExceeded,
}
