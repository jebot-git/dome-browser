/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use cookie::SameSite;
use net::cookie::ServoCookie;
use net::cookie_storage::CookieStorage;
use net_traits::{CookieContext, CookieSource};
use net_traits::blob_url_store::UrlWithBlobClaim;
use net_traits::request::{Destination, InsecureRequestsPolicy, Origin, Referrer, RequestBuilder, RequestClient, RequestMode};
use parking_lot::RwLock;
use servo_url::ServoUrl;

fn url() -> ServoUrl { ServoUrl::parse("https://example.org/").unwrap() }
fn context(same_site: bool, top_level_navigation: bool, safe_method: bool) -> CookieContext {
    CookieContext {same_site,top_level_navigation,safe_method}
}
fn cookie(value: &str) -> ServoCookie { ServoCookie::from_cookie_string(value,&url(),CookieSource::HTTP).unwrap() }
fn names(jar: &mut CookieStorage, source: CookieSource, context: CookieContext) -> Vec<String> {
    jar.cookies_data_for_url_with_context(&url(),source,context).map(|cookie|cookie.name().to_owned()).collect()
}
fn seeded() -> CookieStorage {
    let mut jar=CookieStorage::new(150);
    for value in ["strict=x; SameSite=Strict; Secure", "lax=x; SameSite=Lax; Secure",
        "none=x; SameSite=None; Secure", "default=x; Secure"] {
        jar.push(cookie(value),&url(),CookieSource::HTTP);
    }
    jar
}

#[test]
fn samesite_retrieval_enforces_strict_lax_default_and_none() {
    let mut jar=seeded();
    assert_eq!(names(&mut jar,CookieSource::HTTP,context(true,false,false)),["strict","lax","none","default"]);
    assert_eq!(names(&mut jar,CookieSource::HTTP,context(false,false,true)),["none"]);
    assert_eq!(names(&mut jar,CookieSource::HTTP,context(false,true,true)),["lax","none","default"]);
    assert_eq!(names(&mut jar,CookieSource::HTTP,context(false,true,false)),["none"]);
    assert_eq!(names(&mut jar,CookieSource::NonHTTPCrossSite,context(false,true,true)),["none"]);
    assert_eq!(jar.query_cookies_for_source(&url(),None,CookieSource::NonHTTPCrossSite).len(),1);
}

#[test]
fn samesite_storage_rejects_restricted_cross_site_subresources_but_accepts_navigation() {
    for value in ["strict=x; SameSite=Strict; Secure", "lax=x; SameSite=Lax; Secure", "default=x; Secure"] {
        let cookie=cookie(value);
        assert!(!cookie.same_site_allows_setting(context(false,false,true)));
        assert!(cookie.same_site_allows_setting(context(false,true,false)));
        assert!(cookie.same_site_allows_setting(context(true,false,false)));
    }
    assert!(cookie("none=x; SameSite=None; Secure").same_site_allows_setting(context(false,false,false)));
}

#[test]
fn samesite_none_requires_secure_and_cross_site_script_cannot_set_lax() {
    assert!(ServoCookie::from_cookie_string("none=x; SameSite=None",&url(),CookieSource::HTTP).is_none());
    assert!(ServoCookie::from_cookie_string("none=x; SameSite=None; Secure",&url(),CookieSource::HTTP).is_some());
    for value in ["x=v; SameSite=Strict; Secure","x=v; SameSite=Lax; Secure","x=v; Secure"] {
        assert!(ServoCookie::from_cookie_string(value,&url(),CookieSource::NonHTTPCrossSite).is_none());
    }
    assert!(ServoCookie::from_cookie_string("x=v; SameSite=None; Secure",&url(),CookieSource::NonHTTPCrossSite).is_some());
    assert!(ServoCookie::from_cookie_string("x=v; HttpOnly; SameSite=None; Secure",&url(),CookieSource::NonHTTPCrossSite).is_none());
}

#[test]
fn samesite_last_attribute_including_invalid_value_is_authoritative() {
    for (attributes, expected) in [
        ("SameSite=None; SameSite=Strict",Some(SameSite::Strict)),
        ("SameSite=Strict; SameSite=None",Some(SameSite::None)),
        ("SameSite=Strict; SameSite=invalid",None),
        ("SameSite=Lax; SameSite",None),
        ("SameSite=invalid; SameSite=lAx",Some(SameSite::Lax)),
    ] { assert_eq!(cookie(&format!("x=v; Secure; {attributes}")).cookie.same_site(),expected); }
}

