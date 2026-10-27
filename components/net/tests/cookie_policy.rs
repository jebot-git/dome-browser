/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use net_traits::blob_url_store::UrlWithBlobClaim;
use net_traits::request::{Destination, InsecureRequestsPolicy, Origin, Referrer, RequestBuilder, RequestClient, RequestMode};
use servo_url::ServoUrl;

fn request(target: &str, site: Option<&str>, nested: bool) -> RequestBuilder {
    RequestBuilder::new(None,
        UrlWithBlobClaim::from_url_without_having_claimed_blob(ServoUrl::parse(target).unwrap()),
        Referrer::NoReferrer)
        .client(RequestClient {
            preloaded_resources: Default::default(),
            policy_container: Default::default(),
            origin: Origin::Origin(ServoUrl::parse(target).unwrap().origin()),
            cookie_site: site.map(|site| ServoUrl::parse(site).unwrap().origin()),
            cookie_client_is_null: false,
            is_nested_browsing_context: nested,
            insecure_requests_policy: InsecureRequestsPolicy::DoNotUpgrade,
            has_trustworthy_ancestor_origin: false,
        })
}

#[test]
fn cookie_policy_uses_schemeful_psl_site_and_not_ports_or_suffix_strings() {
    for (target, site, allowed) in [
        ("https://cdn.example.co.uk/a", "https://www.example.co.uk/",true),
        ("https://example.org:8443/", "https://example.org:9443/",true),
        ("http://example.org/", "https://example.org/",false),
        ("https://other.co.uk/", "https://example.co.uk/",false),
        ("https://a.github.io/", "https://b.github.io/",false),
        ("https://cdn.a.github.io/", "https://a.github.io/",true),
        ("https://example.org.evil.test/", "https://example.org/",false),
        ("https://127.0.0.2/", "https://127.0.0.1/",false),
        ("https://[::1]:8443/", "https://[::1]:9443/",true),
    ] {
        let builder = request(target,Some(site),false);
        assert_eq!(builder.is_first_party_for_cookies(),allowed,"{target}, {site}");
        assert_eq!(builder.build().is_first_party_for_cookies(),allowed);
    }
}

#[test]
fn cookie_policy_does_not_trust_own_origin_without_ancestor_context() {
    assert!(!request("https://example.org/",None,true).build().is_first_party_for_cookies());
    assert!(!request("https://example.org/",None,false).build().is_first_party_for_cookies());
    assert!(!RequestBuilder::new(None,
        UrlWithBlobClaim::from_url_without_having_claimed_blob(ServoUrl::parse("https://example.org/").unwrap()),
        Referrer::NoReferrer).build().is_first_party_for_cookies());
}

#[test]
fn cookie_policy_top_level_navigations_establish_first_party_but_frames_do_not() {
    assert!(request("https://new.example/",None,false).mode(RequestMode::Navigate)
        .destination(Destination::Document).build().is_first_party_for_cookies());
    assert!(!request("https://new.example/",Some("https://old.example/"),true).mode(RequestMode::Navigate)
        .destination(Destination::IFrame).build().is_first_party_for_cookies());
    assert!(!request("https://new.example/",None,true).mode(RequestMode::Navigate)
        .destination(Destination::Document).build().is_first_party_for_cookies());
}

#[test]
fn cookie_policy_rechecks_each_redirect_target() {
    let mut request = request("https://example.org/",Some("https://example.org/"),false).build();
    assert!(request.is_first_party_for_cookies());
    request.url_list.push(UrlWithBlobClaim::from_url_without_having_claimed_blob(ServoUrl::parse("https://elsewhere.test/").unwrap()));
    assert!(!request.is_first_party_for_cookies());
}

#[test]
fn cookie_policy_websocket_transport_restores_host_without_duplicate_headers() {
    let context = crate::new_fetch_context(None, None);
    let mut builder = request("https://third.example:8443/socket",Some("https://first.example/"),false)
        .mode(RequestMode::WebSocket {
            protocols: vec![],
            original_url: ServoUrl::parse("wss://third.example:8443/socket").unwrap(),
        });
    builder.client.as_mut().unwrap().origin = Origin::Origin(ServoUrl::parse("https://first.example/").unwrap().origin());
    let mut request = net::test::create_handshake_request(builder, context.state.clone()).unwrap();
    assert_eq!(request.headers["Origin"],"https://first.example");
    assert_eq!(request.headers["Host"],"third.example:8443");
    let key = request.headers["Sec-WebSocket-Key"].clone();
    // Match the ordinary HTTP Fetch path before handing off to WebSocket transport.
    request.headers.remove("Host");
    let handshake = net::test::websocket_transport_handshake(&request).unwrap();
    assert_eq!(handshake.headers()["Host"],"third.example:8443");
    assert_eq!(handshake.headers()["Sec-WebSocket-Key"],key);
    for name in ["Host","Connection","Upgrade","Sec-WebSocket-Key","Sec-WebSocket-Version"] {
        assert_eq!(handshake.headers().get_all(name).iter().count(),1,"{name}");
    }
}
