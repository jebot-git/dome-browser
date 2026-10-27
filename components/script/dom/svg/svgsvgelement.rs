/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use atomic_refcell::AtomicRefCell;
use base64::Engine as _;
use cssparser::Parser;
use dom_struct::dom_struct;
use html5ever::{LocalName, Prefix, local_name, ns};
use js::context::{JSContext, NoGC};
use js::rust::HandleObject;
use layout_api::SVGElementData;
use net_traits::image_cache::{Image, PendingImageId};
use percent_encoding::percent_decode;
use servo_arc::Arc as ServoArc;
use servo_url::ServoUrl;
use style::attr::AttrValue;
use style::parser::ParserContext;
use style::properties::{ComputedValues, LonghandId, PropertyDeclarationId, PropertyId};
use style::stylesheets::Origin;
use style::values::computed::SVGPaintKind;
use style::values::generics::basic_shape::ClipPath;
use style::values::generics::svg::SVGPaintFallback;
use style::values::specified::LengthPercentage;
use style_traits::{ParsingMode, ToCss};
use uuid::Uuid;
use xml5ever::serialize::TraversalScope;

use crate::dom::bindings::codegen::Bindings::DocumentBinding::DocumentMethods;
use crate::dom::bindings::codegen::Bindings::ElementBinding::ElementMethods;
use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::codegen::UnionTypes::TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::{DomRoot, LayoutDom};
use crate::dom::bindings::str::DOMString;
use crate::dom::document::Document;
use crate::dom::element::attributes::storage::{AttrRef, AttributeEntry};
use crate::dom::element::{AttributeMutation, Element};
use crate::dom::iterators::ShadowIncluding;
use crate::dom::node::virtualmethods::VirtualMethods;
use crate::dom::node::{
    ChildrenMutation, CloneChildrenFlag, Node, NodeDamage, NodeTraits, UnbindContext,
};
use crate::dom::svg::snapshot_lifecycle::SnapshotLifecycle;
use crate::dom::svg::svgdefselement::SVGDefsElement;
use crate::dom::svg::svggraphicselement::SVGGraphicsElement;
use crate::dom::svg::svgimageelement::SVGImageElement;
use crate::dom::svg::svgstopelement::SVGStopElement;

// Stack-owned roots keep private resources alive across snapshot allocation.
struct SVGUseSnapshotContext {
    definitions: Option<DomRoot<SVGDefsElement>>,
    ancestors: HashSet<String>,
    nodes_left: usize,
    canonical_ids: HashSet<String>,
    pending_resources: Vec<String>,
}

#[dom_struct]
pub(crate) struct SVGSVGElement {
    svggraphicselement: SVGGraphicsElement,
    #[no_trace]
    uuid: Uuid,
    // The XML source of subtree rooted at this SVG element, serialized into
    // a base64 encoded `data:` url. This is cached to avoid recomputation
    // on each layout and must be invalidated when the subtree changes.
    #[no_trace]
    cached_serialized_data_url: AtomicRefCell<Option<Result<ServoUrl, ()>>>,
    #[no_trace]
    completed_snapshot: AtomicRefCell<Option<ServoUrl>>,
    #[no_trace]
    #[ignore_malloc_size_of = "Only two booleans"]
    snapshot_lifecycle: Cell<SnapshotLifecycle>,
    #[no_trace]
    #[ignore_malloc_size_of = "An integer image identifier"]
    pending_snapshot_image: Cell<Option<PendingImageId>>,
}

impl SVGSVGElement {
    fn new_inherited(
        local_name: LocalName,
        prefix: Option<Prefix>,
        document: &Document,
    ) -> SVGSVGElement {
        SVGSVGElement {
            svggraphicselement: SVGGraphicsElement::new_inherited(local_name, prefix, document),
            uuid: Uuid::new_v4(),
            cached_serialized_data_url: Default::default(),
            completed_snapshot: Default::default(),
            snapshot_lifecycle: Default::default(),
            pending_snapshot_image: Default::default(),
        }
    }

    #[cfg_attr(crown, allow(crown::unrooted_must_root))]
    pub(crate) fn new(
        cx: &mut js::context::JSContext,
        local_name: LocalName,
        prefix: Option<Prefix>,
        document: &Document,
        proto: Option<HandleObject>,
    ) -> DomRoot<SVGSVGElement> {
        Node::reflect_node_with_proto(
            cx,
            Box::new(SVGSVGElement::new_inherited(local_name, prefix, document)),
            document,
            proto,
        )
    }

