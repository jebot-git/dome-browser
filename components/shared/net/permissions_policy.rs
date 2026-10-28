/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Document and navigation state for Permissions Policy. This is deliberately
//! separate from the HTML policy container: header policy is not copied to a
//! subsequently navigated document, while container restrictions are retained.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use content_security_policy::{
    CheckResult, CspList, Directive, Policy, PolicyDisposition, PolicySource,
};
use http::HeaderMap;
use malloc_size_of_derive::MallocSizeOf;
use regex::Regex;
use serde::{Deserialize, Serialize};
use servo_url::OriginSnapshot;
use sfv::{BareItem, Dictionary, ListEntry, Parser};
use url::Url;

use crate::fetch::headers::get_value_from_header_list;

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, MallocSizeOf, Ord, PartialEq, PartialOrd, Serialize,
)]
pub enum PolicyFeature {
    Geolocation,
    Gamepad,
    XrSpatialTracking,
}

impl PolicyFeature {
    pub const ALL: [Self; 3] = [Self::Geolocation, Self::Gamepad, Self::XrSpatialTracking];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|feature| feature.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Geolocation => "geolocation",
            Self::Gamepad => "gamepad",
            Self::XrSpatialTracking => "xr-spatial-tracking",
        }
    }

    pub fn default_all(self) -> bool {
        // Gamepad has a wildcard default; WebXR and Geolocation default to self.
        self == Self::Gamepad
    }
}

// Permissions-source-expression is CSP's scheme-source or host-source. Keep
// the grammar at this boundary; matching itself uses the engine's CSP matcher.
static SOURCE_EXPRESSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|(?:[a-zA-Z][a-zA-Z0-9+.-]*://)?(?:\*|(?:\*\.)?[a-zA-Z0-9-]+(?:\.[a-zA-Z0-9-]+)*\.?)(?::(?:\*|[0-9]+))?(?:/(?:[:@%!$&'()*+,;=0-9a-zA-Z._~-]+)?(?:/[:@%!$&'()*+,;=0-9a-zA-Z._~-]*)*)?)$"
).expect("Permissions Policy source expression grammar")
});

#[derive(Clone, Debug, Default, Deserialize, MallocSizeOf, Serialize)]
struct Allowlist {
    all: bool,
    self_origin: Option<OriginSnapshot>,
    src_origin: Option<OriginSnapshot>,
    expressions: Vec<String>,
}

impl Allowlist {
    fn matches(&self, origin: &OriginSnapshot) -> bool {
        if self.all
            || self
                .self_origin
                .as_ref()
                .is_some_and(|value| value.same_origin_domain(origin))
            || self
                .src_origin
                .as_ref()
                .is_some_and(|value| value.same_origin_domain(origin))
        {
            return true;
        }
        if !origin.immutable().is_tuple() {
            return false;
        }
        let Ok(url) = Url::parse(&origin.immutable().ascii_serialization()) else {
            return false;
        };
        self.expressions.iter().any(|source| {
            // base-uri delegates to the same source-list matching algorithm as
            // Permissions Policy, with the origin URL and redirect count zero.
            let policy = CspList(vec![Policy {
                directive_set: vec![Directive {
                    name: "base-uri".into(),
                    value: vec![source.clone()],
                }],
                source: PolicySource::Header,
                disposition: PolicyDisposition::Enforce,
            }]);
            policy.is_base_allowed_for_document(&url, &url.origin()).0 == CheckResult::Allowed
        })
    }

    fn strings(&self) -> Vec<String> {
        if self.all {
            return vec!["*".into()];
        }
        let mut result = vec![];
        for origin in [&self.self_origin, &self.src_origin].into_iter().flatten() {
            let value = origin.immutable().ascii_serialization().into_owned();
            if !result.contains(&value) {
                result.push(value);
            }
        }
        result.extend(self.expressions.iter().cloned());
        result
    }
}

type Declarations = BTreeMap<PolicyFeature, Allowlist>;

#[derive(Clone, Copy, Debug, Eq, MallocSizeOf, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ViolationDisposition {
    Enforce,
    Report,
}

impl ViolationDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enforce => "enforce",
            Self::Report => "report",
        }
    }
}

