/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;

use dom_struct::dom_struct;
use html5ever::{LocalName, Prefix};
use js::context::JSContext;
use js::rust::HandleObject;
use stylo_dom::ElementState;

use crate::dom::ElementCreator;
use crate::dom::bindings::codegen::Bindings::DocumentBinding::DocumentMethods;
use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::DomRoot;
use crate::dom::document::Document;
use crate::dom::node::virtualmethods::VirtualMethods;
use crate::dom::node::{BindContext, ChildrenMutation, MoveContext, Node, NodeTraits, UnbindContext};
use crate::dom::svg::svgelement::SVGElement;
use crate::dom::svg::svgsvgelement::SVGSVGElement;

#[dom_struct]
pub(crate) struct SVGTitleElement {
    svgelement: SVGElement,
    is_currently_being_parsed: Cell<bool>,
}

impl SVGTitleElement {
    pub(crate) fn new(
        cx: &mut JSContext,
        local_name: LocalName,
        prefix: Option<Prefix>,
        document: &Document,
        proto: Option<HandleObject>,
        creator: ElementCreator,
    ) -> DomRoot<Self> {
        Node::reflect_node_with_proto(cx, Box::new(Self {
            svgelement: SVGElement::new_inherited_with_state(
                ElementState::empty(), local_name, prefix, document,
            ),
            is_currently_being_parsed: Cell::new(creator.is_parser_created()),
        }), document, proto)
    }

    fn is_document_svg_root(parent: &Node) -> bool {
        if !parent.is::<SVGSVGElement>() { return false; }
        parent.owner_doc().GetDocumentElement().is_some_and(|root| {
            std::ptr::eq(root.upcast::<Node>(), parent)
        })
    }

    fn notify_title_changed(&self) {
        let node = self.upcast::<Node>();
        if !self.is_currently_being_parsed.get() && node.is_in_a_document_tree() && node.owner_doc().is_fully_active() &&
            node.GetParentNode().is_some_and(|parent| Self::is_document_svg_root(&parent)) {
            node.owner_doc().title_changed();
        }
    }
}

impl VirtualMethods for SVGTitleElement {
    fn super_type(&self) -> Option<&dyn VirtualMethods> {
        Some(self.upcast::<SVGElement>())
    }

    fn children_changed(&self, cx: &mut JSContext, mutation: &ChildrenMutation) {
        self.super_type().unwrap().children_changed(cx, mutation);
        self.notify_title_changed();
    }

    fn bind_to_tree(&self, cx: &mut JSContext, context: &BindContext) {
        self.super_type().unwrap().bind_to_tree(cx, context);
        self.notify_title_changed();
    }

    fn unbind_from_tree(&self, cx: &mut JSContext, context: &UnbindContext) {
        self.super_type().unwrap().unbind_from_tree(cx, context);
        // The removed node is already detached. Use its former container to
        // refresh the fallback title, including removal of an entire SVG root.
        if context.tree_is_in_a_document_tree && self.upcast::<Node>().owner_doc().is_fully_active() &&
            (Self::is_document_svg_root(context.parent) || context.parent.is::<Document>()) {
            self.upcast::<Node>().owner_doc().title_changed();
        }
    }

    fn moving_steps(&self, cx: &mut JSContext, context: &MoveContext) {
        self.super_type().unwrap().moving_steps(cx, context);
        self.notify_title_changed();
        if self.upcast::<Node>().is_in_a_document_tree() && self.upcast::<Node>().owner_doc().is_fully_active() &&
            context.old_parent.is_some_and(Self::is_document_svg_root) {
            self.upcast::<Node>().owner_doc().title_changed();
        }
    }

    fn pop(&self, cx: &mut JSContext) {
        self.super_type().unwrap().pop(cx);
        self.is_currently_being_parsed.set(false);
        self.notify_title_changed();
    }
}