    pub(crate) fn serialize_and_cache_subtree(&self, cx: &mut js::context::JSContext) {
        if self.snapshot_lifecycle.get().in_flight() {
            return;
        }
        let snapshot_document = self.owner_document().svg_snapshot_document(cx);
        let document_fragment = snapshot_document.CreateDocumentFragment(cx);
        let cloned_node = Node::clone(
            cx,
            self.upcast(),
            Some(&snapshot_document),
            CloneChildrenFlag::CloneChildren,
            None,
        );
        if document_fragment
            .upcast::<Node>()
            .AppendChild(cx, &cloned_node)
            .is_err()
        {
            error!("Unable to clone SVG tree");
            self.discard_failed_snapshot();
            return;
        }

        let resources = self.preserve_paint_styles(cx, &cloned_node);
        self.process_use_elements(cx, &cloned_node, resources);
        // Style the original href attributes before replacing only private
        // snapshot resources. Pending/denied resources must not bypass fetch
        // policy through the rasterizer's data-URL decoder.
        for node in cloned_node.traverse_preorder(ShadowIncluding::No) {
            if let Some(image) = node.downcast::<SVGImageElement>() {
                let source = image.snapshot_source();
                Self::set_snapshot_href(cx, image.upcast(), source.as_ref().map_or("", ServoUrl::as_str));
            }
        }
        if !self.normalize_empty_gradients(cx, &cloned_node) {
            Self::release_snapshot_styles(cx, &cloned_node);
            self.discard_failed_snapshot();
            return;
        }

        let Ok(xml_source) = cloned_node.xml_serialize(TraversalScope::IncludeNode) else {
            Self::release_snapshot_styles(cx, &cloned_node);
            self.discard_failed_snapshot();
            return;
        };

        let xml_source: String = xml_source.into();
        Self::release_snapshot_styles(cx, &cloned_node);
        let base64_encoded_source = base64::engine::general_purpose::STANDARD.encode(xml_source);
        let data_url = format!(
            "data:image/svg+xml;base64,{base64_encoded_source}#svg-{}",
            self.uuid
        );
        match ServoUrl::parse(&data_url) {
            Ok(url) => {
                let mut lifecycle = self.snapshot_lifecycle.get();
                assert!(lifecycle.begin());
                self.snapshot_lifecycle.set(lifecycle);
                self.pending_snapshot_image.set(None);
                *self.cached_serialized_data_url.borrow_mut() = Some(Ok(url));
            },
            Err(error) => {
                error!("Unable to parse serialized SVG data url: {error}");
                self.discard_failed_snapshot();
            },
        };
    }

    // The owned XML is the only snapshot state needed after serialization.
    // These inert nodes cannot be observed by script. Release their large native
    // style allocations now instead of retaining them until a later JS GC.
    fn release_snapshot_styles(cx: &mut JSContext, root: &Node) {
        for node in root.traverse_preorder(ShadowIncluding::Yes) {
            let Some(element) = node.downcast::<Element>() else { continue; };
            element.clean_up_style_data();
            element.style_attribute().borrow_mut().take();
            let position = element.attrs().borrow().iter().position(|attr| {
                attr.namespace() == &ns!() && attr.local_name() == &local_name!("style")
            });
            if let Some(position) = position {
                // Bypass mutation steps: this is private allocation disposal,
                // not a page mutation, CSP check or custom-element reaction.
                if let AttributeEntry::Dom(attr) = element.attrs().remove(position) {
                    let attr = DomRoot::from_ref(&*attr);
                    attr.swap_value(&mut AttrValue::String(String::new()));
                    attr.set_owner(cx, None);
                }
            }
        }
    }

    // The rasterizer receives a standalone SVG document, without the surrounding
    // page's stylesheets or inherited color. Preserve computed paint on the clone
    // so stylesheet-driven icons (including currentColor) keep their page colors.
    fn preserve_paint_styles(&self, cx: &mut JSContext, cloned_node: &Node) -> Vec<String> {
        self.preserve_subtree_paint_styles(cx, self.upcast(), cloned_node)
    }

