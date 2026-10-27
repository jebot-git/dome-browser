/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::{Arc, Mutex};
use http::Method;
use net::embedder::NetToEmbedderMsg;
use embedder_traits::WebResourceResponseMsg;
use net_traits::blob_url_store::UrlWithBlobClaim;
use net_traits::request::{Referrer, Request, RequestBuilder, RequestMode};
use servo_url::ServoUrl;

fn request(url: &str) -> Request {
    let url = ServoUrl::parse(url).unwrap();
    RequestBuilder::new(None, UrlWithBlobClaim::from_url_without_having_claimed_blob(url.clone()), Referrer::NoReferrer)
        .origin(url.origin()).mode(RequestMode::SameOrigin).build()
}

#[test]
fn retains_raw_order_encoding_duplicates_and_empty_fields() {
    let mut r = request("http://site.test/p?keep=a%2Bb&&track=secret&keep=two&#route");
    let target = ServoUrl::parse("http://site.test/p?keep=a%2Bb&&keep=two&#route").unwrap();
    assert!(r.remove_query_parameters(&target));
    assert_eq!(r.current_url(), target);
}

#[test]
fn removes_the_entire_query() {
    let mut r = request("https://site.test/p?track=secret#route");
    assert!(r.remove_query_parameters(&ServoUrl::parse("https://site.test/p#route").unwrap()));
    assert_eq!(r.current_url().query(), None);
    assert_eq!(r.current_url().fragment(), Some("route"));
}

#[test]
fn rejects_scope_or_retained_data_changes_atomically() {
    let original = "http://name:pass@site.test/p?keep=1&track=x&keep=2#route";
    for target in [
        "https://name:pass@site.test/p?keep=1#route",
        "http://name:pass@other.test/p?keep=1#route",
        "http://name:pass@site.test:8080/p?keep=1#route",
        "http://other:pass@site.test/p?keep=1#route",
        "http://name:other@site.test/p?keep=1#route",
        "http://name:pass@site.test/other?keep=1#route",
        "http://name:pass@site.test/p?keep=1",
        "http://name:pass@site.test/p?keep=3#route",
        "http://name:pass@site.test/p?keep=2&keep=1#route",
        "http://name:pass@site.test/p?keep=1&keep=1#route",
        "http://name:pass@site.test/p?keep=1&new=x#route",
    ] {
        let mut r = request(original);
        assert!(!r.remove_query_parameters(&ServoUrl::parse(target).unwrap()), "{target}");
        assert_eq!(r.current_url().as_str(), original);
        assert_eq!(r.url_list.len(), 1);
    }
    let mut r = request("http://site.test/p?encoded=a%2Bb&track=x");
    assert!(!r.remove_query_parameters(&ServoUrl::parse("http://site.test/p?encoded=a+b").unwrap()));
    assert_eq!(r.current_url().query(), Some("encoded=a%2Bb&track=x"));
}

#[test]
fn rejects_non_http_targets_and_query_addition() {
    let mut r = request("file:///tmp/query?track=x");
    assert!(!r.remove_query_parameters(&ServoUrl::parse("file:///tmp/query").unwrap()));
    let mut r = request("http://site.test/p#route?track=fragment");
    assert!(!r.remove_query_parameters(&ServoUrl::parse("http://site.test/p#route").unwrap()));
    assert!(!r.remove_query_parameters(&ServoUrl::parse("http://site.test/p?new=x#route?track=fragment").unwrap()));
}

#[test]
fn preserves_redirect_history_method_and_headers() {
    let mut r = request("https://site.test/start");
    r.url_list.push(UrlWithBlobClaim::from_url_without_having_claimed_blob(ServoUrl::parse("https://site.test/p?track=x&keep=1").unwrap()));
    r.redirect_count = 1;
    r.method = Method::POST;
    r.headers.insert("x-probe", "unchanged".parse().unwrap());
    assert!(r.remove_query_parameters(&ServoUrl::parse("https://site.test/p?keep=1").unwrap()));
    assert_eq!(r.url_list.len(), 2);
    assert_eq!(r.url().as_str(), "https://site.test/start");
    assert_eq!(r.redirect_count, 1);
    assert_eq!(r.method, Method::POST);
    assert_eq!(r.headers["x-probe"], "unchanged");
}

