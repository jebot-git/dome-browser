/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Arc;

use http::{HeaderMap, HeaderValue};
use net_traits::cross_origin_resource_policy::{
    CrossOriginResourcePolicy::{Allowed, Blocked},
    ForNavigation, cross_origin_resource_policy_check,
};
use net_traits::policy_container::{EmbedderPolicyValue, PolicyContainer};
use net_traits::request::{Origin, RequestClient};
use servo_default_resources as _;
use servo_url::ServoUrl;

fn check(
    origin: &str,
    response: &str,
    policy: Option<&str>,
    require_corp: bool,
) -> net_traits::cross_origin_resource_policy::CrossOriginResourcePolicy {
    let origin = Origin::Origin(ServoUrl::parse(origin).unwrap().origin());
    let mut container = PolicyContainer::default();
    if require_corp {
        container.embedder_policy.value = EmbedderPolicyValue::RequireCorp;
    }
    let client = RequestClient {
        preloaded_resources: Default::default(),
        cookie_site: None,
        cookie_client_is_null: false,
        policy_container: Arc::new(container),
        origin: origin.clone(),
        is_nested_browsing_context: false,
        insecure_requests_policy: net_traits::request::InsecureRequestsPolicy::DoNotUpgrade,
        has_trustworthy_ancestor_origin: true,
    };
    let mut headers = HeaderMap::new();
    if let Some(policy) = policy {
        headers.insert(
            "cross-origin-resource-policy",
            HeaderValue::from_str(policy).unwrap(),
        );
    }
    cross_origin_resource_policy_check(
        &origin,
        &client,
        Some(&ServoUrl::parse(response).unwrap()),
        &headers,
        true,
        ForNavigation::No,
    )
}

#[test]
fn policy_changes_require_rechecking_stored_opaque_responses() {
    assert_eq!(
        check("https://a.test", "https://b.test", None, false),
        Allowed
    );
    assert_eq!(
        check("https://a.test", "https://b.test", None, true),
        Blocked
    );
    assert_eq!(
        check("https://a.test", "https://a.test", None, true),
        Allowed
    );
    assert_eq!(
        check(
            "https://a.test",
            "https://b.test",
            Some("cross-origin"),
            true
        ),
        Allowed
    );
}

#[test]
fn same_origin_is_strict_and_case_sensitive() {
    assert_eq!(
        check(
            "https://a.test",
            "https://a.test",
            Some("same-origin"),
            false
        ),
        Allowed
    );
    assert_eq!(
        check(
            "https://a.test",
            "https://a.test:8443",
            Some("same-origin"),
            false
        ),
        Blocked
    );
    assert_eq!(
        check(
            "https://a.test",
            "https://b.test",
            Some("Same-Origin"),
            false
        ),
        Allowed
    );
    assert_eq!(
        check(
            "https://a.test",
            "https://b.test",
            Some("Same-Origin"),
            true
        ),
        Blocked
    );
}

#[test]
fn same_site_uses_public_suffix_and_secure_scheme_rules() {
    assert_eq!(
        check(
            "https://a.example.com",
            "https://b.example.com",
            Some("same-site"),
            false
        ),
        Allowed
    );
    assert_eq!(
        check(
            "http://a.example.com",
            "https://b.example.com",
            Some("same-site"),
            false
        ),
        Blocked
    );
    assert_eq!(
        check(
            "https://a.example.com",
            "http://b.example.com",
            Some("same-site"),
            false
        ),
        Allowed
    );
    assert_eq!(
        check(
            "https://one.github.io",
            "https://two.github.io",
            Some("same-site"),
            false
        ),
        Blocked
    );
}

