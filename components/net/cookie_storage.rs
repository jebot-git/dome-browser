/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Implementation of cookie storage as specified in
//! <http://tools.ietf.org/html/rfc6265>

use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::net::IpAddr;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::time::SystemTime;

use cookie::Cookie;
use itertools::Itertools;
use malloc_size_of_derive::MallocSizeOf;
use net_traits::pub_domains::reg_suffix;
use net_traits::{CookieContext, CookieSource, SiteDescriptor};
use parking_lot::{RwLock, RwLockWriteGuard};
use serde::{Deserialize, Serialize};
use servo_url::ServoUrl;

use crate::cookie::ServoCookie;
use crate::cookie_file::{CookieFile, HEADER};

#[derive(Clone, Debug, Deserialize, Serialize, MallocSizeOf)]
pub struct CookieStorage {
    version: u32,
    cookies_map: HashMap<String, Vec<ServoCookie>>,
    max_per_host: usize,
    #[serde(skip)]
    #[ignore_malloc_size_of = "Cookie file bookkeeping"]
    persistence: Option<CookieFile>,
    #[serde(skip)]
    dirty: bool,
}

/// Serializes cookie access with external-file reload and durable mutation
/// persistence. Private/in-memory jars have no attached file.
pub struct CookieAccess<'a>(RwLockWriteGuard<'a, CookieStorage>);

impl Deref for CookieAccess<'_> {
    type Target = CookieStorage;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl DerefMut for CookieAccess<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}
impl Drop for CookieAccess<'_> {
    fn drop(&mut self) {
        if let Some(mut file) = self.0.persistence.take() {
            file.flush(&mut self.0);
            self.0.persistence = Some(file);
        }
    }
}

#[derive(Debug)]
pub enum RemoveCookieError {
    Overlapping,
    NonHTTP,
}

impl CookieStorage {
    pub fn new(max_cookies: usize) -> CookieStorage {
        CookieStorage {
            version: 1,
            cookies_map: HashMap::new(),
            max_per_host: max_cookies,
            persistence: None,
            dirty: false,
        }
    }

    pub fn load_from_profile(directory: &Path, max_cookies: usize) -> Self {
        let mut storage = Self::new(max_cookies);
        storage.persistence = Some(CookieFile::attach(directory, &mut storage));
        storage
    }