#[test]
fn synchronizes_websocket_transport_queries_and_retains_hsts_upgrade() {
    for (scheme, transport) in [("http", "ws"), ("https", "wss")] {
        let mut r = request(&format!("{scheme}://site.test/p?track=x&keep=1"));
        r.mode = RequestMode::WebSocket { protocols: vec!["chat".into()], original_url: ServoUrl::parse("ws://site.test/p?track=x&keep=1").unwrap() };
        assert!(r.remove_query_parameters(&ServoUrl::parse(&format!("{scheme}://site.test/p?keep=1")).unwrap()));
        let handshake = net::test::websocket_transport_handshake(&r).unwrap();
        assert_eq!(handshake.uri().scheme_str(), Some(transport));
        assert_eq!(handshake.uri().query(), Some("keep=1"));
        assert!(matches!(&r.mode, RequestMode::WebSocket { protocols, .. } if protocols == &["chat"]));
    }
}

#[test]
fn interceptor_sends_only_cleaned_urls_and_cancels_invalid_edits() {
    for invalid in [false, true] {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let captured = observed.clone();
        let (server, url) = net::test_util::make_server(move |r, _| captured.lock().unwrap().push(r.uri().to_string()));
        let mut url = url.url(); url.as_mut_url().set_path("/privacy");url.as_mut_url().set_query(Some("track=secret&keep=1"));
        let (proxy, receiver) = crate::create_generic_embedder_proxy_and_receiver::<NetToEmbedderMsg>();
        let handler = std::thread::spawn(move || {
            while let Ok(message) = receiver.recv_timeout(std::time::Duration::from_secs(10)) {
                if let NetToEmbedderMsg::WebResourceRequested(_, mut r, sender) = message {
                    r.url.as_mut_url().set_query(Some("keep=1"));
                    if invalid { r.url.as_mut_url().set_path("/other"); }
                    sender.send(WebResourceResponseMsg::RemoveQueryParameters(r.url)).unwrap();
                    return;
                }
            }
            panic!("No interception message received");
        });
        let mut context = crate::new_fetch_context(None, Some(proxy));
        let response = crate::fetch_with_context(request(url.as_str()), &mut context);
        handler.join().unwrap();
        if invalid {
            assert!(response.is_network_error());
            assert!(observed.lock().unwrap().is_empty());
        } else {
            assert!(!response.is_network_error());
            assert_eq!(&*observed.lock().unwrap(), &["/privacy?keep=1"]);
        }
        server.close();
    }
}

#[test]
fn reuses_only_effectively_matching_preloads_including_opaque_responses() {
    use net_traits::request::{Destination, InsecureRequestsPolicy, Origin, PreloadEntry, PreloadId, PreloadKey, RequestClient};
    use net_traits::response::{Response, ResponseBody, ResponseType};
    use net_traits::{ResourceFetchTiming, ResourceTimingType};
    for sanitized in [false, true] {
        for opaque in [false, true] {
            let observed = Arc::new(Mutex::new(Vec::new()));
            let captured = observed.clone();
            let (server, url) = net::test_util::make_server(move |r, _| captured.lock().unwrap().push(r.uri().to_string()));
            let mut url = url.url();url.as_mut_url().set_path("/privacy");url.as_mut_url().set_query(Some("track=secret&keep=1"));
            let mut cleaned = url.clone();cleaned.as_mut_url().set_query(Some("keep=1"));
            let mut r = request(url.as_str());r.mode = RequestMode::NoCors;r.destination = Destination::Script;
            r.origin = Origin::Origin(crate::mock_origin());
            let id = PreloadId::default();
            let key = PreloadKey { url: url.clone(), destination: r.destination, mode: r.mode.clone(), credentials_mode: r.credentials_mode };
            r.client = Some(RequestClient {
                preloaded_resources: [(key, id.clone())].into_iter().collect(),
                policy_container: Default::default(), origin: r.origin.clone(),
                cookie_site: Some(crate::mock_origin()), cookie_client_is_null: false,
                is_nested_browsing_context: false, insecure_requests_policy: InsecureRequestsPolicy::DoNotUpgrade,
                has_trustworthy_ancestor_origin: false,
            });
            let effective = if sanitized { cleaned } else { url };
            let mut response = Response::new(effective.clone(), ResourceFetchTiming::new(ResourceTimingType::Resource));
            response.url_list.push(effective);
            *response.body.lock() = ResponseBody::Done(b"preload marker".to_vec());
            let response = if opaque { response.to_filtered(ResponseType::Opaque) } else { response };
            let mut entry = PreloadEntry::new(String::new());entry.with_response(response);
            let (proxy, receiver) = crate::create_generic_embedder_proxy_and_receiver::<NetToEmbedderMsg>();
            let handler = std::thread::spawn(move || {
                while let Ok(message) = receiver.recv_timeout(std::time::Duration::from_secs(10)) {
                    if let NetToEmbedderMsg::WebResourceRequested(_, mut r, sender) = message {
                        r.url.as_mut_url().set_query(Some("keep=1"));
                        sender.send(WebResourceResponseMsg::RemoveQueryParameters(r.url)).unwrap();
                        return;
                    }
                }
                panic!("No interception message received");
            });
            let mut context = crate::new_fetch_context(None, Some(proxy));
            context.preloaded_resources.lock().unwrap().insert(id.clone(), entry);
            let response = crate::fetch_with_context(r, &mut context);
            handler.join().unwrap();
            assert!(!response.is_network_error());
            assert!(!context.preloaded_resources.lock().unwrap().contains_key(&id));
            if sanitized {
                assert!(observed.lock().unwrap().is_empty());
                assert!(matches!(&*response.actual_response().body.lock(), ResponseBody::Done(bytes) if bytes == b"preload marker"));
            } else {
                assert_eq!(&*observed.lock().unwrap(), &["/privacy?keep=1"]);
            }
            server.close();
        }
    }
}

