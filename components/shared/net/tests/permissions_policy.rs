/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
use http::{HeaderMap, HeaderValue};
use net_traits::permissions_policy::{
    ContainerPolicy, PermissionsPolicy, PolicyFeature as F, ViolationDisposition as D,
};
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

fn reporting_policy(enforced: &str, report_only: &str, at: &OriginSnapshot) -> PermissionsPolicy {
    let mut headers = HeaderMap::new();
    headers.insert(
        "permissions-policy",
        HeaderValue::from_str(enforced).unwrap(),
    );
    headers.insert(
        "permissions-policy-report-only",
        HeaderValue::from_str(report_only).unwrap(),
    );
    PermissionsPolicy::for_document(None, at, Some(&headers))
}

#[test]
fn report_only_has_a_separate_decision_and_destination() {
    let a = origin("https://a.test");
    let p = reporting_policy(
        "xr-spatial-tracking=*;report-to=enforced",
        "xr-spatial-tracking=();report-to=diagnostics",
        &a,
    );
    assert!(allows(&p, &a));
    assert_eq!(p.allowlist(F::XrSpatialTracking, &a), ["*"]);
    let check = p.check(F::XrSpatialTracking, &a, &a);
    assert!(check.allowed);
    let violation = check.violation.unwrap();
    assert_eq!(violation.disposition, D::Report);
    assert_eq!(violation.endpoint.as_deref(), Some("diagnostics"));
    // Other features retain their own state and produce no spurious report.
    assert!(p.check(F::Geolocation, &a, &a).violation.is_none());
    assert!(p.check(F::Gamepad, &a, &a).violation.is_none());
}

#[test]
fn enforced_denial_takes_precedence_over_report_only() {
    let a = origin("https://a.test");
    for report_only in [
        "xr-spatial-tracking=*;report-to=diagnostics",
        "xr-spatial-tracking=();report-to=diagnostics",
    ] {
        let p = reporting_policy("xr-spatial-tracking=();report-to=enforced", report_only, &a);
        let check = p.check(F::XrSpatialTracking, &a, &a);
        assert!(!check.allowed);
        let violation = check.violation.unwrap();
        assert_eq!(violation.disposition, D::Enforce);
        assert_eq!(violation.endpoint.as_deref(), Some("enforced"));
    }
}

#[test]
fn reporting_parameter_is_a_member_token_and_not_an_allowlist_item_parameter() {
    let a = origin("https://a.test");
    for value in [
        "xr-spatial-tracking=();report-to=collector",
        "xr-spatial-tracking=unknown;report-to=collector",
        "xr-spatial-tracking=(\"https://other.test\");report-to=collector",
    ] {
        let p = reporting_policy("", value, &a);
        let check = p.check(F::XrSpatialTracking, &a, &a);
        assert!(check.allowed, "{value}");
        assert_eq!(
            check.violation.unwrap().endpoint.as_deref(),
            Some("collector"),
            "{value}"
        );
    }
    for value in [
        "xr-spatial-tracking=();report-to=\"collector\"",
        "xr-spatial-tracking=();report-to=?1",
        "xr-spatial-tracking=();report-to=42",
        "xr-spatial-tracking=(\"https://other.test\";report-to=collector)",
    ] {
        let check = reporting_policy("", value, &a).check(F::XrSpatialTracking, &a, &a);
        assert!(check.allowed, "{value}");
        assert_eq!(check.violation.unwrap().endpoint, None, "{value}");
    }
}

#[test]
fn malformed_report_only_does_not_replace_enforced_policy() {
    let a = origin("https://a.test");
    for report_only in [
        "xr-spatial-tracking=(",
        "xr-spatial-tracking=();report-to=@",
        "unknown-feature=();report-to=collector",
    ] {
        let p = reporting_policy("xr-spatial-tracking=*", report_only, &a);
        assert!(p.check(F::XrSpatialTracking, &a, &a).violation.is_none());
        let p = reporting_policy("xr-spatial-tracking=()", report_only, &a);
        let check = p.check(F::XrSpatialTracking, &a, &a);
        assert!(!check.allowed);
        assert_eq!(check.violation.unwrap().disposition, D::Enforce);
    }
}

#[test]
fn repeated_report_only_fields_and_duplicate_declarations_use_structured_fields() {
    let a = origin("https://a.test");
    let mut headers = HeaderMap::new();
    for value in [
        "geolocation=();report-to=geo",
        "xr-spatial-tracking=*;report-to=old, xr-spatial-tracking=();report-to=xr",
    ] {
        headers.append(
            "permissions-policy-report-only",
            HeaderValue::from_str(value).unwrap(),
        );
    }
    let p = PermissionsPolicy::for_document(None, &a, Some(&headers));
    assert_eq!(
        p.check(F::Geolocation, &a, &a)
            .violation
            .unwrap()
            .endpoint
            .as_deref(),
        Some("geo")
    );
    assert_eq!(
        p.check(F::XrSpatialTracking, &a, &a)
            .violation
            .unwrap()
            .endpoint
            .as_deref(),
        Some("xr")
    );
}