    fn preserve_subtree_paint_styles(
        &self,
        cx: &mut JSContext,
        original_root: &Node,
        cloned_node: &Node,
    ) -> Vec<String> {
        let document = self.owner_document();
        let mut resources = Vec::new();
        let properties = [
            ("opacity", LonghandId::Opacity),
            ("display", LonghandId::Display),
            ("visibility", LonghandId::Visibility),
            ("clip-path", LonghandId::ClipPath),
            ("paint-order", LonghandId::PaintOrder),
            ("color", LonghandId::Color),
            ("stop-color", LonghandId::StopColor),
            ("stop-opacity", LonghandId::StopOpacity),
            ("fill", LonghandId::Fill),
            ("fill-opacity", LonghandId::FillOpacity),
            ("fill-rule", LonghandId::FillRule),
            ("stroke", LonghandId::Stroke),
            ("stroke-opacity", LonghandId::StrokeOpacity),
            ("stroke-width", LonghandId::StrokeWidth),
            ("stroke-linecap", LonghandId::StrokeLinecap),
            ("stroke-linejoin", LonghandId::StrokeLinejoin),
            ("stroke-dasharray", LonghandId::StrokeDasharray),
            ("stroke-dashoffset", LonghandId::StrokeDashoffset),
            ("stroke-miterlimit", LonghandId::StrokeMiterlimit),
        ];
        for (original, cloned) in original_root
            .traverse_preorder(ShadowIncluding::No)
            .zip(cloned_node.traverse_preorder(ShadowIncluding::No))
        {
            let (Some(original), Some(cloned)) =
                (original.downcast::<Element>(), cloned.downcast::<Element>())
            else {
                continue;
            };
            // Serialization follows layout. Do not trigger another reflow here.
            let Some(style) = original.style_without_reflow() else {
                continue;
            };
            let mut declarations = String::new();
            {
                // Serialize a canonical declaration list. The rasterizer stops
                // at empty declarations and gives an earlier !important value
                // priority, so retain only properties that the snapshot does not
                // replace with resolved computed values below.
                let guard = document.style_shared_author_lock().read();
                if let Some(block) = original.style_attribute().borrow().as_ref() {
                    let mut block = block.read_with(&guard).clone();
                    // Variables have already been substituted by the native
                    // cascade. The rasterizer's simpler declaration tokenizer
                    // stops at custom properties, losing later display/paint.
                    let custom_properties: Vec<_> = block.declarations().iter()
                        .filter_map(|declaration| match declaration.id() {
                            PropertyDeclarationId::Custom(name) => Some(PropertyId::Custom(name.clone())),
                            _ => None,
                        })
                        .collect();
                    for property in custom_properties {
                        if let Some(first) = block.first_declaration_to_remove(&property) {
                            block.remove_property(&property, first);
                        }
                    }
                    for (_, id) in properties {
                        let property = PropertyId::NonCustom(id.into());
                        if let Some(first) = block.first_declaration_to_remove(&property) {
                            block.remove_property(&property, first);
                        }
                    }
                    block
                        .to_css(&mut declarations)
                        .expect("Writing CSS to a String cannot fail");
                }
            }
            if !declarations.is_empty() && !declarations.ends_with(';') {
                declarations.push(';');
            }
            for (name, id) in properties {
                let mut value = style.computed_value_to_string(PropertyDeclarationId::Longhand(id));
                // The standalone rasterizer accepts legacy SVG colors, while
                // computed CSS can retain Lab, Oklab, P3 and other color spaces.
                // Convert only this private raster snapshot; CSSOM stays intact.
                match id {
                    LonghandId::Color => {
                        value = style.get_inherited_text().get_color()
                            .into_srgb_legacy().to_css_string();
                    },
                    LonghandId::StopColor => {
                        value = style.resolve_color(&style.get_svg().stop_color)
                            .into_srgb_legacy().to_css_string();
                    },
                    LonghandId::Fill | LonghandId::Stroke => {
                        let paint = if id == LonghandId::Fill {
                            &style.get_inherited_svg().fill
                        } else {
                            &style.get_inherited_svg().stroke
                        };
                        if let SVGPaintKind::Color(color) = &paint.kind {
                            value = style.resolve_color(color).into_srgb_legacy().to_css_string();
                        }
                    },
                    _ => {},
                }
                // The outer SVG's opacity is already applied by layout to the
                // raster image. Descendants still composite inside that image.
                if id == LonghandId::Opacity && std::ptr::eq(original, self.upcast::<Element>()) {
                    value = String::from("1");
                }
                if value.contains("url(") {
                    // The standalone raster document has a data: URL. Rebase
                    // same-document paint servers to its cloned fragment IDs,
                    // including references supplied by surrounding stylesheets.
                    let (url, fallback) = match id {
                        LonghandId::Fill | LonghandId::Stroke => {
                            let paint = if id == LonghandId::Fill {
                                &style.get_inherited_svg().fill
                            } else {
                                &style.get_inherited_svg().stroke
                            };
                            let SVGPaintKind::PaintServer(url) = &paint.kind else {
                                continue;
                            };
                            let fallback = match &paint.fallback {
                                SVGPaintFallback::Color(color) => {
                                    style.resolve_color(color).into_srgb_legacy().to_css_string()
                                },
                                other => other.to_css_string(),
                            };
                            (url, Some(fallback))
                        },
                        LonghandId::ClipPath => {
                            let ClipPath::Url(url) = &style.get_svg().clip_path else {
                                continue;
                            };
                            (url, None)
                        },
                        _ => continue,
                    };
                    let target = url
                        .url()
                        .map(|url| ServoUrl::from_url(url.as_ref().clone()));
                    let fragment = target
                        .as_ref()
                        .filter(|target| target.is_equal_excluding_fragments(&document.url()))
                        .and_then(|target| target.fragment());
                    if let Some(fragment) = fragment {
                        let decoded = percent_decode(fragment.as_bytes()).decode_utf8().ok();
                        let fragment = decoded.as_deref().unwrap_or(fragment);
                        resources.push(fragment.to_owned());
                        value = String::from("url(");
                        cssparser::serialize_string(&format!("#{fragment}"), &mut value)
                            .expect("Writing CSS to a String cannot fail");
                        value.push(')');
                        if let Some(fallback) = fallback {
                            value.push(' ');
                            value.push_str(if fallback.is_empty() {
                                "none"
                            } else {
                                &fallback
                            });
                        }
                    } else if let Some(fallback) = fallback {
                        // The standalone rasterizer cannot load external SVG
                        // paint servers. Use their specified fallback, or no
                        // paint when omitted; never bind to a local ID instead.
                        value = if fallback.is_empty() {
                            String::from("none")
                        } else {
                            fallback
                        };
                    } else {
                        // External clip resources are outside this snapshot's
                        // existing resource support. Never rebase them locally.
                        continue;
                    }
                }
                declarations.push_str(&format!("{name}:{value} !important;"));
            }
            let _ = cloned.SetAttribute(
                cx,
                DOMString::from("style"),
                TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString::String(DOMString::from(
                    declarations,
                )),
            );
        }
        resources
    }

