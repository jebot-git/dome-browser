#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue};
    use net_traits::policy_container::{EmbedderPolicy, EmbedderPolicyValue::*};
    fn headers(values: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (key, value) in values {
            headers.append(*key, HeaderValue::from_static(value));
        }
        headers
    }
    #[test]
    fn structured_item_token_and_parameters() {
        for value in [
            "require-corp",
            "require-corp;flag",
            "require-corp;report-to=\"collector\"",
        ] {
            assert_eq!(
                EmbedderPolicy::from_headers(
                    Some(&headers(&[("cross-origin-embedder-policy", value)])),
                    true
                )
                .value,
                RequireCorp
            );
        }
        for value in [
            "Require-Corp",
            "\"require-corp\"",
            "require-corp, require-corp",
            "unknown-value",
            "require-corp;broken=@",
            "require-corp junk",
            "?1",
            "42",
            "unsafe-none",
        ] {
            assert_eq!(
                EmbedderPolicy::from_headers(
                    Some(&headers(&[("cross-origin-embedder-policy", value)])),
                    true
                )
                .value,
                UnsafeNone,
                "{value}"
            );
        }
    }
    #[test]
    fn duplicates_and_non_secure_context_fail_open() {
        let duplicate = headers(&[
            ("cross-origin-embedder-policy", "require-corp"),
            ("cross-origin-embedder-policy", "require-corp"),
        ]);
        assert_eq!(
            EmbedderPolicy::from_headers(Some(&duplicate), true).value,
            UnsafeNone
        );
        let valid = headers(&[
            ("cross-origin-embedder-policy", "require-corp"),
            ("cross-origin-embedder-policy-report-only", "credentialless"),
        ]);
        let result = EmbedderPolicy::from_headers(Some(&valid), false);
        assert_eq!(result.value, UnsafeNone);
        assert_eq!(result.report_only_value, UnsafeNone);
        assert_eq!(EmbedderPolicy::from_headers(None, true).value, UnsafeNone);
    }
    #[test]
    fn report_only_and_endpoints_are_independent() {
        let values = headers(&[
            (
                "cross-origin-embedder-policy",
                "credentialless;report-to=\"enforced\"",
            ),
            (
                "cross-origin-embedder-policy-report-only",
                "require-corp;report-to=\"observed\"",
            ),
        ]);
        let result = EmbedderPolicy::from_headers(Some(&values), true);
        assert_eq!(result.value, Credentialless);
        assert_eq!(result.reporting_endpoint.as_deref(), Some("enforced"));
        assert_eq!(result.report_only_value, RequireCorp);
        assert_eq!(
            result.report_only_reporting_endpoint.as_deref(),
            Some("observed")
        );
        let values = headers(&[(
            "cross-origin-embedder-policy-report-only",
            "require-corp;report-to=token",
        )]);
        let result = EmbedderPolicy::from_headers(Some(&values), true);
        assert_eq!(result.value, UnsafeNone);
        assert_eq!(result.report_only_value, RequireCorp);
        assert!(result.report_only_reporting_endpoint.is_none());
    }
}

#[test]
fn only_isolating_policies_satisfy_child_adherence() {
    use net_traits::policy_container::EmbedderPolicyValue::*;
    assert!(!UnsafeNone.compatible_with_cross_origin_isolation());
    assert!(RequireCorp.compatible_with_cross_origin_isolation());
    assert!(Credentialless.compatible_with_cross_origin_isolation());
}