    pub fn access(jar: &RwLock<Self>) -> CookieAccess<'_> {
        let mut storage = jar.write();
        if let Some(mut file) = storage.persistence.take() {
            file.refresh(&mut storage);
            storage.persistence = Some(file);
        }
        CookieAccess(storage)
    }

    pub(crate) fn file_capacity(&self) -> usize { self.max_per_host }
    pub(crate) fn file_dirty(&self) -> bool { self.dirty }
    pub(crate) fn file_clean(&mut self) { self.dirty = false; }
    pub(crate) fn replace_file_records(&mut self, other: Self) {
        self.cookies_map = other.cookies_map;
        self.dirty = false;
    }
    pub(crate) fn file_records(&self) -> Vec<ServoCookie> {
        self.cookies_map.values().flatten().cloned().collect()
    }

    pub(crate) fn from_file_records(records: Vec<ServoCookie>, capacity: usize) -> std::io::Result<Self> {
        let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid cookie record");
        let mut result = Self::new(capacity);
        for mut record in records {
            // Retire unsafe SameSite=None records accepted by older builds,
            // without making unrelated valid cookies unavailable on upgrade.
            if record.cookie.same_site() == Some(cookie::SameSite::None) && !record.cookie.secure().unwrap_or(false) {
                continue;
            }
            let domain = record.cookie.domain().ok_or_else(invalid)?.to_owned();
            let url = ServoUrl::parse(&format!("https://{domain}/")).map_err(|_| invalid())?;
            let canonical_domain = url.host_str().ok_or_else(invalid)?;
            if !canonical_domain.eq_ignore_ascii_case(&domain) || !url.username().is_empty() || url.password().is_some() {
                return Err(invalid());
            }
            record.cookie.set_domain(canonical_domain.to_owned());
            let domain = record.cookie.domain().ok_or_else(invalid)?;
            let path = record.cookie.path().ok_or_else(invalid)?;
            if domain.is_empty() || !path.starts_with('/') ||
                record.cookie.to_string().chars().any(|c| c.is_control()) {
                return Err(invalid());
            }
            let mut validation_cookie = record.cookie.clone();
            if record.host_only { validation_cookie.unset_domain(); }
            // This pass checks domain/prefix/security structure, not receipt
            // time. Stored absolute expiry remains authoritative.
            validation_cookie.set_max_age(None);
            validation_cookie.set_expires(None);
            let validated = ServoCookie::new_wrapped(validation_cookie, &url, CookieSource::HTTP)
                .ok_or_else(invalid)?;
            if url.host_str() != Some(domain) || !url.username().is_empty() || url.password().is_some() ||
                validated.host_only != record.host_only || validated.cookie.domain() != Some(domain) {
                return Err(invalid());
            }
            let bucket = result.cookies_map.entry(reg_host(domain)).or_default();
            if bucket.len() >= capacity || bucket.iter().any(|old| old.cookie.domain() == Some(domain) &&
                old.cookie.path() == Some(path) && old.cookie.name() == record.cookie.name()) {
                return Err(invalid());
            }
            // Preserve stored absolute expiry and security flags. Reconstructing
            // via new_wrapped would restart Max-Age on every file reload.
            if !is_cookie_expired(&record) { bucket.push(record); }
        }
        Ok(result)
    }

    pub(crate) fn parse_file(text: &str, capacity: usize) -> std::io::Result<Self> {
        let records = text.lines().filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
            .map(|line| serde_json::from_str::<ServoCookie>(line).map_err(|_|
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid cookie record")))
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_file_records(records, capacity)
    }

    pub(crate) fn file_text(&self) -> std::io::Result<String> {
        let mut records = self.file_records();
        records.retain(|c| !is_cookie_expired(c));
        records.sort_by(|a, b| (a.cookie.domain(), a.cookie.path(), a.cookie.name())
            .cmp(&(b.cookie.domain(), b.cookie.path(), b.cookie.name())));
        let mut text = HEADER.to_owned();
        for record in records {
            text.push_str(&serde_json::to_string(&record).map_err(|_|
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Cannot encode cookie record"))?);
            text.push('\n');
        }
        Ok(text)
    }

    // http://tools.ietf.org/html/rfc6265#section-5.3
    pub fn remove(
        &mut self,
        cookie: &ServoCookie,
        url: &ServoUrl,
        source: CookieSource,
    ) -> Result<Option<ServoCookie>, RemoveCookieError> {
        let domain = reg_host(cookie.cookie.domain().as_ref().unwrap_or(&""));
        let cookies = self.cookies_map.entry(domain).or_default();

        // https://www.ietf.org/id/draft-ietf-httpbis-cookie-alone-01.txt Step 2
        if !cookie.cookie.secure().unwrap_or(false) && !url.is_secure_scheme() {
            let new_domain = cookie.cookie.domain().as_ref().unwrap().to_owned();
            let new_path = cookie.cookie.path().as_ref().unwrap().to_owned();

            let any_overlapping = cookies.iter().any(|c| {
                let existing_domain = c.cookie.domain().as_ref().unwrap().to_owned();
                let existing_path = c.cookie.path().as_ref().unwrap().to_owned();

                c.cookie.name() == cookie.cookie.name() &&
                    c.cookie.secure().unwrap_or(false) &&
                    (ServoCookie::domain_match(new_domain, existing_domain) ||
                        ServoCookie::domain_match(existing_domain, new_domain)) &&
                    ServoCookie::path_match(new_path, existing_path)
            });

            if any_overlapping {
                return Err(RemoveCookieError::Overlapping);
            }
        }

        // Step 11.1
        let position = cookies.iter().position(|c| {
            c.cookie.domain() == cookie.cookie.domain() &&
                c.cookie.path() == cookie.cookie.path() &&
                c.cookie.name() == cookie.cookie.name()
        });

        if let Some(ind) = position {
            // Step 11.4
            let c = cookies.remove(ind);
            self.dirty = true;

            // http://tools.ietf.org/html/rfc6265#section-5.3 step 11.2
            if c.cookie.http_only().unwrap_or(false) && source.is_non_http() {
                // Undo the removal.
                cookies.push(c);
                Err(RemoveCookieError::NonHTTP)
            } else {
                Ok(Some(c))
            }
        } else {
            Ok(None)
        }
    }

    pub fn delete_cookies_for_sites(&mut self, sites: &Vec<String>) {
        self.dirty = true;
        // Note: We assume the number of sites is smaller than the number of
        // entries in the cookies map. If this assumption stops holding in
        // practice, this implementation can be revised to use `retain`
        // together with a temporary `HashSet` of sites.
        for site in sites {
            // TODO: We currently mark cookies as expired instead of removing
            // them immediately (same behavior as in the functions below).
            // This is safe because higher-level cookie accessors always call
            // `remove_expired_cookies_for_url` / `remove_all_expired_cookies`.
            // Consider whether we should instead delete the entries directly.
            if let Some(cookies) = self.cookies_map.get_mut(site) {
                for cookie in cookies.iter_mut() {
                    cookie.set_expiry_time_in_past();
                }
            }
        }
    }

    pub fn clear_session_cookies(&mut self) {
        self.dirty = true;
        self.cookies_map
            .values_mut()
            .flat_map(|cookies| cookies.iter_mut())
            .filter(|cookie| !cookie.persistent)
            .for_each(|cookie| cookie.set_expiry_time_in_past());
    }

    pub fn clear_storage(&mut self, url: Option<&ServoUrl>) {
        self.dirty = true;
        if let Some(url) = url {
            let domain = reg_host(url.host_str().unwrap_or(""));
            if let Some(cookies) = self.cookies_map.get_mut(&domain) {
                for cookie in cookies.iter_mut() {
                    cookie.set_expiry_time_in_past();
                }
            }
        } else {
            self.cookies_map.clear();
        }
    }

    pub fn delete_cookie_with_name(&mut self, url: &ServoUrl, name: String) {
        self.dirty = true;
        let domain = reg_host(url.host_str().unwrap_or(""));
        if let Some(cookies) = self.cookies_map.get_mut(&domain) {
            for cookie in cookies.iter_mut() {
                if cookie.cookie.name() == name {
                    cookie.set_expiry_time_in_past();
                }
            }
        }
    }

    // http://tools.ietf.org/html/rfc6265#section-5.3
    pub fn push(&mut self, mut cookie: ServoCookie, url: &ServoUrl, source: CookieSource) {
        // https://www.ietf.org/id/draft-ietf-httpbis-cookie-alone-01.txt Step 1
        if cookie.cookie.secure().unwrap_or(false) && !url.is_secure_scheme() {
            return;
        }

        let old_cookie = self.remove(&cookie, url, source);
        if old_cookie.is_err() {
            // This new cookie is not allowed to overwrite an existing one.
            return;
        }

        // Step 11
        if let Some(old_cookie) = old_cookie.unwrap() {
            // Step 11.3
            cookie.creation_time = old_cookie.creation_time;
        }

        // Step 12
        let domain = reg_host(cookie.cookie.domain().as_ref().unwrap_or(&""));
        let cookies = self.cookies_map.entry(domain).or_default();

        if cookies.len() == self.max_per_host {
            let old_len = cookies.len();
            cookies.retain(|c| !is_cookie_expired(c));
            let new_len = cookies.len();

            // https://www.ietf.org/id/draft-ietf-httpbis-cookie-alone-01.txt
            if new_len == old_len &&
                !evict_one_cookie(cookie.cookie.secure().unwrap_or(false), cookies)
            {
                return;
            }
        }
        cookies.push(cookie);
        self.dirty = true;
    }

    pub fn cookie_comparator(a: &ServoCookie, b: &ServoCookie) -> Ordering {
        let a_path_len = a.cookie.path().as_ref().map_or(0, |p| p.len());
        let b_path_len = b.cookie.path().as_ref().map_or(0, |p| p.len());
        match a_path_len.cmp(&b_path_len) {
            Ordering::Equal => a.creation_time.cmp(&b.creation_time),
            // Ensure that longer paths are sorted earlier than shorter paths
            Ordering::Greater => Ordering::Less,
            Ordering::Less => Ordering::Greater,
        }
    }

    pub fn remove_expired_cookies_for_url(&mut self, url: &ServoUrl) {
        let domain = reg_host(url.host_str().unwrap_or(""));
        if let Entry::Occupied(mut entry) = self.cookies_map.entry(domain) {
            let cookies = entry.get_mut();
            let old_len = cookies.len();
            cookies.retain(|c| !is_cookie_expired(c));
            self.dirty |= cookies.len() != old_len;
            if cookies.is_empty() {
                entry.remove_entry();
            }
        }
    }

    pub fn remove_all_expired_cookies(&mut self) {
        let dirty = &mut self.dirty;
        self.cookies_map.retain(|_, cookies| {
            let old_len = cookies.len();
            cookies.retain(|c| !is_cookie_expired(c));
            *dirty |= cookies.len() != old_len;
            !cookies.is_empty()
        });
    }

    // http://tools.ietf.org/html/rfc6265#section-5.4
    pub fn cookies_for_url(&mut self, url: &ServoUrl, source: CookieSource) -> Option<String> {
        self.cookies_for_url_with_context(url, source, CookieContext::same_site())
    }

    pub fn cookies_for_url_with_context(&mut self, url: &ServoUrl, source: CookieSource, context: CookieContext) -> Option<String> {
        // Let cookie-list be the set of cookies from the cookie store
        let cookie_list = self.cookies_data_for_url_with_context(url, source, context);

        let reducer = |acc: String, cookie: Cookie<'static>| -> String {
            // Serialize the cookie-list into a cookie-string by processing each cookie in the cookie-list in order:
            // If the cookies' name is not empty, output the cookie's name followed by the %x3D ("=") character.
            // If the cookies' value is not empty, output the cookie's value.
            // If there is an unprocessed cookie in the cookie-list, output the characters %x3B and %x20 ("; ").
            // Security: the above steps allow for "nameless" cookies which have proved to be a security footgun
            // especially with the new cookie name prefix proposals
            (match acc.len() {
                0 => acc,
                _ => acc + "; ",
            }) + cookie.name() +
                "=" +
                cookie.value()
        };

        // Serialize the cookie-list into a cookie-string by processing each cookie in the cookie-list in order
        let result = cookie_list.fold(String::new(), reducer);

        match result.len() {
            0 => None,
            _ => Some(result),
        }
    }

    /// <https://cookiestore.spec.whatwg.org/#query-cookies>
    pub fn query_cookies(&mut self, url: &ServoUrl, name: Option<String>) -> Vec<Cookie<'static>> {
        self.query_cookies_for_source(url, name, CookieSource::NonHTTP)
    }

    pub fn query_cookies_for_source(&mut self, url: &ServoUrl, name: Option<String>, source: CookieSource) -> Vec<Cookie<'static>> {
        // 1. Retrieve cookie-list given request-uri and "non-HTTP" source
        let cookie_list = self.cookies_data_for_url(url, source);

        // 3. For each cookie in cookie-list, run these steps:
        // 3.2. If name is given, then run these steps:
        if let Some(name) = name {
            // Let cookieName be the result of running UTF-8 decode without BOM on cookie’s name.
            // If cookieName does not equal name, then continue.
            cookie_list.filter(|cookie| cookie.name() == name).collect()
        } else {
            cookie_list.collect()
        }

        // Note: we do not convert the list into CookieListItem's here, we do that in script to not not have to define
        // the binding types in net.

        // Return list
    }

    pub fn cookies_data_for_url<'a>(
        &'a mut self,
        url: &'a ServoUrl,
        source: CookieSource,
    ) -> impl Iterator<Item = cookie::Cookie<'static>> + 'a {
        self.cookies_data_for_url_with_context(url, source, CookieContext::same_site())
    }

    pub fn cookies_data_for_url_with_context<'a>(
        &'a mut self,
        url: &'a ServoUrl,
        source: CookieSource,
        context: CookieContext,
    ) -> impl Iterator<Item = cookie::Cookie<'static>> + 'a {
        let domain = reg_host(url.host_str().unwrap_or(""));
        let cookies = self.cookies_map.entry(domain).or_default();

        cookies
            .iter_mut()
            .filter(move |c| c.appropriate_for_url(url, source) && c.same_site_allows(context, source))
            .sorted_by(|a: &&mut ServoCookie, b: &&mut ServoCookie| {
                // The user agent SHOULD sort the cookie-list
                CookieStorage::cookie_comparator(a, b)
            })
            .map(|c| {
                // Update the last-access-time of each cookie in the cookie-list to the current date and time
                c.touch();
                c.cookie.clone()
            })
    }

    pub fn cookie_site_descriptors(&self) -> Vec<SiteDescriptor> {
        self.cookies_map
            .keys()
            .cloned()
            .map(SiteDescriptor::new)
            .collect()
    }
}

