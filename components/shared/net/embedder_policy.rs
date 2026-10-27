/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use http::HeaderMap;
use sfv::{BareItem, Item, Parser};

use crate::fetch::headers::get_value_from_header_list;
use crate::policy_container::{EmbedderPolicy, EmbedderPolicyValue};

/// Parse one structured field item. Lists, strings and malformed parameters
/// do not opt into isolation, even if their bytes contain a recognized token.
fn parse_header(headers: &HeaderMap, name: &str) -> (EmbedderPolicyValue, Option<String>) {
    let Some(bytes) = get_value_from_header_list(name, headers) else {
        return (EmbedderPolicyValue::UnsafeNone, None);
    };
    let Ok(item) = Parser::new(&bytes).parse::<Item>() else {
        return (EmbedderPolicyValue::UnsafeNone, None);
    };
    let BareItem::Token(token) = &item.bare_item else {
        return (EmbedderPolicyValue::UnsafeNone, None);
    };
    let value = match token.as_str() {
        "require-corp" => EmbedderPolicyValue::RequireCorp,
        "credentialless" => EmbedderPolicyValue::Credentialless,
        _ => return (EmbedderPolicyValue::UnsafeNone, None),
    };
    let endpoint = match item.params.get("report-to") {
        Some(BareItem::String(endpoint)) => Some(endpoint.as_str().to_owned()),
        _ => None,
    };
    (value, endpoint)
}

impl EmbedderPolicy {
    /// <https://html.spec.whatwg.org/multipage/browsers.html#obtain-an-embedder-policy>
    pub fn from_headers(headers: Option<&HeaderMap>, is_secure_context: bool) -> Self {
        let Some(headers) = headers.filter(|_| is_secure_context) else {
            return Self::default();
        };
        let (value, reporting_endpoint) = parse_header(headers, "cross-origin-embedder-policy");
        let (report_only_value, report_only_reporting_endpoint) =
            parse_header(headers, "cross-origin-embedder-policy-report-only");
        Self {
            value,
            reporting_endpoint,
            report_only_value,
            report_only_reporting_endpoint,
        }
    }
}