    fn process_use_elements(&self, cx: &mut JSContext, root_node: &Node, resources: Vec<String>) {
        // Collect before appending definitions: imported instances are expanded
        // recursively with their own host style, never revisited as shared data.
        let uses: Vec<_> = self
            .upcast::<Node>()
            .traverse_preorder(ShadowIncluding::No)
            .zip(root_node.traverse_preorder(ShadowIncluding::No))
            .filter_map(|(original, cloned)| {
                let original = original.downcast::<Element>()?;
                let cloned = cloned.downcast::<Element>()?;
                if original.namespace() != &ns!(svg) ||
                    original.local_name() != &local_name!("use")
                {
                    return None;
                }
                Some((
                    DomRoot::from_ref(original),
                    DomRoot::from_ref(cloned),
                    original.style_without_reflow()?,
                ))
            })
            .collect();
        let mut context = SVGUseSnapshotContext {
            definitions: None,
            ancestors: HashSet::new(),
            // Match the rasterizer's node bound and bound native recursion too.
            nodes_left: 1_000_000usize.saturating_sub(
                root_node.traverse_preorder(ShadowIncluding::No).count(),
            ),
            canonical_ids: root_node.traverse_preorder(ShadowIncluding::No)
                .filter_map(|node| {
                    let element = node.downcast::<Element>()?;
                    let id = element.get_string_attribute(&local_name!("id"));
                    (!id.str().is_empty()).then(|| id.str().to_owned())
                }).collect(),
            pending_resources: resources,
        };
        context.pending_resources.extend(self.normalize_paint_resource_links(cx, root_node));
        for (original, cloned, style) in uses {
            self.process_use_instance(cx, &original, &cloned, style, root_node, &mut context);
        }
        self.import_use_paint_resources(cx, root_node, &mut context);
    }

