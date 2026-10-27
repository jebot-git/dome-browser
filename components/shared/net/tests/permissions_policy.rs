/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
use http::{HeaderMap, HeaderValue};
use net_traits::permissions_policy::{ContainerPolicy, PermissionsPolicy, PolicyFeature as F};
use servo_url::{ImmutableOrigin, MutableOrigin, OriginSnapshot, ServoUrl};

fn origin(value: &str) -> OriginSnapshot {
    MutableOrigin::new(ServoUrl::parse(value).unwrap().origin()).snapshot()
}
fn policy(value: &str, at: &OriginSnapshot) -> PermissionsPolicy {
    let mut headers = HeaderMap::new();
    headers.insert("permissions-policy", HeaderValue::from_str(value).unwrap());
    PermissionsPolicy::for_document(None, at, Some(&headers))
}
fn allows(policy: &PermissionsPolicy, at: &OriginSnapshot) -> bool {
    policy.allows(F::XrSpatialTracking, at, at)
}
fn child(
    parent: PermissionsPolicy,
    parent_origin: &OriginSnapshot,
    src: &OriginSnapshot,
    actual: &OriginSnapshot,
    allow: &str,
) -> PermissionsPolicy {
    PermissionsPolicy::for_document(
        Some(&ContainerPolicy::new(
            parent,
            parent_origin.clone(),
            src.clone(),
            allow,
        )),
        actual,
        None,
    )
}
#[test]
fn defaults_follow_the_feature_specification() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    let p = PermissionsPolicy::default();
    assert!(allows(&p, &a));
    assert!(!p.allows(F::XrSpatialTracking, &b, &a));
    assert!(!allows(&child(p.clone(), &a, &b, &b, ""), &b));
    let c = child(p, &a, &b, &b, "");
    assert!(c.allows(F::Gamepad, &b, &b));
    assert!(!c.allows(F::Geolocation, &b, &b));
}
#[test]
fn header_empty_self_all_and_origins() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    assert!(!allows(&policy("xr-spatial-tracking=()", &a), &a));
    assert!(allows(&policy("xr-spatial-tracking=self", &a), &a));
    assert!(!allows(&policy("xr-spatial-tracking=self", &a), &b));
    assert!(allows(&policy("xr-spatial-tracking=*", &a), &b));
    assert!(allows(
        &policy("xr-spatial-tracking=\"https://b.test\"", &a),
        &b
    ));
    assert!(!allows(
        &policy("xr-spatial-tracking=\"https://b.test\"", &a),
        &a
    ));
}
#[test]
fn structured_field_member_types_and_parameters() {
    let a = origin("https://a.test");
    for value in [
        "xr-spatial-tracking=?0",
        "xr-spatial-tracking=42",
        "xr-spatial-tracking=(",
        "xr-spatial-tracking=();broken=@",
    ] {
        assert!(allows(&policy(value, &a), &a), "{value}");
    }
    for value in [
        "xr-spatial-tracking=(42 ?1)",
        "xr-spatial-tracking=();report-to=collector",
        "xr-spatial-tracking=\"invalid expression\"",
    ] {
        assert!(!allows(&policy(value, &a), &a), "{value}");
    }
    assert!(allows(
        &policy("xr-spatial-tracking=(42 self);ignored", &a),
        &a
    ));
}
#[test]
fn header_repeated_fields_and_dictionary_duplicates() {
    let a = origin("https://a.test");
    let mut headers = HeaderMap::new();
    headers.append(
        "permissions-policy",
        HeaderValue::from_static("geolocation=()"),
    );
    headers.append(
        "permissions-policy",
        HeaderValue::from_static("xr-spatial-tracking=()"),
    );
    let p = PermissionsPolicy::for_document(None, &a, Some(&headers));
    assert!(!allows(&p, &a));
    assert!(!p.allows(F::Geolocation, &a, &a));
    assert!(allows(
        &policy("xr-spatial-tracking=(), xr-spatial-tracking=*", &a),
        &a
    ));
}
#[test]
fn csp_source_matching_preserves_scheme_ports_and_host_boundaries() {
    let a = origin("https://a.test");
    let p = policy(
        "xr-spatial-tracking=(\"https://*.example.test\" \"http://port.test:8080\" \"https://any.test:*\")",
        &a,
    );
    for value in [
        "https://a.example.test",
        "https://b.a.example.test",
        "http://port.test:8080",
        "https://any.test:8443",
    ] {
        assert!(allows(&p, &origin(value)), "{value}");
    }
    for value in [
        "https://example.test",
        "https://badexample.test",
        "http://a.example.test",
        "http://port.test",
        "https://a.example.test:444",
    ] {
        assert!(!allows(&p, &origin(value)), "{value}");
    }
    assert!(allows(
        &policy("xr-spatial-tracking=\"http:\"", &a),
        &origin("https://b.test")
    ));
}
#[test]
fn explicit_attribute_delegation_and_first_directive_wins() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    let p = PermissionsPolicy::default();
    for allow in [
        "xr-spatial-tracking",
        "xr-spatial-tracking 'src'",
        "xr-spatial-tracking https://b.test",
        "xr-spatial-tracking 'none' *",
    ] {
        assert!(allows(&child(p.clone(), &a, &b, &b, allow), &b), "{allow}");
    }
    for allow in [
        "xr-spatial-tracking 'none'",
        "xr-spatial-tracking 'self'",
        "xr-spatial-tracking 'none'; xr-spatial-tracking *",
        "XR-spatial-tracking *",
        "xr-spatial-tracking https://*.test",
    ] {
        assert!(!allows(&child(p.clone(), &a, &b, &b, allow), &b), "{allow}");
    }
}
#[test]
fn attribute_origin_normalization_ignores_paths_but_rejects_credentials() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    assert!(allows(
        &child(
            PermissionsPolicy::default(),
            &a,
            &b,
            &b,
            "xr-spatial-tracking https://b.test/path?q=1"
        ),
        &b
    ));
    for value in [
        "xr-spatial-tracking https://user@b.test",
        "xr-spatial-tracking blob:https://b.test/id",
    ] {
        assert!(!allows(
            &child(PermissionsPolicy::default(), &a, &b, &b, value),
            &b
        ));
    }
}
#[test]
fn declared_src_does_not_follow_a_redirect() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    let c = origin("https://c.test");
    assert!(!allows(
        &child(
            PermissionsPolicy::default(),
            &a,
            &b,
            &c,
            "xr-spatial-tracking"
        ),
        &c
    ));
    assert!(allows(
        &child(
            PermissionsPolicy::default(),
            &a,
            &b,
            &c,
            "xr-spatial-tracking *"
        ),
        &c
    ));
}
#[test]
fn ancestor_restrictions_cannot_be_reenabled() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    for header in ["xr-spatial-tracking=()", "xr-spatial-tracking=self"] {
        let container = ContainerPolicy::new(
            policy(header, &a),
            a.clone(),
            b.clone(),
            "xr-spatial-tracking *",
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            "permissions-policy",
            HeaderValue::from_static("xr-spatial-tracking=*"),
        );
        assert!(!allows(
            &PermissionsPolicy::for_document(Some(&container), &b, Some(&headers)),
            &b
        ));
    }
    let first = child(PermissionsPolicy::default(), &a, &b, &b, "");
    assert!(!allows(
        &child(first, &b, &a, &a, "xr-spatial-tracking *"),
        &a
    ));
}
#[test]
fn parent_must_allow_its_own_origin_before_it_can_delegate() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    assert!(!allows(
        &child(
            policy("xr-spatial-tracking=\"https://b.test\"", &a),
            &a,
            &b,
            &b,
            "xr-spatial-tracking *"
        ),
        &b
    ));
    assert!(allows(
        &child(
            policy("xr-spatial-tracking=(self \"https://b.test\")", &a),
            &a,
            &b,
            &b,
            "xr-spatial-tracking *"
        ),
        &b
    ));
}
#[test]
fn opaque_origins_require_identity_or_explicit_wildcard() {
    let a = origin("https://a.test");
    let opaque = MutableOrigin::new(ImmutableOrigin::new_opaque()).snapshot();
    let other = MutableOrigin::new(ImmutableOrigin::new_opaque()).snapshot();
    assert!(allows(&policy("xr-spatial-tracking=*", &a), &opaque));
    assert!(!allows(
        &policy("xr-spatial-tracking=\"https:\"", &a),
        &opaque
    ));
    assert!(allows(
        &child(
            PermissionsPolicy::default(),
            &a,
            &opaque,
            &opaque,
            "xr-spatial-tracking"
        ),
        &opaque
    ));
    assert!(!allows(
        &child(
            PermissionsPolicy::default(),
            &a,
            &opaque,
            &other,
            "xr-spatial-tracking"
        ),
        &other
    ));
    assert!(allows(
        &child(
            PermissionsPolicy::default(),
            &a,
            &opaque,
            &other,
            "xr-spatial-tracking *"
        ),
        &other
    ));
}
#[test]
fn owned_snapshots_preserve_nested_restrictions() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    let container = ContainerPolicy::new(
        policy("xr-spatial-tracking=()", &a),
        a.clone(),
        b.clone(),
        "xr-spatial-tracking *",
    );
    // Navigation snapshots are owned values and do not change when a new
    // parent response or an iframe attribute establishes a different policy.
    let cloned = container.clone();
    assert!(!allows(
        &PermissionsPolicy::for_document(Some(&cloned), &b, None),
        &b
    ));
    assert!(allows(
        &child(
            PermissionsPolicy::default(),
            &a,
            &b,
            &b,
            "xr-spatial-tracking *"
        ),
        &b
    ));
}
#[test]
fn allowlist_introspection_uses_effective_policy() {
    let a = origin("https://a.test");
    assert_eq!(
        PermissionsPolicy::default().allowlist(F::XrSpatialTracking, &a),
        ["https://a.test"]
    );
    assert_eq!(
        PermissionsPolicy::default().allowlist(F::Gamepad, &a),
        ["*"]
    );
    assert!(
        policy("xr-spatial-tracking=()", &a)
            .allowlist(F::XrSpatialTracking, &a)
            .is_empty()
    );
    assert_eq!(
        policy("xr-spatial-tracking=*", &a).allowlist(F::XrSpatialTracking, &a),
        ["*"]
    );
}

#[test]
fn source_paths_use_the_source_matcher_without_reparsing_a_header() {
    let a = origin("https://a.test");
    for value in [
        "https://a.test/nonroot",
        "https://a.test/;x",
        "https://a.test/%3Bx",
    ] {
        assert!(!allows(
            &policy(&format!("xr-spatial-tracking=\"{value}\""), &a),
            &a
        ));
    }
}

#[test]
fn header_self_follows_the_documents_live_origin_domain() {
    let a = MutableOrigin::new(ServoUrl::parse("https://a.example.test").unwrap().origin());
    let b = MutableOrigin::new(ServoUrl::parse("https://b.example.test").unwrap().origin());
    let mut p = policy("xr-spatial-tracking=self", &a.snapshot());
    a.set_domain(url::Host::parse("example.test").unwrap());
    p.refresh_self_origin(&a.snapshot());
    assert!(allows(&p, &a.snapshot()));
    assert!(!allows(&p, &b.snapshot()));
    b.set_domain(url::Host::parse("example.test").unwrap());
    assert!(allows(&p, &b.snapshot()));
}