#[test]
fn discarded_preload_cannot_bypass_cors_after_a_redirect() {
    use http::StatusCode;
    use net_traits::request::{InsecureRequestsPolicy, PreloadEntry, PreloadId, PreloadKey, RequestClient};
    use net_traits::response::{Response, ResponseBody};
    use net_traits::{ResourceFetchTiming, ResourceTimingType};
    let final_hits = Arc::new(Mutex::new(Vec::new()));
    let captured = final_hits.clone();
    let (final_server, final_url) = net::test_util::make_server(move |r, _| captured.lock().unwrap().push(r.uri().to_string()));
    let mut final_url = final_url.url();final_url.as_mut_url().set_path("/final");
    let first_hits = Arc::new(Mutex::new(Vec::new()));
    let captured = first_hits.clone();
    let (server, url) = net::test_util::make_server(move |r, response| {
        captured.lock().unwrap().push(r.uri().to_string());
        *response.status_mut() = StatusCode::FOUND;
        response.headers_mut().insert(http::header::LOCATION, final_url.as_str().parse().unwrap());
    });
    let mut url = url.url();url.as_mut_url().set_path("/privacy");url.as_mut_url().set_query(Some("track=secret&keep=1"));
    let mut r = request(url.as_str());r.mode = RequestMode::CorsMode;
    let id = PreloadId::default();
    let key = PreloadKey { url: url.clone(), destination: r.destination, mode: r.mode.clone(), credentials_mode: r.credentials_mode };
    r.client = Some(RequestClient {
        preloaded_resources: [(key, id.clone())].into_iter().collect(), policy_container: Default::default(), origin: r.origin.clone(),
        cookie_site: Some(url.origin()), cookie_client_is_null: false, is_nested_browsing_context: false,
        insecure_requests_policy: InsecureRequestsPolicy::DoNotUpgrade, has_trustworthy_ancestor_origin: false,
    });
    let mut stale = Response::new(url.clone(), ResourceFetchTiming::new(ResourceTimingType::Resource));
    stale.url_list.push(url);*stale.body.lock() = ResponseBody::Done(b"stale private response".to_vec());
    let mut entry = PreloadEntry::new(String::new());entry.with_response(stale);
    let (proxy, receiver) = crate::create_generic_embedder_proxy_and_receiver::<NetToEmbedderMsg>();
    let handler = std::thread::spawn(move || {
        while let Ok(message) = receiver.recv_timeout(std::time::Duration::from_secs(10)) {
            if let NetToEmbedderMsg::WebResourceRequested(_, mut r, sender) = message {
                r.url.as_mut_url().set_query(Some("keep=1"));
                sender.send(WebResourceResponseMsg::RemoveQueryParameters(r.url)).unwrap();return;
            }
        }
        panic!("No interception message received");
    });
    let mut context = crate::new_fetch_context(None, Some(proxy));
    context.preloaded_resources.lock().unwrap().insert(id, entry);
    let response = crate::fetch_with_context(r, &mut context);
    handler.join().unwrap();
    assert!(response.is_network_error(), "The cross-origin target without ACAO must fail CORS");
    assert_eq!(&*first_hits.lock().unwrap(), &["/privacy?keep=1"]);
    assert_eq!(&*final_hits.lock().unwrap(), &["/final"]);
    server.close();final_server.close();
}
