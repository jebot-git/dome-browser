/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Arc;

use net_traits::blob_url_store::UrlWithBlobClaim;
use net_traits::policy_container::PolicyContainer;
use net_traits::request::{Destination, InsecureRequestsPolicy, Origin, Referrer, Request, RequestBuilder, RequestClient, RequestMode};
use net_traits::response::ResponseBody;
use net_traits::NetworkError;
use servo_base::id::TEST_WEBVIEW_ID;
use servo_url::{ImmutableOrigin, ServoUrl};

const CONTENT: &[u8] = b"synthetic local fixture; never a user file";

fn fixture() -> (tempfile::TempDir, ServoUrl) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private-fixture.txt");
    std::fs::write(&path, CONTENT).unwrap();
    let url = ServoUrl::from_file_path(path).unwrap();
    (directory, url)
}

fn request(url: &ServoUrl, origin: ImmutableOrigin, policy: Arc<PolicyContainer>) -> Request {
    RequestBuilder::new(Some(TEST_WEBVIEW_ID), UrlWithBlobClaim::new(url.clone(), None), Referrer::NoReferrer)
        .origin(origin.clone())
        .mode(RequestMode::NoCors)
        .destination(Destination::Image)
        .policy_container(policy.clone())
        .client(RequestClient {
            preloaded_resources: Default::default(),
            policy_container: policy,
            origin: Origin::Origin(origin),
            cookie_site: None,
            cookie_client_is_null: false,
            is_nested_browsing_context: false,
            insecure_requests_policy: InsecureRequestsPolicy::DoNotUpgrade,
            has_trustworthy_ancestor_origin: false,
        })
        .build()
}

fn assert_denied(request: Request) {
    let response = crate::fetch(request, None);
    assert_eq!(response.get_network_error(), Some(&NetworkError::ResourceLoadError("Local file access denied".into())));
}

fn assert_contents(request: Request) {
    let response = crate::fetch(request, None);
    assert!(!response.is_network_error(), "{:?}", response.get_network_error());
    let body = response.actual_response().body.lock();
    assert!(matches!(&*body, ResponseBody::Done(bytes) if bytes.as_slice() == CONTENT));
}

#[test]
fn network_subresources_cannot_read_local_files() {
    let (_directory, url) = fixture();
    for destination in [Destination::Image, Destination::Style, Destination::Script, Destination::Worker, Destination::None] {
        let mut request = request(&url, ServoUrl::parse("https://remote.example/").unwrap().origin(), Default::default());
        request.destination = destination;
        assert_denied(request);
    }
}

#[test]
fn opaque_clients_have_no_implicit_file_authority() {
    let (_directory, url) = fixture();
    assert_denied(request(&url, ImmutableOrigin::new_opaque(), Default::default()));
}

#[test]
fn a_file_referrer_does_not_grant_file_authority() {
    let (_directory, url) = fixture();
    let mut request = request(&url, ServoUrl::parse("https://remote.example/").unwrap().origin(), Default::default());
    request.referrer = Referrer::ReferrerUrl(url.clone());
    assert_denied(request);
}

#[test]
fn directory_listing_is_protected_by_the_same_boundary() {
    let (directory, _url) = fixture();
    let url = ServoUrl::from_file_path(directory.path()).unwrap();
    assert_denied(request(&url, ImmutableOrigin::new_opaque(), Default::default()));
}

#[test]
fn native_browser_navigation_can_open_a_local_file() {
    let (_directory, url) = fixture();
    let mut request = request(&url, ImmutableOrigin::new_opaque(), Default::default());
    request.mode = RequestMode::Navigate;
    request.destination = Destination::Document;
    request.client.as_mut().unwrap().cookie_client_is_null = true;
    assert_contents(request);
}

#[test]
fn nested_navigation_cannot_claim_browser_authority() {
    let (_directory, url) = fixture();
    let mut request = request(&url, ImmutableOrigin::new_opaque(), Default::default());
    request.mode = RequestMode::Navigate;
    request.destination = Destination::Document;
    let client = request.client.as_mut().unwrap();
    client.cookie_client_is_null = true;
    client.is_nested_browsing_context = true;
    assert_denied(request);
}

#[test]
fn script_navigation_is_not_a_native_file_selection() {
    let (_directory, url) = fixture();
    let mut request = request(&url, ServoUrl::parse("https://remote.example/").unwrap().origin(), Default::default());
    request.mode = RequestMode::Navigate;
    request.destination = Destination::Document;
    assert_denied(request);
}

#[test]
fn explicitly_granted_local_context_and_policy_clones_keep_file_access() {
    let (_directory, url) = fixture();
    let origin = ImmutableOrigin::new_opaque();
    let mut policy = PolicyContainer::default();
    policy.local_file_origin = Some(origin.clone());
    let inherited = Arc::new(policy.clone());
    assert_contents(request(&url, origin, inherited));
}

#[test]
fn a_clientless_navigation_does_not_implicitly_bypass_the_guard() {
    let (_directory, url) = fixture();
    let mut request = request(&url, ImmutableOrigin::new_opaque(), Default::default());
    request.mode = RequestMode::Navigate;
    request.destination = Destination::Document;
    request.client = None;
    assert_denied(request);
}

#[test]
fn copying_a_local_policy_does_not_authorize_an_unrelated_opaque_client() {
    let (_directory, url) = fixture();
    let mut policy = PolicyContainer::default();
    policy.local_file_origin = Some(ImmutableOrigin::new_opaque());
    assert_denied(request(&url, ImmutableOrigin::new_opaque(), Arc::new(policy)));
}

#[test]
fn copying_a_local_policy_does_not_authorize_a_network_client() {
    let (_directory, url) = fixture();
    let mut policy = PolicyContainer::default();
    policy.local_file_origin = Some(ImmutableOrigin::new_opaque());
    assert_denied(request(&url, ServoUrl::parse("https://remote.example/").unwrap().origin(), Arc::new(policy)));
}
