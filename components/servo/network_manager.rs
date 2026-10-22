/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashSet;

use net_traits::ResourceThreads;
use servo_constellation_traits::EmbedderToConstellationMessage;

use crate::proxies::ConstellationProxy;

/// Streaming events for a user-requested resource download. Callbacks run on
/// the fetch thread, never the UI thread. Returning false cancels the request.
pub enum ResourceDownloadEvent {
    Response { url: url::Url, content_type: String, content_disposition: String },
    Data(Vec<u8>),
    Finished(Result<(), String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CacheEntry {
    key: String,
}

impl CacheEntry {
    pub fn new(key: String) -> Self {
        Self { key }
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Provides APIs for managing network-related state.
///
/// `NetworkManager` is responsible for data owned by the networking layer,
/// such as the HTTP cache. This data is not considered site data and is
/// therefore intentionally separate from `SiteDataManager`.
pub struct NetworkManager {
    public_resource_threads: ResourceThreads,
    private_resource_threads: ResourceThreads,
    constellation_proxy: ConstellationProxy,
}

impl NetworkManager {
    /// Fetch a resource using the browser's cookies, cache, TLS and HTTP auth.
    /// This privileged API is for explicit native user actions, not web scripts.
    /// Blob URLs require a document-owned claim and are not accepted here.
    pub fn download_resource(
        &self,
        webview_id: crate::WebViewId,
        url: url::Url,
        source: url::Url,
        mut callback: impl FnMut(ResourceDownloadEvent) -> bool + Send + 'static,
    ) -> Result<(), String> {
        use net_traits::request::{CredentialsMode, Initiator, Referrer, RequestBuilder};
        use net_traits::{FetchResponseMsg, cancel_async_fetch, fetch_async};
        use servo_url::ServoUrl;
        if !matches!(url.scheme(), "http" | "https" | "data") &&
            !(url.scheme() == "file" && source.scheme() == "file") {
            return Err(format!("Saving {} URLs is not supported", url.scheme()));
        }
        let source = ServoUrl::from_url(source);
        let request = RequestBuilder::new(Some(webview_id),
            net_traits::blob_url_store::UrlWithBlobClaim::for_url(ServoUrl::from_url(url))
                .map_err(|_| "Blob URLs need a document-owned claim")?,
            Referrer::ReferrerUrl(source.clone()))
            .origin(source.origin()).credentials_mode(CredentialsMode::Include)
            .initiator(Initiator::Download);
        let id = request.id;
        let thread = self.public_resource_threads.core_thread.clone();
        let cancel_thread = thread.clone();
        let mut stopped = false;
        fetch_async(&thread, request, None, Box::new(move |message| {
            if stopped { return; }
            let event = match message {
                FetchResponseMsg::ProcessResponse(_, Ok(metadata)) => {
                    let metadata = metadata.metadata();
                    if !metadata.status.is_success() {
                        ResourceDownloadEvent::Finished(Err(format!("HTTP {}", metadata.status.raw_code())))
                    } else {
                        let header = |name: &str| metadata.headers.as_ref()
                            .and_then(|h| h.get(name)).and_then(|v| v.to_str().ok())
                            .unwrap_or("").to_owned();
                        ResourceDownloadEvent::Response { url: metadata.final_url.clone().into_url(),
                            content_type: header("content-type"), content_disposition: header("content-disposition") }
                    }
                },
                FetchResponseMsg::ProcessResponse(_, Err(error)) => ResourceDownloadEvent::Finished(Err(format!("{error:?}"))),
                FetchResponseMsg::ProcessResponseChunk(_, bytes) => ResourceDownloadEvent::Data(bytes.to_vec()),
                FetchResponseMsg::ProcessResponseEOF(_, result, _) => ResourceDownloadEvent::Finished(result.map_err(|e| format!("{e:?}"))),
                _ => return,
            };
            let finished = matches!(event, ResourceDownloadEvent::Finished(_));
            if !callback(event) || finished {
                stopped = true;
                cancel_async_fetch(vec![id], &cancel_thread);
            }
        }));
        Ok(())
    }
    pub(crate) fn new(
        public_resource_threads: ResourceThreads,
        private_resource_threads: ResourceThreads,
        constellation_proxy: ConstellationProxy,
    ) -> Self {
        Self {
            public_resource_threads,
            private_resource_threads,
            constellation_proxy,
        }
    }

    /// Returns cache entries currently stored in the HTTP cache.
    ///
    /// The returned list contains one [`CacheEntry`] per unique cache key
    /// (URL) for which the networking layer currently maintains cached
    /// responses.
    ///
    /// Both public and private browsing contexts are included in the result.
    ///
    /// Note: The networking layer currently only implements an in-memory HTTP
    /// cache. Support for an on-disk cache is under development.
    pub fn cache_entries(&self) -> Vec<CacheEntry> {
        let public_entries = self.public_resource_threads.cache_entries();
        let private_entries = self.private_resource_threads.cache_entries();

        let unique_keys: HashSet<String> = public_entries
            .into_iter()
            .chain(private_entries)
            .map(|entry| entry.key)
            .collect();

        unique_keys.into_iter().map(CacheEntry::new).collect()
    }

    /// Clears the network (HTTP) cache.
    ///
    /// This removes all cached network responses maintained by the networking
    /// layer for both public and private browsing contexts.
    ///
    /// Note: The networking layer currently only implements an in-memory HTTP
    /// cache. Support for an on-disk cache is under development.
    pub fn clear_cache(&self) {
        self.public_resource_threads.clear_cache();
        self.private_resource_threads.clear_cache();
    }

    /// Set the network online state. This controls the value of [`navigator.onLine`]
    /// and may fire `online` and `offline` events in web content.
    ///
    /// [`navigator.onLine`]: https://html.spec.whatwg.org/multipage/#dom-navigator-online
    pub fn set_online_state(&self, online: bool) {
        self.constellation_proxy
            .send(EmbedderToConstellationMessage::SetNetworkOnlineState(
                online,
            ));
    }
}
