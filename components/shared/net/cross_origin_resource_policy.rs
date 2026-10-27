/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Shared Fetch CORP checks for network and Cache API response delivery.

use http::HeaderMap;
use servo_url::{ImmutableOrigin, ServoUrl};

use crate::fetch::headers::get_value_from_header_list;
use crate::policy_container::EmbedderPolicyValue;
use crate::pub_domains::reg_suffix;
use crate::request::{Origin, RequestClient};

/// <https://html.spec.whatwg.org/multipage/#schemelessly-same-site>
fn is_schemelessy_same_site(site_a: &ImmutableOrigin, site_b: &ImmutableOrigin) -> bool {
    // Step 1
    if !site_a.is_tuple() && !site_b.is_tuple() && site_a == site_b {
        true
    } else if site_a.is_tuple() && site_b.is_tuple() {
        // Step 2.1
        let host_a = site_a.host().map(|h| h.to_string()).unwrap_or_default();
        let host_b = site_b.host().map(|h| h.to_string()).unwrap_or_default();

        let host_a_reg = reg_suffix(&host_a);
        let host_b_reg = reg_suffix(&host_b);

        // Step 2.2-2.3
        (site_a.host() == site_b.host() && host_a_reg.is_empty())
            || (host_a_reg == host_b_reg && !host_a_reg.is_empty())
    } else {
        // Step 3
        false
    }
}

/// <https://fetch.spec.whatwg.org/#cross-origin-resource-policy-check>
///
/// This is obtained from [cross_origin_resource_policy_check]
#[derive(Debug, PartialEq)]
pub enum CrossOriginResourcePolicy {
    Allowed,
    Blocked,
}

pub enum ForNavigation {
    Yes,
    No,
}

/// <https://fetch.spec.whatwg.org/#cross-origin-resource-policy-check>
pub fn cross_origin_resource_policy_check(
    origin: &Origin,
    request_client: &RequestClient,
    response_url: Option<&ServoUrl>,
    response_headers: &HeaderMap,
    request_includes_credentials: bool,
    for_navigation: ForNavigation,
) -> CrossOriginResourcePolicy {
    // Step 1. Set forNavigation to false if it is not given.
    //
    // That's the default value of the enum

    // Step 2. Let embedderPolicy be settingsObject’s policy container’s embedder policy.
    let embedder_policy = &request_client.policy_container.embedder_policy;

    // Step 3. If the cross-origin resource policy internal check with origin, "unsafe-none",
    // response, and forNavigation returns blocked, then return blocked.
    if cross_origin_resource_policy_internal_check(
        origin,
        EmbedderPolicyValue::UnsafeNone,
        response_url,
        response_headers,
        request_includes_credentials,
        &for_navigation,
    ) == CrossOriginResourcePolicy::Blocked
    {
        return CrossOriginResourcePolicy::Blocked;
    }

    // TODO Step 4. If the cross-origin resource policy internal check with origin,
    // embedderPolicy’s report only value, response, and forNavigation returns blocked, then queue
    // a cross-origin embedder policy CORP violation report with response, settingsObject,
    // destination, and true.

    // Step 5. If the cross-origin resource policy internal check with origin, embedderPolicy’s
    // value, response, and forNavigation returns allowed, then return allowed.
    if cross_origin_resource_policy_internal_check(
        origin,
        embedder_policy.value,
        response_url,
        response_headers,
        request_includes_credentials,
        &for_navigation,
    ) == CrossOriginResourcePolicy::Allowed
    {
        return CrossOriginResourcePolicy::Allowed;
    }

    // TODO Step 6. Queue a cross-origin embedder policy CORP violation report with response,
    // settingsObject, destination, and false.

    // Step 7. Return blocked.
    CrossOriginResourcePolicy::Blocked
}

/// <https://fetch.spec.whatwg.org/#cross-origin-resource-policy-internal-check>
fn cross_origin_resource_policy_internal_check(
    origin: &Origin,
    embedder_policy_value: EmbedderPolicyValue,
    response_url: Option<&ServoUrl>,
    response_headers: &HeaderMap,
    request_includes_credentials: bool,
    for_navigation: &ForNavigation,
) -> CrossOriginResourcePolicy {
    // Step 1. If forNavigation is true and embedderPolicyValue is "unsafe-none", then return allowed.
    if let ForNavigation::Yes = for_navigation
        && let EmbedderPolicyValue::UnsafeNone = embedder_policy_value
    {
        return CrossOriginResourcePolicy::Allowed;
    }

    // Step 2. Let policy be the result of getting `Cross-Origin-Resource-Policy` from response’s header list.
    let policy = get_value_from_header_list("cross-origin-resource-policy", response_headers);
    let policy = policy.as_deref().and_then(|h| std::str::from_utf8(h).ok());

    // Step 3. If policy is neither `same-origin`, `same-site`, nor `cross-origin`, then set policy to null.
    let policy = policy
        .filter(|&s| s == "same-origin" || s == "same-site" || s == "cross-origin")
        // Step 4. If policy is null, then switch on embedderPolicyValue:
        .or(match embedder_policy_value {
            // Do nothing.
            EmbedderPolicyValue::UnsafeNone => None,
            // Set policy to `same-origin`.
            EmbedderPolicyValue::RequireCorp => Some("same-origin"),
            EmbedderPolicyValue::Credentialless
                if request_includes_credentials || matches!(for_navigation, ForNavigation::Yes) =>
            {
                Some("same-origin")
            },
            EmbedderPolicyValue::Credentialless => None,
        });

    // Step 5. Switch on policy:
    match policy {
        Some("same-origin") => {
            // If origin is same origin with response’s URL’s origin, then return allowed.
            if let Origin::Origin(request_origin) = origin
                && response_url.is_some_and(|url| request_origin == &url.origin())
            {
                return CrossOriginResourcePolicy::Allowed;
            }

            // Otherwise, return blocked.
            CrossOriginResourcePolicy::Blocked
        },
        Some("same-site") => {
            if let Some(response_url) = response_url {
                // If all of the following are true
                // origin is schemelessly same site with response’s URL’s origin
                // origin’s scheme is "https" or response’s URL’s scheme is not "https"
                if let Origin::Origin(request_origin) = origin
                    && is_schemelessy_same_site(request_origin, &response_url.origin())
                    && (request_origin.scheme() == Some("https")
                        || response_url.scheme() != "https")
                {
                    return CrossOriginResourcePolicy::Allowed;
                }
            }
            // Otherwise, return blocked.
            CrossOriginResourcePolicy::Blocked
        },
        // null / 'cross-origin'
        // Return allowed.
        _ => CrossOriginResourcePolicy::Allowed,
    }
}
