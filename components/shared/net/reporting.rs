/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Response-local Reporting API endpoint configuration.

use http::HeaderMap;
use malloc_size_of_derive::MallocSizeOf;
use serde::ser::Error;
use serde::{Serialize, Serializer};
use servo_url::ServoUrl;
use sfv::{BareItem, Dictionary, ListEntry, Parser};

use crate::fetch::headers::get_value_from_header_list;

/// The response outcome defined by Reporting's delivery algorithm. Attempted
/// reports are consumed for every outcome; this does not request a retry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    Success,
    Failure,
    RemoveEndpoint,
}

impl DeliveryOutcome {
    pub fn from_status(status: Option<u16>) -> Self {
        match status {
            Some(200..=299) => Self::Success,
            Some(410) => Self::RemoveEndpoint,
            _ => Self::Failure,
        }
    }

    /// None retires this endpoint; successful responses end a failure streak.
    pub fn updated_failures(self, failures: u32) -> Option<u32> {
        match self {
            Self::Success => Some(0),
            Self::Failure => Some(failures.saturating_add(1)),
            Self::RemoveEndpoint => None,
        }
    }
}

/// Build the Reporting transport request without a Fetch client or a window.
/// Browser cookie policy retains its ancestor-aware provenance separately.
pub fn reporting_request(
    url: crate::blob_url_store::UrlWithBlobClaim,
    origin: servo_url::ImmutableOrigin,
    source: crate::request::RequestClient,
    pipeline: Option<servo_base::id::PipelineId>,
    body: Option<crate::request::RequestBody>,
) -> crate::request::RequestBuilder {
    use crate::request::{
        CredentialsMode, Destination, Referrer, RequestBuilder, RequestMode, ServiceWorkersMode,
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/reports+json"),
    );
    RequestBuilder::new(None, url, Referrer::NoReferrer)
        .method(http::Method::POST)
        .headers(headers)
        .body(body)
        .origin(origin)
        .destination(Destination::Report)
        .mode(RequestMode::CorsMode)
        .credentials_mode(CredentialsMode::CredentialsSameOrigin)
        .service_workers_mode(ServiceWorkersMode::None)
        .unsafe_request(true)
        .pipeline_id(pipeline)
        .reporting_cookie_client(source)
}

/// Owned DOM string data for reports. Queued tasks can carry this without a JS
/// root, and both observer snapshots and JSON preserve all UTF-16 code units.
#[derive(Clone, Debug, Eq, MallocSizeOf, PartialEq)]
pub struct ReportText(Vec<u16>);

impl ReportText {
    pub fn new(code_units: Vec<u16>) -> Self {
        Self(code_units)
    }

    pub fn code_units(&self) -> &[u16] {
        &self.0
    }
}

impl Serialize for ReportText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Ok(text) = String::from_utf16(&self.0) {
            return serializer.serialize_str(&text);
        }
        // JSON permits escaped unpaired surrogates. Rust String cannot hold
        // them, so serialize a validated JSON string made solely of hex escapes.
        let mut json = String::from("\"");
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for unit in &self.0 {
            json.push_str("\\u");
            for shift in [12, 8, 4, 0] {
                json.push(HEX[((unit >> shift) & 15) as usize] as char);
            }
        }
        json.push('"');
        serde_json::value::RawValue::from_string(json)
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ReportingEndpoint {
    pub name: String,
    pub url: ServoUrl,
}

/// <https://w3c.github.io/reporting/#process-header>
///
/// Parse the complete structured dictionary before interpreting any member.
/// A malformed field invalidates the dictionary; an unusable member only skips
/// that endpoint. Parameters are allowed, but are not part of the URL value.
pub fn parse_reporting_endpoints(
    response_url: &ServoUrl,
    headers: &HeaderMap,
) -> Option<Vec<ReportingEndpoint>> {
    let bytes = get_value_from_header_list("reporting-endpoints", headers)?;
    let dictionary = Parser::new(&bytes).parse::<Dictionary>().ok()?;
    let mut endpoints = Vec::new();
    for (name, entry) in dictionary {
        let ListEntry::Item(item) = entry else {
            continue;
        };
        let BareItem::String(value) = item.bare_item else {
            continue;
        };
        let Ok(url) = ServoUrl::parse_with_base(Some(response_url), value.as_str()) else {
            continue;
        };
        if url.origin().is_potentially_trustworthy() {
            endpoints.push(ReportingEndpoint {
                name: name.as_str().to_owned(),
                url,
            });
        }
    }
    Some(endpoints)
}
