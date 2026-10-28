/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use http::{HeaderMap, HeaderValue};
use net_traits::reporting::{ReportingEndpoint, parse_reporting_endpoints};
use servo_default_resources as _;
use servo_url::ServoUrl;

fn endpoints(values: &[&str]) -> Option<Vec<ReportingEndpoint>> {
    let mut headers = HeaderMap::new();
    for value in values {
        headers.append("reporting-endpoints", HeaderValue::from_str(value).unwrap());
    }
    parse_reporting_endpoints(
        &ServoUrl::parse("https://page.test/path/document").unwrap(),
        &headers,
    )
}

#[test]
fn quoted_url_delimiters_and_parameters_do_not_split_an_endpoint() {
    let parsed = endpoints(&[
        r#"collector="/reports?a=1,b=2";ignored=?1, second="https://other.test/submit""#,
    ])
    .unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].name, "collector");
    assert_eq!(parsed[0].url.as_str(), "https://page.test/reports?a=1,b=2");
    assert_eq!(parsed[1].name, "second");
    assert_eq!(parsed[1].url.as_str(), "https://other.test/submit");
}

#[test]
fn quoted_string_escapes_are_decoded_before_parsing_the_url() {
    let parsed = endpoints(&[r#"collector="/reports?sample=\"value\"""#]).unwrap();
    assert_eq!(
        parsed[0].url.as_str(),
        "https://page.test/reports?sample=%22value%22"
    );
}

#[test]
fn repeated_fields_form_one_dictionary_and_last_duplicate_wins() {
    let parsed = endpoints(&[
        r#"collector="/old", second="/second""#,
        r#"collector="/new""#,
    ])
    .unwrap();
    assert_eq!(parsed.len(), 2);
    let collector = parsed
        .iter()
        .find(|endpoint| endpoint.name == "collector")
        .unwrap();
    assert_eq!(collector.url.as_str(), "https://page.test/new");
    assert!(parsed.iter().any(|endpoint| endpoint.name == "second"));
}

#[test]
fn absent_or_malformed_dictionary_is_not_a_partial_configuration() {
    assert!(endpoints(&[]).is_none());
    for bad in [
        r#"collector="/reports", broken=@"#,
        r#"collector="/reports", Broken="/other""#,
        r#"collector="/reports", broken="unterminated"#,
    ] {
        assert!(endpoints(&[bad]).is_none(), "{bad}");
    }
}

#[test]
fn non_string_members_and_unparseable_urls_only_skip_their_endpoint() {
    let parsed = endpoints(&[
        r#"boolean=?1, number=12, token=collector, list=("/reports"), invalid="https://[", valid="/reports""#,
    ]).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].name, "valid");
}

#[test]
fn untrustworthy_destinations_are_ignored_but_loopback_is_trustworthy() {
    let parsed = endpoints(&[
        r#"unsafe="http://remote.test/report", opaque="data:text/plain,report", secure="https://remote.test/report", loopback="http://127.0.0.1:1234/report""#,
    ]).unwrap();
    assert_eq!(
        parsed
            .iter()
            .map(|endpoint| endpoint.name.as_str())
            .collect::<Vec<_>>(),
        ["secure", "loopback"]
    );
}

#[test]
fn relative_endpoint_resolves_against_the_current_response() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "reporting-endpoints",
        HeaderValue::from_static(r#"collector="submit""#),
    );
    let url = ServoUrl::parse("https://response.test/redirected/document").unwrap();
    let parsed = parse_reporting_endpoints(&url, &headers).unwrap();
    assert_eq!(
        parsed[0].url.as_str(),
        "https://response.test/redirected/submit"
    );
}

#[test]
fn report_text_preserves_valid_unicode_and_json_metacharacters() {
    for text in [
        "",
        "plain",
        "quote\" slash\\ control\0\n",
        "žluťoučký",
        "🦊",
    ] {
        let value = net_traits::reporting::ReportText::new(text.encode_utf16().collect());
        let encoded = serde_json::to_string(&value).unwrap();
        assert_eq!(serde_json::from_str::<String>(&encoded).unwrap(), text);
    }
}