    fn normalize_empty_gradients(&self, cx: &mut JSContext, root: &Node) -> bool {
        let mut index = HashMap::new();
        let mut gradients = Vec::new();
        let mut count = 0usize;
        for node in root.traverse_preorder(ShadowIncluding::No) {
            count += 1;
            let Some(element) = node.downcast::<Element>() else { continue; };
            let id = element.get_string_attribute(&local_name!("id"));
            if !id.str().is_empty() {
                index.entry(id.str().to_owned()).or_insert_with(|| DomRoot::from_ref(element));
            }
            if element.namespace() == &ns!(svg) &&
                matches!(element.local_name(), &local_name!("linearGradient") | &local_name!("radialGradient"))
            {
                gradients.push(DomRoot::from_ref(element));
            }
        }
        let mut resolved = HashMap::<String, bool>::new();
        let mut empty = Vec::new();
        for gradient in gradients {
            let mut current = gradient.clone();
            let mut path = HashSet::new();
            let has_stops = loop {
                let id = current.get_string_attribute(&local_name!("id")).str().to_owned();
                if !id.is_empty() {
                    if let Some(result) = resolved.get(&id) { break *result; }
                    if !path.insert(id) { break false; }
                }
                if current.upcast::<Node>().children().any(|node| {
                    node.downcast::<Element>().is_some_and(|element| {
                        element.namespace() == &ns!(svg) && element.local_name() == &local_name!("stop")
                    })
                }) {
                    break true;
                }
                let href = current.get_string_attribute(&local_name!("href"));
                let view = href.str();
                let Some(id) = view.strip_prefix('#') else { break false; };
                let Some(next) = index.get(id) else { break false; };
                if next.namespace() != &ns!(svg) ||
                    !matches!(next.local_name(), &local_name!("linearGradient") | &local_name!("radialGradient"))
                {
                    break false;
                }
                current = next.clone();
            };
            resolved.extend(path.into_iter().map(|id| (id, has_stops)));
            if !has_stops { empty.push(gradient); }
        }
        if count.saturating_add(empty.len()) > 1_000_000 { return false; }
        let document = root.owner_doc();
        for gradient in empty {
            // SVG2 paints an empty gradient as none. The rasterizer otherwise
            // mistakes it for an unresolved URL and selects a fallback color.
            // A private transparent stop preserves none without changing CSSOM.
            let stop = SVGStopElement::new(cx, local_name!("stop"), None, &document, None);
            let _ = stop.upcast::<Element>().SetAttribute(cx, DOMString::from("style"),
                TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString::String(DOMString::from(
                    "stop-color:transparent !important;stop-opacity:0 !important;opacity:0 !important;",
                )));
            let _ = gradient.upcast::<Node>().AppendChild(cx, stop.upcast());
        }
        true
    }

    fn normalize_paint_resource_links(&self, cx: &mut JSContext, root: &Node) -> Vec<String> {
        let document = self.owner_document();
        let mut resources = Vec::new();
        for node in root.traverse_preorder(ShadowIncluding::No) {
            let Some(element) = node.downcast::<Element>() else { continue; };
            if element.namespace() != &ns!(svg) ||
                !matches!(element.local_name(), &local_name!("linearGradient") |
                    &local_name!("radialGradient") | &local_name!("pattern"))
            {
                continue;
            }
            let href = {
                let attrs = element.attrs().borrow();
                [ns!(), ns!(xlink)].iter().find_map(|namespace| {
                    attrs.iter()
                        .find(|attr| attr.namespace() == namespace && attr.local_name() == &local_name!("href"))
                        .map(|attr| String::from(&**attr.value()))
                })
            };
            let Some(href) = href else { continue; };
            Self::set_snapshot_href(cx, element, "");
            let Ok(target) = document.base_url().join(&href) else { continue; };
            if !target.is_equal_excluding_fragments(&document.url()) {
                Self::set_snapshot_href(cx, element, target.as_str());
                continue;
            }
            if let Some(fragment) = target.fragment()
                && let Ok(id) = percent_decode(fragment.as_bytes()).decode_utf8()
            {
                Self::set_snapshot_href(cx, element, &format!("#{id}"));
                resources.push(id.into_owned());
            }
        }
        resources
    }