/// Diagnostics for a feature check. A report-only violation never changes the
/// enforced decision, and an enforced violation takes precedence over it.
#[derive(Debug, Eq, PartialEq)]
pub struct PolicyViolation {
    pub disposition: ViolationDisposition,
    pub endpoint: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct PolicyCheck {
    pub allowed: bool,
    pub violation: Option<PolicyViolation>,
}

#[derive(Clone, Debug, Default, Deserialize, MallocSizeOf, Serialize)]
struct PolicyState {
    // Absent inherited entries are Enabled (the empty top-level policy).
    inherited: BTreeMap<PolicyFeature, bool>,
    declared: Declarations,
    // Configuration belongs to this response. An ancestor's endpoint must not
    // become a destination for reports from a different document.
    reporting: BTreeMap<PolicyFeature, String>,
}

fn header_policy(
    headers: Option<&HeaderMap>,
    origin: &OriginSnapshot,
    header: &str,
) -> PolicyState {
    let Some(bytes) = headers.and_then(|headers| get_value_from_header_list(header, headers))
    else {
        return PolicyState::default();
    };
    let Ok(dictionary) = Parser::new(&bytes).parse::<Dictionary>() else {
        return PolicyState::default();
    };
    let mut state = PolicyState::default();
    for (name, entry) in dictionary {
        let Some(feature) = PolicyFeature::parse(name.as_str()) else {
            continue;
        };
        let (items, params) = match entry {
            ListEntry::Item(item) => match item.bare_item {
                value @ (BareItem::Token(_) | BareItem::String(_)) => (vec![value], item.params),
                _ => continue,
            },
            ListEntry::InnerList(list) => (
                list.items.into_iter().map(|item| item.bare_item).collect(),
                list.params,
            ),
        };
        if let Some(BareItem::Token(endpoint)) = params.get("report-to") {
            state
                .reporting
                .insert(feature, endpoint.as_str().to_owned());
        }
        let mut allowlist = Allowlist::default();
        for item in items {
            match item {
                BareItem::Token(token) if token.as_str() == "*" => allowlist.all = true,
                BareItem::Token(token) if token.as_str() == "self" => {
                    allowlist.self_origin = Some(origin.clone())
                },
                BareItem::String(source) if SOURCE_EXPRESSION.is_match(source.as_str()) => {
                    allowlist.expressions.push(source.as_str().to_owned())
                },
                _ => {},
            }
        }
        state.declared.insert(feature, allowlist);
    }
    state
}

fn container_policy(value: &str, origin: &OriginSnapshot, src: &OriginSnapshot) -> Declarations {
    let mut declarations = Declarations::new();
    for directive in value.split(';') {
        let mut words = directive.split_ascii_whitespace();
        let Some(feature) = words.next().and_then(PolicyFeature::parse) else {
            continue;
        };
        // The first occurrence wins in the HTML attribute serialization.
        if declarations.contains_key(&feature) {
            continue;
        }
        let words: Vec<_> = words.collect();
        let mut allowlist = Allowlist::default();
        if words.is_empty() {
            allowlist.src_origin = Some(src.clone());
        }
        for word in words {
            if word == "*" {
                allowlist.all = true;
            } else if word.eq_ignore_ascii_case("'self'") {
                allowlist.self_origin = Some(origin.clone());
            } else if word.eq_ignore_ascii_case("'src'") {
                allowlist.src_origin = Some(src.clone());
            } else if let Ok(url) = Url::parse(word) {
                if url.scheme() != "blob"
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.origin().is_tuple()
                    && !url.host_str().is_some_and(|host| host.contains('*'))
                {
                    allowlist
                        .expressions
                        .push(url.origin().ascii_serialization());
                }
            }
        }
        declarations.insert(feature, allowlist);
    }
    declarations
}

#[derive(Clone, Debug, Default, Deserialize, MallocSizeOf, Serialize)]
pub struct PermissionsPolicy {
    enforced: PolicyState,
    report_only: PolicyState,
}

#[derive(Clone, Debug, Deserialize, MallocSizeOf, Serialize)]
pub struct ContainerPolicy {
    parent: PermissionsPolicy,
    parent_origin: OriginSnapshot,
    declared: Declarations,
}

impl ContainerPolicy {
    pub fn new(
        parent: PermissionsPolicy,
        parent_origin: OriginSnapshot,
        src_origin: OriginSnapshot,
        allow: &str,
    ) -> Self {
        let declared = container_policy(allow, &parent_origin, &src_origin);
        Self {
            parent,
            parent_origin,
            declared,
        }
    }

