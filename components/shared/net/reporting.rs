/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Response-local Reporting API endpoint configuration.

use http::HeaderMap;
use servo_url::ServoUrl;
use sfv::{BareItem, Dictionary, ListEntry, Parser};

use crate::fetch::headers::get_value_from_header_list;

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