#[test]
fn samesite_old_insecure_none_record_is_ignored_without_losing_valid_records() {
    let directory=tempfile::tempdir().unwrap();
    let good=cookie("good=kept; Secure; SameSite=Strict");
    let mut unsafe_record=serde_json::to_value(&good).unwrap();
    unsafe_record["cookie"]="unsafe=discarded; Domain=example.org; Path=/; SameSite=None".into();
    let path=directory.path().join("ck.txt");
    std::fs::write(&path,format!("{}\n{}\n",serde_json::to_string(&good).unwrap(),unsafe_record)).unwrap();
    let jar=RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    assert_eq!(CookieStorage::access(&jar).cookies_for_url(&url(),CookieSource::HTTP).as_deref(),Some("good=kept"));
    // Loading does not rewrite user files; the rejected record cannot be sent.
    assert!(std::fs::read_to_string(path).unwrap().contains("unsafe=discarded"));
}

#[test]
fn samesite_nonpositive_max_age_expires_without_overflow() {
    for age in ["0","-1","-9223372036854775808"] {
        let mut jar=CookieStorage::new(150);
        jar.push(cookie(&format!("x=v; Max-Age={age}")),&url(),CookieSource::HTTP);
        jar.remove_all_expired_cookies();
        assert_eq!(jar.cookies_for_url(&url(),CookieSource::HTTP),None);
    }
}

#[test]
fn samesite_edited_cookie_file_uses_the_same_attribute_parser_as_http() {
    let directory=tempfile::tempdir().unwrap();
    let mut row=serde_json::to_value(cookie("edited=x; Secure; SameSite=None")).unwrap();
    row["cookie"]="edited=x; Domain=example.org; Path=/; Secure; SameSite=None; SameSite=invalid".into();
    std::fs::write(directory.path().join("ck.txt"),format!("{row}\n")).unwrap();
    let jar=RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    assert_eq!(CookieStorage::access(&jar).cookies_for_url(&url(),CookieSource::NonHTTPCrossSite),None);
    assert_eq!(CookieStorage::access(&jar).cookies_for_url(&url(),CookieSource::NonHTTP).as_deref(),Some("edited=x"));
}

#[test]
fn samesite_request_context_tracks_redirects_methods_and_null_clients() {
    let builder=|| RequestBuilder::new(None,UrlWithBlobClaim::from_url_without_having_claimed_blob(url()),Referrer::NoReferrer)
        .client(RequestClient {preloaded_resources:Default::default(),policy_container:Default::default(),
            origin:Origin::Origin(url().origin()),cookie_site:Some(url().origin()),cookie_client_is_null:false,
            is_nested_browsing_context:false,insecure_requests_policy:InsecureRequestsPolicy::DoNotUpgrade,
            has_trustworthy_ancestor_origin:false});
    assert!(builder().cookie_context().same_site);
    let mut request=builder().mode(RequestMode::Navigate).destination(Destination::Document).build();
    request.url_list.push(UrlWithBlobClaim::from_url_without_having_claimed_blob(ServoUrl::parse("https://elsewhere.test/").unwrap()));
    request.url_list.push(UrlWithBlobClaim::from_url_without_having_claimed_blob(url()));
    assert!(!request.cookie_context().same_site);
    assert!(request.cookie_context().safe_method && request.cookie_context().top_level_navigation);
    request.method=http::Method::POST;assert!(!request.cookie_context().safe_method);
    request.client.as_mut().unwrap().cookie_client_is_null=true;
    assert!(request.cookie_context().same_site);
}

#[test]
fn samesite_omitted_credentials_cannot_reuse_a_stale_cookie_header() {
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    let observed=Arc::new(AtomicBool::new(false));
    let seen=observed.clone();
    let handler=move |request: http::Request<hyper::body::Incoming>,
        response: &mut http::Response<http_body_util::combinators::BoxBody<bytes::Bytes,hyper::Error>>| {
        seen.store(!request.headers().contains_key(http::header::COOKIE),Ordering::SeqCst);
        *response.body_mut()=net::test_util::make_body(b"ok".to_vec());
    };
    let (server,url)=net::test_util::make_server(handler);
    let mut headers=http::HeaderMap::new();headers.insert(http::header::COOKIE,http::HeaderValue::from_static("stale=fixture"));
    let response=crate::fetch(RequestBuilder::new(None,url.clone(),Referrer::NoReferrer)
        .origin(url.origin()).headers(headers)
        .credentials_mode(net_traits::request::CredentialsMode::Omit).build(),None);
    let _=server.close();
    assert!(!response.is_network_error());assert!(observed.load(Ordering::SeqCst));
}
