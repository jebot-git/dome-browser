/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use js::context::JSContext;
use net_traits::permissions_policy::{PermissionsPolicy as Policy, PolicyFeature};
use script_bindings::reflector::{Reflector, reflect_dom_object};
use servo_url::{MutableOrigin, OriginSnapshot, ServoUrl};

use crate::dom::bindings::codegen::Bindings::HTMLIFrameElementBinding::HTMLIFrameElementMethods;
use crate::dom::bindings::codegen::Bindings::PermissionsPolicyBinding::PermissionsPolicyMethods;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::bindings::str::DOMString;
use crate::dom::document::Document;
use crate::dom::htmliframeelement::HTMLIFrameElement;
use crate::dom::node::{Node, NodeTraits};

#[dom_struct]
pub(crate) struct PermissionsPolicy {
    reflector_: Reflector,
    node: Dom<Node>,
}

pub(crate) type DOMPermissionsPolicy = PermissionsPolicy;

impl PermissionsPolicy {
    pub(crate) fn new(cx: &mut JSContext, node: &Node) -> DomRoot<Self> {
        reflect_dom_object(
            cx,
            Box::new(Self {
                reflector_: Reflector::new(),
                node: Dom::from_ref(node),
            }),
            &*node.owner_window(),
        )
    }

    // Frame introspection observes the owner and attributes, never the loaded
    // child response policy. This remains usable for cross-origin children.
    fn observable(&self) -> (Policy, OriginSnapshot) {
        if let Some(document) = self.node.downcast::<Document>() {
            return (document.permissions_policy(), document.origin().snapshot());
        }
        let frame = self
            .node
            .downcast::<HTMLIFrameElement>()
            .expect("Policy associated with document or iframe");
        let origin = frame.permissions_policy_declared_origin();
        let document = frame.owner_document();
        let container = net_traits::permissions_policy::ContainerPolicy::new(
            document.permissions_policy(),
            document.origin().snapshot(),
            origin.clone(),
            &frame.Allow().str(),
        );
        (
            Policy::for_document(Some(&container), &origin, None),
            origin,
        )
    }
}

impl PermissionsPolicyMethods<crate::DomTypeHolder> for PermissionsPolicy {
    fn AllowsFeature(&self, name: DOMString, origin: Option<DOMString>) -> bool {
        let Some(feature) = PolicyFeature::parse(&name.str()) else {
            return false;
        };
        let (policy, default_origin) = self.observable();
        let origin = match origin {
            None => default_origin.clone(),
            Some(value) => {
                let Ok(url) = ServoUrl::parse(&value.str()) else {
                    return false;
                };
                if !url.origin().is_tuple() {
                    return false;
                }
                MutableOrigin::new(url.origin()).snapshot()
            },
        };
        policy.allows(feature, &origin, &default_origin)
    }

    fn Features(&self) -> Vec<DOMString> {
        PolicyFeature::ALL
            .into_iter()
            .map(|feature| feature.name().into())
            .collect()
    }

    fn AllowedFeatures(&self) -> Vec<DOMString> {
        let (policy, origin) = self.observable();
        PolicyFeature::ALL
            .into_iter()
            .filter(|feature| policy.allows(*feature, &origin, &origin))
            .map(|feature| feature.name().into())
            .collect()
    }

    fn GetAllowlistForFeature(&self, name: DOMString) -> Vec<DOMString> {
        let Some(feature) = PolicyFeature::parse(&name.str()) else {
            return vec![];
        };
        let (policy, origin) = self.observable();
        policy
            .allowlist(feature, &origin)
            .into_iter()
            .map(DOMString::from)
            .collect()
    }
}
