/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Arc;

use net::connector::CACertificates;
use net::fetch::methods::{should_request_be_blocked_as_mixed_content, should_response_be_blocked_as_mixed_content};
use net::protocols::ProtocolRegistry;
use net::resource_thread::new_core_resource_thread;
use net_traits::blob_url_store::{BlobBuf, BlobResolver, UrlWithBlobClaim, parse_blob_url};
use net_traits::filemanager_thread::FileManagerThreadMsg;
use net_traits::request::{Destination, InsecureRequestsPolicy, Origin, Referrer, Request, RequestBuilder, RequestClient, RequestMode};
use net_traits::response::Response;
use net_traits::{CoreResourceMsg, ResourceFetchTiming, ResourceThreads, ResourceTimingType};
use profile_traits::mem::ProfilerChan as MemProfilerChan;
use profile_traits::time::ProfilerChan;
use servo_base::generic_channel;
use servo_url::{ImmutableOrigin, ServoUrl};
use uuid::Uuid;

/// Acquire claims from the real blob store, keeping its profiling channels alive.
struct Store {
    threads: ResourceThreads,
    _receivers: Vec<Box<dyn std::any::Any>>,
}

impl Store {
    fn new() -> Self {
        let (tx, rx) = generic_channel::channel().unwrap();
        let (mtx, mrx) = generic_channel::channel().unwrap();
        let (core, _private) = new_core_resource_thread(
            None, ProfilerChan(Some(tx)), MemProfilerChan(mtx),
            crate::create_generic_embedder_proxy(), None, CACertificates::Default, false,
            Arc::new(ProtocolRegistry::with_internal_protocols()),
        );
        Self { threads: ResourceThreads::new(core), _receivers: vec![Box::new(rx), Box::new(mrx)] }
    }

    fn claim(&self, origin: &ImmutableOrigin) -> UrlWithBlobClaim {
        let id = Uuid::new_v4();
        self.threads.core_thread.send(CoreResourceMsg::ToFileManager(
            FileManagerThreadMsg::PromoteMemory(id, BlobBuf {
                filename: None, type_string: "text/javascript".into(),
                size: 1, bytes: vec![b';'],
            }, true, origin.clone()),
        )).unwrap();
        let url = ServoUrl::parse(&format!("blob:{}/{}", origin.ascii_serialization(), id)).unwrap();
        let token = BlobResolver { origin: origin.clone(), resource_threads: &self.threads }
            .acquire_blob_token_for(&url).expect("active real store entry must be claimable");
        UrlWithBlobClaim::new(url, Some(token))
    }

    fn revoke(&self, url: &ServoUrl, origin: &ImmutableOrigin) {
        let (tx, rx) = generic_channel::channel().unwrap();
        self.threads.core_thread.send(CoreResourceMsg::ToFileManager(
            FileManagerThreadMsg::RevokeBlobURL(parse_blob_url(url).unwrap(), origin.clone(), tx),
        )).unwrap();
        rx.recv().unwrap().unwrap();
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let (tx, rx) = generic_channel::oneshot().unwrap();
        if self.threads.core_thread.send(CoreResourceMsg::Exit(tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

fn local_origin() -> ImmutableOrigin {
    ServoUrl::parse("file:///synthetic-blob-policy-fixture.html").unwrap().origin()
}

fn request(url: UrlWithBlobClaim, origin: ImmutableOrigin) -> Request {
    RequestBuilder::new(None, url, Referrer::NoReferrer)
        .origin(origin.clone()).mode(RequestMode::SameOrigin).destination(Destination::Worker)
        .policy_container(Default::default())
        .client(RequestClient {
            preloaded_resources: Default::default(), policy_container: Default::default(),
            origin: Origin::Origin(origin), cookie_site: None, cookie_client_is_null: false,
            is_nested_browsing_context: false,
            insecure_requests_policy: InsecureRequestsPolicy::DoNotUpgrade,
            has_trustworthy_ancestor_origin: true,
        }).build()
}

fn response(url: ServoUrl) -> Response {
    Response::new(url, ResourceFetchTiming::new(ResourceTimingType::Resource))
}

#[test]
fn local_blob_claim_survives_request_and_matching_response_checks() {
    let store = Store::new();
    let origin = local_origin();
    let url = store.claim(&origin);
    assert!(!url.url().is_potentially_trustworthy(), "serialized null loses file-origin identity");
    let response = response(url.url());
    let request = request(url, origin);
    let protocols = ProtocolRegistry::with_internal_protocols();
    assert!(!should_request_be_blocked_as_mixed_content(&request, &protocols));
    assert!(!should_response_be_blocked_as_mixed_content(&request, &response, &protocols));
}

#[test]
fn a_response_cannot_borrow_another_blob_urls_file_origin() {
    let store = Store::new();
    let origin = local_origin();
    let request = request(store.claim(&origin), origin);
    let protocols = ProtocolRegistry::with_internal_protocols();
    for url in [format!("blob:null/{}", Uuid::new_v4()), "http://insecure.example/response".into()] {
        assert!(should_response_be_blocked_as_mixed_content(
            &request, &response(ServoUrl::parse(&url).unwrap()), &protocols,
        ));
    }
}

#[test]
fn unrelated_opaque_claims_are_not_trusted_by_a_secure_ancestor() {
    let store = Store::new();
    let origin = ImmutableOrigin::new_opaque();
    let url = store.claim(&origin);
    let response = response(url.url());
    let request = request(url, origin);
    let protocols = ProtocolRegistry::with_internal_protocols();
    assert!(should_request_be_blocked_as_mixed_content(&request, &protocols));
    assert!(should_response_be_blocked_as_mixed_content(&request, &response, &protocols));
}

#[test]
fn unclaimed_null_blob_cannot_inherit_its_initiators_file_trust() {
    let url = ServoUrl::parse(&format!("blob:null/{}", Uuid::new_v4())).unwrap();
    let response = response(url.clone());
    let request = request(UrlWithBlobClaim::new(url, None), local_origin());
    let protocols = ProtocolRegistry::with_internal_protocols();
    assert!(should_request_be_blocked_as_mixed_content(&request, &protocols));
    assert!(should_response_be_blocked_as_mixed_content(&request, &response, &protocols));
}

#[test]
fn revoked_entry_rejects_new_claims_without_invalidating_existing_claim_trust() {
    let store = Store::new();
    let origin = local_origin();
    let claimed = store.claim(&origin);
    let url = claimed.url();
    store.revoke(&url, &origin);
    assert!(BlobResolver { origin: origin.clone(), resource_threads: &store.threads }
        .acquire_blob_token_for(&url).is_none());
    let request = request(claimed, origin);
    let protocols = ProtocolRegistry::with_internal_protocols();
    assert!(!should_request_be_blocked_as_mixed_content(&request, &protocols));
    assert!(!should_response_be_blocked_as_mixed_content(&request, &response(url), &protocols));
}

#[test]
fn redirects_do_not_reuse_an_earlier_blob_claim() {
    let store = Store::new();
    let origin = local_origin();
    let claimed = store.claim(&origin);
    let earlier = claimed.url();
    let mut request = request(claimed, origin);
    request.url_list.push(UrlWithBlobClaim::new(
        ServoUrl::parse(&format!("blob:null/{}", Uuid::new_v4())).unwrap(), None,
    ));
    let protocols = ProtocolRegistry::with_internal_protocols();
    assert!(should_request_be_blocked_as_mixed_content(&request, &protocols));
    assert!(should_response_be_blocked_as_mixed_content(&request, &response(earlier), &protocols));
}