    fn import_use_paint_resources(
        &self,
        cx: &mut JSContext,
        snapshot_root: &Node,
        context: &mut SVGUseSnapshotContext,
    ) {
        let document = self.owner_document();
        while let Some(id) = context.pending_resources.pop() {
            if !context.canonical_ids.insert(id.clone()) { continue; }
            let Some(original) = document.GetElementById(cx, DOMString::from(id)) else { continue; };
            if original.namespace() != &ns!(svg) { continue; }
            let original_node = original.upcast::<Node>();
            let count = original_node.traverse_preorder(ShadowIncluding::No)
                .take(context.nodes_left.saturating_add(1)).count();
            if count > context.nodes_left { continue; }
            context.nodes_left -= count;
            let cloned = Node::clone(cx, original_node, Some(&snapshot_root.owner_doc()), CloneChildrenFlag::CloneChildren, None);
            for node in cloned.traverse_preorder(ShadowIncluding::No) {
                if let Some(element) = node.downcast::<Element>() {
                    let id = element.get_string_attribute(&local_name!("id"));
                    if !id.str().is_empty() { context.canonical_ids.insert(id.str().to_owned()); }
                }
            }
            // Paint URLs address the defining document. Their resource subtree
            // keeps that document's computed styles, not the use host's values.
            context.pending_resources.extend(
                self.preserve_subtree_paint_styles(cx, original_node, &cloned),
            );
            context.pending_resources.extend(self.normalize_paint_resource_links(cx, &cloned));
            let uses: Vec<_> = original_node.traverse_preorder(ShadowIncluding::No)
                .zip(cloned.traverse_preorder(ShadowIncluding::No))
                .filter_map(|(original, cloned)| {
                    let original = original.downcast::<Element>()?;
                    let cloned = cloned.downcast::<Element>()?;
                    if original.namespace() != &ns!(svg) || original.local_name() != &local_name!("use") {
                        return None;
                    }
                    Some((DomRoot::from_ref(original), DomRoot::from_ref(cloned), original.style_without_reflow()?))
                }).collect();
            for (original, cloned, style) in uses {
                self.process_use_instance(cx, &original, &cloned, style, snapshot_root, context);
            }
            if context.definitions.is_none() {
                let defs = SVGDefsElement::new(cx, local_name!("defs"), None, &snapshot_root.owner_doc(), None);
                let _ = snapshot_root.AppendChild(cx, defs.upcast());
                context.definitions = Some(defs);
            }
            let _ = context.definitions.as_ref().unwrap().upcast::<Node>().AppendChild(cx, &cloned);
        }
    }

    fn set_snapshot_href(cx: &mut JSContext, element: &Element, href: &str) {
        element.remove_attribute(cx, &ns!(xlink), &local_name!("href"));
        let _ = element.SetAttribute(
            cx,
            DOMString::from("href"),
            TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString::String(DOMString::from(href)),
        );
    }

    fn process_use_instance(
        &self,
        cx: &mut JSContext,
        source_use: &Element,
        snapshot_use: &Element,
        inherited: ServoArc<ComputedValues>,
        snapshot_root: &Node,
        context: &mut SVGUseSnapshotContext,
    ) {
        let href = {
            let attrs = source_use.attrs().borrow();
            [ns!(), ns!(xlink)].iter().find_map(|namespace| {
                attrs
                    .iter()
                    .find(|attr| {
                        attr.namespace() == namespace && attr.local_name() == &local_name!("href")
                    })
                    .map(|attr| String::from(&**attr.value()))
            })
        };
        let Some(href) = href else {
            return;
        };
        // Invalid and circular references must not bind to an existing source
        // definition accidentally. Modern href presence wins even when empty.
        Self::set_snapshot_href(cx, snapshot_use, "");
        let document = self.upcast::<Node>().owner_doc();
        let Ok(target) = document.base_url().join(&href) else {
            return;
        };
        if !target.is_equal_excluding_fragments(&document.url()) {
            Self::set_snapshot_href(cx, snapshot_use, target.as_str());
            return;
        }
        let Some(fragment) = target.fragment() else {
            return;
        };
        let Ok(id) = percent_decode(fragment.as_bytes()).decode_utf8() else {
            return;
        };
        let Some(referenced) = document.GetElementById(cx, DOMString::from(id.as_ref())) else {
            return;
        };
        if referenced.namespace() != &ns!(svg) || context.ancestors.len() >= 64 {
            return;
        }
        let referenced_node = referenced.upcast::<Node>();
        if source_use.upcast::<Node>()
            .inclusive_ancestors_unrooted(cx.no_gc(), ShadowIncluding::No)
            .any(|ancestor| std::ptr::eq(&**ancestor, referenced_node))
        {
            return;
        }
        if !context.ancestors.insert(id.to_string()) {
            return;
        }
        let node_count = referenced_node
            .traverse_preorder(ShadowIncluding::No)
            .take(context.nodes_left.saturating_add(1))
            .count();
        if node_count > context.nodes_left {
            context.ancestors.remove(id.as_ref());
            return;
        }
        context.nodes_left -= node_count;
        let instance = Node::clone(cx, referenced_node, Some(&snapshot_root.owner_doc()), CloneChildrenFlag::CloneChildren, None);
        // Match source rules against the detached instance's own ancestry, but
        // inherit values and custom properties from this specific use host.
        self.owner_window().layout().style_svg_use_instance(
            instance.to_trusted_node_address(),
            inherited,
            document.animation_manager().sets(),
            document.current_animation_timeline_value(),
        );
        let prefix = format!("dome-instance-{}", Uuid::new_v4());
        let mut ids = HashMap::new();
        for node in instance.traverse_preorder(ShadowIncluding::No) {
            if let Some(element) = node.downcast::<Element>() {
                let old_id = element.get_string_attribute(&local_name!("id"));
                if !old_id.str().is_empty() {
                    let next = ids.len();
                    ids.entry(old_id.str().to_owned()).or_insert_with(|| format!("{prefix}-{next}"));
                }
            }
        }
        let Some(instance_id) = ids.get(id.as_ref()).cloned() else {
            context.ancestors.remove(id.as_ref());
            return;
        };
        // Paint is resolved only after the native instance cascade. Keep the
        // clone detached until nested uses and fragment identities are complete.
        context.pending_resources.extend(self.preserve_subtree_paint_styles(cx, &instance, &instance));
        context.pending_resources.extend(self.normalize_paint_resource_links(cx, &instance));
        let nested: Vec<_> = instance
            .traverse_preorder(ShadowIncluding::No)
            .filter_map(|node| {
                let element = node.downcast::<Element>()?;
                if element.namespace() != &ns!(svg) ||
                    element.local_name() != &local_name!("use")
                {
                    return None;
                }
                Some((DomRoot::from_ref(element), element.style_without_reflow()?))
            })
            .collect();
        for (element, style) in nested {
            self.process_use_instance(cx, &element, &element, style, snapshot_root, context);
        }
        // Instance identities remain private. Paint URLs continue to address
        // canonical resources from the defining document, imported separately.
        for node in instance.traverse_preorder(ShadowIncluding::No) {
            let Some(element) = node.downcast::<Element>() else {
                continue;
            };
            let old_id = element.get_string_attribute(&local_name!("id"));
            if let Some(new_id) = ids.get(old_id.str().as_ref()) {
                let _ = element.SetAttribute(cx, DOMString::from("id"),
                    TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString::String(DOMString::from(new_id.as_str())));
            }
        }
        if context.definitions.is_none() {
            let defs = SVGDefsElement::new(cx, local_name!("defs"), None, &snapshot_root.owner_doc(), None);
            let _ = snapshot_root.AppendChild(cx, defs.upcast());
            context.definitions = Some(defs);
        }
        let _ = context.definitions.as_ref().unwrap().upcast::<Node>().AppendChild(cx, &instance);
        Self::set_snapshot_href(cx, snapshot_use, &format!("#{instance_id}"));
        context.ancestors.remove(id.as_ref());
    }