    fn inherits(&self, feature: PolicyFeature, origin: &OriginSnapshot, report_only: bool) -> bool {
        let parent = if report_only {
            &self.parent.report_only
        } else {
            &self.parent.enforced
        };
        if !parent.feature_value(feature, &self.parent_origin)
            || !parent.feature_value(feature, origin)
        {
            return false;
        }
        self.declared.get(&feature).map_or_else(
            || feature.default_all() || origin.same_origin(&self.parent_origin),
            |allowlist| allowlist.matches(origin),
        )
    }
}

impl PermissionsPolicy {
    /// Header self-origin denotes the document's live origin, including domain
    /// relaxation. Inherited policy remains the value established at navigation.
    pub fn refresh_self_origin(&mut self, origin: &OriginSnapshot) {
        for allowlist in self
            .enforced
            .declared
            .values_mut()
            .chain(self.report_only.declared.values_mut())
        {
            if allowlist.self_origin.is_some() {
                allowlist.self_origin = Some(origin.clone());
            }
        }
    }

    pub fn for_document(
        container: Option<&ContainerPolicy>,
        origin: &OriginSnapshot,
        headers: Option<&HeaderMap>,
    ) -> Self {
        let make_state = |report_only| {
            let header = if report_only {
                "permissions-policy-report-only"
            } else {
                "permissions-policy"
            };
            let mut state = header_policy(headers, origin, header);
            state.inherited = PolicyFeature::ALL
                .into_iter()
                .map(|feature| {
                    (
                        feature,
                        container.is_none_or(|container| {
                            container.inherits(feature, origin, report_only)
                        }),
                    )
                })
                .collect();
            state
        };
        Self {
            enforced: make_state(false),
            report_only: make_state(true),
        }
    }

    /// Compute the enforced decision and the report, if any, without emitting
    /// it. Introspection uses `allows` instead and has no reporting side effect.
    /// <https://w3c.github.io/webappsec-permissions-policy/#is-feature-enabled>
    pub fn check(
        &self,
        feature: PolicyFeature,
        origin: &OriginSnapshot,
        document_origin: &OriginSnapshot,
    ) -> PolicyCheck {
        let allowed = self.enforced.allows(feature, origin, document_origin);
        let violation = if !allowed {
            Some((&self.enforced, ViolationDisposition::Enforce))
        } else if !self.report_only.allows(feature, origin, document_origin) {
            Some((&self.report_only, ViolationDisposition::Report))
        } else {
            None
        };
        PolicyCheck {
            allowed,
            violation: violation.map(|(state, disposition)| PolicyViolation {
                disposition,
                endpoint: state.reporting.get(&feature).cloned(),
            }),
        }
    }

    pub fn allows(
        &self,
        feature: PolicyFeature,
        origin: &OriginSnapshot,
        document_origin: &OriginSnapshot,
    ) -> bool {
        self.enforced.allows(feature, origin, document_origin)
    }

    pub fn allowlist(&self, feature: PolicyFeature, origin: &OriginSnapshot) -> Vec<String> {
        if !self.allows(feature, origin, origin) {
            return vec![];
        }
        self.enforced.declared.get(&feature).map_or_else(
            || {
                if feature.default_all() {
                    vec!["*".into()]
                } else {
                    vec![origin.immutable().ascii_serialization().into_owned()]
                }
            },
            Allowlist::strings,
        )
    }
}

impl PolicyState {
    fn feature_value(&self, feature: PolicyFeature, origin: &OriginSnapshot) -> bool {
        self.inherited.get(&feature).copied().unwrap_or(true)
            && self
                .declared
                .get(&feature)
                .is_none_or(|allowlist| allowlist.matches(origin))
    }

    fn allows(
        &self,
        feature: PolicyFeature,
        origin: &OriginSnapshot,
        document_origin: &OriginSnapshot,
    ) -> bool {
        self.feature_value(feature, origin)
            && (self.declared.contains_key(&feature)
                || feature.default_all()
                || origin.same_origin(document_origin))
    }
}
