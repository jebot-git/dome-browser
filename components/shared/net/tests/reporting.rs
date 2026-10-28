/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use http::{HeaderMap, HeaderValue};
use net_traits::reporting::{ReportingEndpoint, parse_reporting_endpoints};
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