fn reg_host(url: &str) -> String {
    let host_for_ip_parse = url
        .strip_prefix('[')
        .and_then(|url| url.strip_suffix(']'))
        .unwrap_or(url);
    if let Ok(address) = host_for_ip_parse.parse::<IpAddr>() {
        debug_assert!(address.to_string() == address.to_string().to_lowercase());
        return address.to_string();
    }

    reg_suffix(url).to_lowercase()
}

fn is_cookie_expired(cookie: &ServoCookie) -> bool {
    matches!(cookie.expiry_time, Some(date_time) if date_time <= SystemTime::now())
}

fn evict_one_cookie(is_secure_cookie: bool, cookies: &mut Vec<ServoCookie>) -> bool {
    // Remove non-secure cookie with oldest access time
    let oldest_accessed = get_oldest_accessed(false, cookies);

    if let Some((index, _)) = oldest_accessed {
        cookies.remove(index);
    } else {
        // All secure cookies were found
        if !is_secure_cookie {
            return false;
        }
        let oldest_accessed = get_oldest_accessed(true, cookies);
        if let Some((index, _)) = oldest_accessed {
            cookies.remove(index);
        }
    }
    true
}

fn get_oldest_accessed(
    is_secure_cookie: bool,
    cookies: &mut [ServoCookie],
) -> Option<(usize, SystemTime)> {
    let mut oldest_accessed = None;
    for (i, c) in cookies.iter().enumerate() {
        if (c.cookie.secure().unwrap_or(false) == is_secure_cookie) &&
            oldest_accessed
                .as_ref()
                .is_none_or(|(_, current_oldest_time)| c.last_access < *current_oldest_time)
        {
            oldest_accessed = Some((i, c.last_access));
        }
    }
    oldest_accessed
}