#[test]
fn report_only_restrictions_inherit_without_enforcing_or_inheriting_endpoints() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    let c = origin("https://c.test");
    let parent = reporting_policy(
        "xr-spatial-tracking=*",
        "xr-spatial-tracking=();report-to=parent",
        &a,
    );
    let p = child(parent, &a, &b, &b, "xr-spatial-tracking *");
    let check = p.check(F::XrSpatialTracking, &b, &b);
    assert!(check.allowed);
    let violation = check.violation.unwrap();
    assert_eq!(violation.disposition, D::Report);
    assert_eq!(violation.endpoint, None);
    let container = ContainerPolicy::new(p, b, c.clone(), "xr-spatial-tracking *");
    let mut headers = HeaderMap::new();
    headers.insert(
        "permissions-policy-report-only",
        HeaderValue::from_static("xr-spatial-tracking=*;report-to=child"),
    );
    // A descendant response cannot undo its inherited report-only restriction.
    let p = PermissionsPolicy::for_document(Some(&container), &c, Some(&headers));
    let check = p.check(F::XrSpatialTracking, &c, &c);
    assert!(check.allowed);
    let violation = check.violation.unwrap();
    assert_eq!(violation.disposition, D::Report);
    assert_eq!(violation.endpoint.as_deref(), Some("child"));
}

#[test]
fn enforced_ancestor_restrictions_do_not_turn_into_report_only_restrictions() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    let parent = reporting_policy(
        "xr-spatial-tracking=();report-to=parent",
        "xr-spatial-tracking=*",
        &a,
    );
    let p = child(parent, &a, &b, &b, "xr-spatial-tracking *");
    let check = p.check(F::XrSpatialTracking, &b, &b);
    assert!(!check.allowed);
    let violation = check.violation.unwrap();
    assert_eq!(violation.disposition, D::Enforce);
    assert_eq!(violation.endpoint, None);
}

#[test]
fn container_delegation_and_parent_origin_apply_to_both_dispositions() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    let parent = reporting_policy("xr-spatial-tracking=*", "xr-spatial-tracking=self", &a);
    let p = child(parent, &a, &b, &b, "xr-spatial-tracking *");
    assert!(p.check(F::XrSpatialTracking, &b, &b).allowed);
    assert_eq!(
        p.check(F::XrSpatialTracking, &b, &b)
            .violation
            .unwrap()
            .disposition,
        D::Report
    );
    let parent = reporting_policy(
        "xr-spatial-tracking=*",
        "xr-spatial-tracking=\"https://b.test\"",
        &a,
    );
    let p = child(parent, &a, &b, &b, "xr-spatial-tracking *");
    assert_eq!(
        p.check(F::XrSpatialTracking, &b, &b)
            .violation
            .unwrap()
            .disposition,
        D::Report
    );
    let parent = reporting_policy("xr-spatial-tracking=*", "xr-spatial-tracking=*", &a);
    let p = child(parent, &a, &b, &b, "xr-spatial-tracking 'none'");
    assert!(!p.check(F::XrSpatialTracking, &b, &b).allowed);
    assert_eq!(
        p.check(F::XrSpatialTracking, &b, &b)
            .violation
            .unwrap()
            .disposition,
        D::Enforce
    );
}

#[test]
fn a_new_navigation_replaces_both_response_policies_and_their_destinations() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    let container = ContainerPolicy::new(
        PermissionsPolicy::default(),
        a.clone(),
        b.clone(),
        "xr-spatial-tracking *",
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        "permissions-policy-report-only",
        HeaderValue::from_static("xr-spatial-tracking=();report-to=old"),
    );
    let old = PermissionsPolicy::for_document(Some(&container), &b, Some(&headers));
    assert!(old.check(F::XrSpatialTracking, &b, &b).violation.is_some());
    let next = PermissionsPolicy::for_document(Some(&container), &b, None);
    assert!(next.check(F::XrSpatialTracking, &b, &b).violation.is_none());
}

#[test]
fn refreshing_live_self_origin_updates_both_policies_without_changing_snapshots() {
    let a = MutableOrigin::new(ServoUrl::parse("https://a.example.test").unwrap().origin());
    let b = MutableOrigin::new(ServoUrl::parse("https://b.example.test").unwrap().origin());
    let mut p = reporting_policy(
        "xr-spatial-tracking=*",
        "xr-spatial-tracking=self;report-to=collector",
        &a.snapshot(),
    );
    let old_snapshot = p.clone();
    a.set_domain(url::Host::parse("example.test").unwrap());
    b.set_domain(url::Host::parse("example.test").unwrap());
    p.refresh_self_origin(&a.snapshot());
    assert!(
        p.check(F::XrSpatialTracking, &b.snapshot(), &a.snapshot())
            .violation
            .is_none()
    );
    assert_eq!(
        old_snapshot
            .check(F::XrSpatialTracking, &b.snapshot(), &a.snapshot())
            .violation
            .unwrap()
            .disposition,
        D::Report
    );
}