    pub(crate) fn invalidate_cached_serialized_subtree_and_rasterization_result(
        &self,
        no_gc: &NoGC,
    ) {
        let mut lifecycle = self.snapshot_lifecycle.get();
        let replace = lifecycle.invalidate();
        self.snapshot_lifecycle.set(lifecycle);
        if replace {
            *self.cached_serialized_data_url.borrow_mut() = None;
        }
        self.upcast::<Node>().dirty(no_gc, NodeDamage::Other);
    }

    pub(crate) fn note_pending_snapshot_image(&self, id: PendingImageId) {
        if self.snapshot_lifecycle.get().in_flight() && self.pending_snapshot_image.get().is_none()
        {
            self.pending_snapshot_image.set(Some(id));
        }
    }

    pub(crate) fn snapshot_rasterized(&self, id: PendingImageId, no_gc: &NoGC) {
        if !self.snapshot_lifecycle.get().in_flight() {
            return;
        }
        let Some(Ok(source)) = self.cached_serialized_data_url.borrow().clone() else {
            return;
        };
        let window = self.owner_window();
        let matches = matches!(window.image_cache().get_image(source.clone(), window.origin().immutable().clone(), None),
            Some(Image::Vector(image)) if image.id == id);
        if !matches {
            return;
        }
        if window
            .image_cache()
            .get_rasterized_vector_image(id)
            .is_none()
        {
            self.pending_snapshot_image.set(Some(id));
            self.snapshot_failed(id, no_gc);
            return;
        }
        let previous = self.completed_snapshot.borrow_mut().replace(source.clone());
        if let Some(previous) = previous.filter(|previous| *previous != source) {
            self.retire_snapshot(&previous);
        }
        self.finish_snapshot(no_gc);
    }

    pub(crate) fn snapshot_failed(&self, id: PendingImageId, no_gc: &NoGC) {
        if self.pending_snapshot_image.get() != Some(id) {
            return;
        }
        self.discard_failed_snapshot();
        self.finish_snapshot(no_gc);
    }

    fn discard_failed_snapshot(&self) {
        let current = self
            .cached_serialized_data_url
            .borrow_mut()
            .replace(Err(()))
            .and_then(Result::ok);
        let previous = self.completed_snapshot.borrow_mut().take();
        if let Some(source) = current {
            self.retire_snapshot(&source);
        }
        if let Some(source) = previous {
            self.retire_snapshot(&source);
        }
    }