#[test]
fn duplicate_policy_headers_are_combined_before_validation() {
    let origin = Origin::Origin(ServoUrl::parse("https://a.test").unwrap().origin());
    let client = RequestClient {
        preloaded_resources: Default::default(),
        cookie_site: None,
        cookie_client_is_null: false,
        policy_container: Arc::new(PolicyContainer::default()),
        origin: origin.clone(),
        is_nested_browsing_context: false,
        insecure_requests_policy: net_traits::request::InsecureRequestsPolicy::DoNotUpgrade,
        has_trustworthy_ancestor_origin: true,
    };
    let mut headers = HeaderMap::new();
    headers.append(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    headers.append(
        "cross-origin-resource-policy",
        HeaderValue::from_static("cross-origin"),
    );
    // The combined value is invalid, so unsafe-none allows the response.
    assert_eq!(
        cross_origin_resource_policy_check(
            &origin,
            &client,
            Some(&ServoUrl::parse("https://b.test").unwrap()),
            &headers,
            true,
            ForNavigation::No
        ),
        Allowed
    );
}

#[test]
fn credentialless_distinguishes_credentialed_cached_responses_and_navigation() {
    let origin = Origin::Origin(ServoUrl::parse("https://a.test").unwrap().origin());
    let mut policy = PolicyContainer::default();
    policy.embedder_policy.value = EmbedderPolicyValue::Credentialless;
    let client = RequestClient {
        preloaded_resources: Default::default(),
        cookie_site: None,
        cookie_client_is_null: false,
        policy_container: Arc::new(policy),
        origin: origin.clone(),
        is_nested_browsing_context: false,
        insecure_requests_policy: net_traits::request::InsecureRequestsPolicy::DoNotUpgrade,
        has_trustworthy_ancestor_origin: true,
    };
    let url = ServoUrl::parse("https://b.test").unwrap();
    let mut headers = HeaderMap::new();
    assert_eq!(
        cross_origin_resource_policy_check(
            &origin,
            &client,
            Some(&url),
            &headers,
            false,
            ForNavigation::No
        ),
        Allowed
    );
    assert_eq!(
        cross_origin_resource_policy_check(
            &origin,
            &client,
            Some(&url),
            &headers,
            true,
            ForNavigation::No
        ),
        Blocked
    );
    assert_eq!(
        cross_origin_resource_policy_check(
            &origin,
            &client,
            Some(&url),
            &headers,
            false,
            ForNavigation::Yes
        ),
        Blocked
    );
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    assert_eq!(
        cross_origin_resource_policy_check(
            &origin,
            &client,
            Some(&url),
            &headers,
            false,
            ForNavigation::No
        ),
        Blocked
    );
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("cross-origin"),
    );
    assert_eq!(
        cross_origin_resource_policy_check(
            &origin,
            &client,
            Some(&url),
            &headers,
            true,
            ForNavigation::No
        ),
        Allowed
    );
}

#[test]
fn credentialless_request_credentials_respect_mode_origin_and_redirect_taint() {
    use net_traits::blob_url_store::UrlWithBlobClaim;
    use net_traits::request::{InsecureRequestsPolicy, Referrer, RequestBuilder, RequestMode};
    let owner = ServoUrl::parse("https://owner.test").unwrap();
    let foreign = ServoUrl::parse("https://foreign.test").unwrap();
    let mut policy = PolicyContainer::default();
    policy.embedder_policy.value = EmbedderPolicyValue::Credentialless;
    let client = RequestClient {
        preloaded_resources: Default::default(),
        cookie_site: None,
        cookie_client_is_null: false,
        policy_container: Arc::new(policy),
        origin: Origin::Origin(owner.origin()),
        is_nested_browsing_context: false,
        insecure_requests_policy: InsecureRequestsPolicy::DoNotUpgrade,
        has_trustworthy_ancestor_origin: true,
    };
    let make = |url: ServoUrl| {
        let mut request =
            RequestBuilder::new(None, UrlWithBlobClaim::new(url, None), Referrer::NoReferrer)
                .origin(owner.origin())
                .build();
        request.mode = RequestMode::NoCors;
        request.client = Some(client.clone());
        request
    };
    let mut cross = make(foreign.clone());
    assert!(!cross.coep_allows_credentials());
    cross.mode = RequestMode::CorsMode;
    assert!(cross.coep_allows_credentials());
    let mut same = make(owner.clone());
    assert!(same.coep_allows_credentials());
    same.url_list.push(UrlWithBlobClaim::new(foreign, None));
    same.url_list.push(UrlWithBlobClaim::new(owner, None));
    assert!(!same.coep_allows_credentials());
}