#[test]
fn report_text_preserves_unpaired_utf16_in_json_strings() {
    for (units, expected) in [
        (vec![0xd800], r#""\ud800""#),
        (vec![0xdc00], r#""\udc00""#),
        (vec![0x61, 0xd800, 0x62], r#""\u0061\ud800\u0062""#),
        (
            vec![0xd800, 0xd83e, 0xdd8a, 0xdc00],
            r#""\ud800\ud83e\udd8a\udc00""#,
        ),
    ] {
        let value = net_traits::reporting::ReportText::new(units);
        let encoded = serde_json::to_string(&value).unwrap();
        assert_eq!(encoded, expected);
        let raw: Box<serde_json::value::RawValue> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(raw.get(), expected);
    }
}

#[test]
fn report_text_escapes_cannot_add_json_members() {
    #[derive(serde::Serialize)]
    struct Body {
        attribute: net_traits::reporting::ReportText,
        unchanged: bool,
    }
    let units = vec![0xd800, 0x22, 0x7d, 0x2c, 0x22, 0x5c, 0x0a];
    let encoded = serde_json::to_string(&Body {
        attribute: net_traits::reporting::ReportText::new(units),
        unchanged: true,
    })
    .unwrap();
    assert_eq!(
        encoded,
        r#"{"attribute":"\ud800\u0022\u007d\u002c\u0022\u005c\u000a","unchanged":true}"#
    );
}

#[test]
fn delivery_outcomes_cover_http_boundaries_and_network_failure() {
    use net_traits::reporting::DeliveryOutcome as D;
    for (status, expected) in [
        (None, D::Failure),
        (Some(0), D::Failure),
        (Some(199), D::Failure),
        (Some(200), D::Success),
        (Some(204), D::Success),
        (Some(299), D::Success),
        (Some(300), D::Failure),
        (Some(304), D::Failure),
        (Some(400), D::Failure),
        (Some(410), D::RemoveEndpoint),
        (Some(500), D::Failure),
    ] {
        assert_eq!(D::from_status(status), expected, "{status:?}");
    }
}

#[test]
fn delivery_failure_streak_resets_saturates_and_retires() {
    use net_traits::reporting::DeliveryOutcome as D;
    assert_eq!(D::Failure.updated_failures(0), Some(1));
    assert_eq!(D::Failure.updated_failures(u32::MAX), Some(u32::MAX));
    assert_eq!(D::Success.updated_failures(19), Some(0));
    assert_eq!(D::RemoveEndpoint.updated_failures(19), None);
}

fn reporting_request(target: &str, site: Option<&str>) -> net_traits::request::RequestBuilder {
    use net_traits::blob_url_store::UrlWithBlobClaim;
    use net_traits::request::{InsecureRequestsPolicy, Origin, RequestClient};
    let origin = ServoUrl::parse("https://page.example/").unwrap().origin();
    net_traits::reporting::reporting_request(
        UrlWithBlobClaim::from_url_without_having_claimed_blob(ServoUrl::parse(target).unwrap()),
        origin.clone(),
        RequestClient {
            preloaded_resources: Default::default(),
            policy_container: Default::default(),
            origin: Origin::Origin(origin),
            cookie_site: site.map(|value| ServoUrl::parse(value).unwrap().origin()),
            cookie_client_is_null: false,
            is_nested_browsing_context: site.is_none(),
            insecure_requests_policy: InsecureRequestsPolicy::DoNotUpgrade,
            has_trustworthy_ancestor_origin: false,
        },
        None,
        None,
    )
}

#[test]
fn reporting_transport_has_null_client_no_window_and_no_service_worker() {
    use net_traits::request::{
        CredentialsMode, Destination, Referrer, RequestMode, ServiceWorkersMode,
        TraversableForUserPrompts,
    };
    let request = reporting_request(
        "https://page.example/collect",
        Some("https://page.example/"),
    );
    assert!(request.client.is_none());
    assert!(request.reporting_cookie_client.is_some());
    assert!(matches!(
        request.service_workers_mode,
        ServiceWorkersMode::None
    ));
    assert!(matches!(
        request.credentials_mode,
        CredentialsMode::CredentialsSameOrigin
    ));
    assert!(matches!(request.mode, RequestMode::CorsMode));
    assert!(matches!(request.referrer, Referrer::NoReferrer));
    assert!(request.target_webview_id.is_none());
    assert!(!request.use_url_credentials);
    assert!(request.unsafe_request);
    assert_eq!(request.method, http::Method::POST);
    assert_eq!(request.destination, Destination::Report);
    assert_eq!(
        request.headers[http::header::CONTENT_TYPE],
        "application/reports+json"
    );
    let request = request.build();
    assert!(request.client.is_none());
    assert!(matches!(
        request.traversable_for_user_prompts,
        TraversableForUserPrompts::NoTraversable
    ));
}

#[test]
fn reporting_null_fetch_client_preserves_ancestor_cookie_policy() {
    for (target, site, expected) in [
        (
            "https://page.example/collect",
            Some("https://page.example/"),
            true,
        ),
        ("https://page.example/collect", None, false),
        (
            "https://collector.example/collect",
            Some("https://page.example/"),
            false,
        ),
        (
            "http://page.example/collect",
            Some("https://page.example/"),
            false,
        ),
    ] {
        let builder = reporting_request(target, site);
        assert_eq!(builder.is_first_party_for_cookies(), expected);
        assert_eq!(builder.cookie_context().same_site, expected);
        let request = builder.build();
        assert_eq!(request.is_first_party_for_cookies(), expected);
        assert_eq!(request.cookie_context().same_site, expected);
        assert!(request.cookie_client().is_some());
    }
}

#[test]
fn reporting_cookie_provenance_survives_serialization_and_redirects() {
    use net_traits::blob_url_store::UrlWithBlobClaim;
    use net_traits::request::RequestBuilder;
    let builder = reporting_request(
        "https://page.example/collect",
        Some("https://page.example/"),
    );
    let encoded = serde_json::to_string(&builder).unwrap();
    let decoded: RequestBuilder = serde_json::from_str(&encoded).unwrap();
    assert!(decoded.client.is_none());
    assert!(decoded.is_first_party_for_cookies());
    let mut request = decoded.build();
    request
        .url_list
        .push(UrlWithBlobClaim::from_url_without_having_claimed_blob(
            ServoUrl::parse("https://collector.example/redirected").unwrap(),
        ));
    assert!(!request.is_first_party_for_cookies());
    assert!(!request.cookie_context().same_site);
    assert!(request.client.is_none());
    assert!(!request.cookie_context().same_site);
}

#[test]
fn reporting_provenance_is_ignored_for_other_request_destinations() {
    use net_traits::request::Destination;
    let mut builder = reporting_request(
        "https://page.example/collect",
        Some("https://page.example/"),
    );
    builder.destination = Destination::Script;
    assert!(builder.cookie_client().is_none());
    assert!(!builder.is_first_party_for_cookies());
    let request = builder.build();
    assert!(request.cookie_client().is_none());
    assert!(!request.is_first_party_for_cookies());
}

#[test]
fn older_serialized_requests_default_to_no_reporting_cookie_provenance() {
    use net_traits::request::RequestBuilder;
    let builder = reporting_request("https://page.example/collect", None);
    let mut data = serde_json::to_value(builder).unwrap();
    data.as_object_mut()
        .unwrap()
        .remove("reporting_cookie_client");
    let decoded: RequestBuilder = serde_json::from_value(data).unwrap();
    assert!(decoded.reporting_cookie_client.is_none());
}