    fn finish_snapshot(&self, no_gc: &NoGC) {
        let mut lifecycle = self.snapshot_lifecycle.get();
        let newer = lifecycle.complete();
        self.snapshot_lifecycle.set(lifecycle);
        self.pending_snapshot_image.set(None);
        if newer {
            *self.cached_serialized_data_url.borrow_mut() = None;
        }
        self.upcast::<Node>().dirty(no_gc, NodeDamage::Other);
    }

    fn retire_snapshot(&self, source: &ServoUrl) {
        let window = self.owner_window();
        window.layout_mut().remove_cached_image(source);
        window
            .image_cache()
            .evict_svg_snapshot(source, window.origin().immutable());
    }

    fn release_snapshots(&self) {
        self.snapshot_lifecycle.set(Default::default());
        self.pending_snapshot_image.set(None);
        let current = self
            .cached_serialized_data_url
            .borrow_mut()
            .take()
            .and_then(Result::ok);
        let completed = self.completed_snapshot.borrow_mut().take();
        if let Some(source) = current {
            self.retire_snapshot(&source);
        }
        if let Some(source) = completed {
            self.retire_snapshot(&source);
        }
        self.owner_window()
            .image_cache()
            .evict_rasterized_image(&self.uuid);
    }
}

impl<'dom> LayoutDom<'dom, SVGSVGElement> {
    pub(crate) fn data(self) -> SVGElementData<'dom> {
        let svg_id = self.unsafe_get().uuid;
        let element = self.upcast::<Element>();
        let width = element.get_attr_for_layout(&ns!(), &local_name!("width"));
        let height = element.get_attr_for_layout(&ns!(), &local_name!("height"));
        let view_box = element.get_attr_for_layout(&ns!(), &local_name!("viewBox"));
        SVGElementData {
            source: self
                .unsafe_get()
                .cached_serialized_data_url
                .borrow()
                .clone(),
            completed_source: self.unsafe_get().completed_snapshot.borrow().clone(),
            snapshot_pending: self.unsafe_get().snapshot_lifecycle.get().in_flight(),
            width,
            height,
            view_box,
            svg_id,
        }
    }
}

impl VirtualMethods for SVGSVGElement {
    fn super_type(&self) -> Option<&dyn VirtualMethods> {
        Some(self.upcast::<SVGGraphicsElement>() as &dyn VirtualMethods)
    }

    fn attribute_mutated(
        &self,
        cx: &mut js::context::JSContext,
        attr: AttrRef<'_>,
        mutation: AttributeMutation,
    ) {
        self.super_type()
            .unwrap()
            .attribute_mutated(cx, attr, mutation);

        self.invalidate_cached_serialized_subtree_and_rasterization_result(cx.no_gc());
    }

    fn attribute_affects_presentational_hints(&self, attr: AttrRef<'_>) -> bool {
        match attr.local_name() {
            &local_name!("width") | &local_name!("height") => true,
            _ => self
                .super_type()
                .unwrap()
                .attribute_affects_presentational_hints(attr),
        }
    }

    fn parse_plain_attribute(&self, name: &LocalName, value: DOMString) -> AttrValue {
        match *name {
            local_name!("width") | local_name!("height") => {
                let value = &value.str();
                let parser = &mut Parser::new(value);
                let doc = self.owner_document();
                let url = doc.url().into_url().into();
                let context = ParserContext::new(
                    Origin::Author,
                    &url,
                    None,
                    ParsingMode::ALLOW_UNITLESS_LENGTH,
                    doc.quirks_mode(),
                    /* namespaces = */ Default::default(),
                    None,
                    None,
                    /* attr_taint = */ Default::default(),
                );
                let val = LengthPercentage::parse_quirky(
                    &context,
                    parser,
                    style::values::specified::AllowQuirks::Always,
                );
                AttrValue::LengthPercentage(value.to_string(), val.ok())
            },
            _ => self
                .super_type()
                .unwrap()
                .parse_plain_attribute(name, value),
        }
    }

    fn children_changed(&self, cx: &mut JSContext, mutation: &ChildrenMutation) {
        if let Some(super_type) = self.super_type() {
            super_type.children_changed(cx, mutation);
        }

        self.invalidate_cached_serialized_subtree_and_rasterization_result(cx.no_gc());
    }

    fn unbind_from_tree(&self, cx: &mut js::context::JSContext, context: &UnbindContext<'_>) {
        if let Some(s) = self.super_type() {
            s.unbind_from_tree(cx, context);
        }

        self.release_snapshots();
    }
}
