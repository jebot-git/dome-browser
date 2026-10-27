/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;
use std::sync::Arc;
use bytes::Bytes;
use dom_struct::dom_struct;
use js::context::JSContext;
use mime::{self, Mime};
use net_traits::image_cache::{Image, ImageCache, ImageCacheResult, ImageLoadListener, ImageOrMetadataAvailable, ImageResponse, PendingImageId};
use net_traits::request::{Destination, RequestId};
use net_traits::{FetchMetadata, FetchResponseMsg, Metadata, NetworkError, ResourceFetchTiming};
use script_bindings::cell::DomRefCell;
use servo_url::ServoUrl;
use html5ever::{LocalName, Prefix, local_name, ns};
use js::rust::HandleObject;
use style::attr::AttrValue;

use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::document::Document;
use crate::dom::element::{AttributeMutation, Element, cors_setting_for_element, referrer_policy_for_element, reflect_cross_origin_attribute, set_cross_origin_attribute};
use crate::dom::bindings::codegen::Bindings::SVGImageElementBinding::SVGImageElementMethods;
use crate::dom::bindings::refcounted::Trusted;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::csp::{GlobalCspReporting, Violation};
use crate::dom::eventtarget::EventTarget;
use crate::dom::globalscope::GlobalScope;
use crate::dom::performance::performanceresourcetiming::InitiatorType;
use crate::event_loop::document_loader::{LoadBlocker, LoadType};
use crate::fetch::fetch::{RequestWithGlobalScope, create_a_potential_cors_request};
use crate::fetch::network_listener::{self, FetchResponseListener, ResourceTimingListener};
use crate::dom::svg::svgelement::SVGElement;
use crate::dom::element::attributes::storage::AttrRef;
use crate::dom::node::virtualmethods::VirtualMethods;
use crate::dom::node::{BindContext, CloneChildrenFlag, Node, NodeTraits};
use crate::dom::svg::svggraphicselement::SVGGraphicsElement;

/// <https://svgwg.org/svg2-draft/embedded.html#Placement>
const DEFAULT_WIDTH: u32 = 300;
const DEFAULT_HEIGHT: u32 = 150;

#[dom_struct]
pub(crate) struct SVGImageElement {
    svggraphicselement: SVGGraphicsElement,
    generation: Cell<u64>,
    // Same-document moves retain the current request. Attribute changes and
    // adoption invalidate it; a changed base URL selects a different resource.
    request_started: Cell<bool>,
    #[no_trace]
    request_url: DomRefCell<Option<ServoUrl>>,
    blocker: DomRefCell<Option<LoadBlocker>>,
    #[no_trace]
    image: DomRefCell<Option<Image>>,
    #[no_trace]
    snapshot_source: DomRefCell<Option<ServoUrl>>,
}

impl SVGImageElement {
    fn new_inherited(
        local_name: LocalName,
        prefix: Option<Prefix>,
        document: &Document,
    ) -> SVGImageElement {
        SVGImageElement {
            svggraphicselement: SVGGraphicsElement::new_inherited(local_name, prefix, document),
            generation: Cell::new(0),
            request_started: Cell::new(false),
            request_url: Default::default(),
            blocker: Default::default(),
            image: Default::default(),
            snapshot_source: Default::default(),
        }
    }

    pub(crate) fn new(
        cx: &mut js::context::JSContext,
        local_name: LocalName,
        prefix: Option<Prefix>,
        document: &Document,
        proto: Option<HandleObject>,
    ) -> DomRoot<SVGImageElement> {
        Node::reflect_node_with_proto(
            cx,
            Box::new(SVGImageElement::new_inherited(local_name, prefix, document)),
            document,
            proto,
        )
    }

    fn image_href(&self) -> Option<String> {
        let attrs = self.upcast::<Element>().attrs().borrow();
        attrs.iter().find(|attr| attr.namespace() == &ns!() && attr.local_name() == &local_name!("href"))
            .or_else(|| attrs.iter().find(|attr| attr.namespace() == &ns!(xlink) && attr.local_name() == &local_name!("href")))
            .map(|attr| String::from(&**attr.value()))
    }

    fn queue_image_update(&self, cx: &mut JSContext) {
        let document = self.owner_document();
        // Snapshot clones are inert and inherit only already decoded resources.
        if document.is_svg_snapshot_document() {
            return;
        }
        self.request_started.set(false);
        self.request_url.borrow_mut().take();
        self.generation.set(self.generation.get().wrapping_add(1));
        let generation = self.generation.get();
        LoadBlocker::terminate(cx, &self.blocker);
        self.image.borrow_mut().take();
        self.snapshot_source.borrow_mut().take();
        self.upcast::<SVGElement>().invalidate_enclosing_svg_viewports(cx);
        if !document.has_browsing_context() || !self.upcast::<Node>().is_connected() {
            return;
        }
        let Some(href) = self.image_href().filter(|href| !href.is_empty()) else { return };
        let url = document.base_url().join(&href).ok();
        self.request_started.set(true);
        *self.request_url.borrow_mut() = url.clone();
        if let Some(url) = url.as_ref() {
            *self.blocker.borrow_mut() = Some(LoadBlocker::new(&document, LoadType::Image(url.clone())));
        }
        // Once connection starts a request, simple removal does not cancel its
        // terminal event. Source changes and adoption invalidate the generation;
        // document teardown still cancels tasks through the owning task manager.
        let target = Trusted::new(self);
        self.owner_global().task_manager().dom_manipulation_task_source().queue(
            task!(update_svg_image: move |cx| {
                let element = target.root();
                if element.generation.get() != generation {
                    return;
                }
                if let Some(url) = url {
                    element.load_image(cx, url, generation);
                } else {
                    element.process_image_response(cx, ImageResponse::FailedToLoadOrDecode, generation);
                }
            }),
        );
    }

    fn load_image(&self, cx: &mut JSContext, url: ServoUrl, generation: u64) {
        let window = self.owner_window();
        let cache = window.image_cache();
        let cors = cors_setting_for_element(self.upcast());
        match cache.get_cached_image_status(url.clone(), window.origin().immutable(), cors) {
            ImageCacheResult::Available(ImageOrMetadataAvailable::ImageAvailable {image, url}) => {
                self.process_image_response(cx, ImageResponse::Loaded(image, url), generation);
            },
            ImageCacheResult::Available(ImageOrMetadataAvailable::MetadataAvailable(_, id)) |
            ImageCacheResult::Pending(id) => self.register_image_listener(id, generation),
            ImageCacheResult::ReadyForRequest(id) => {
                self.register_image_listener(id, generation);
                let global = self.owner_global();
                let request = create_a_potential_cors_request(
                    Some(window.webview_id()), url.clone(), Destination::Image, cors,
                    None, global.get_referrer(),
                ).with_global_scope(&global)
                 .referrer_policy(referrer_policy_for_element(self.upcast()));
                self.owner_document().fetch_background(request, SVGImageContext {
                    image_cache: cache, status: Ok(()), id, aborted: false, url,
                    element: Trusted::new(self),
                });
            },
            ImageCacheResult::FailedToLoadOrDecode => {
                self.process_image_response(cx, ImageResponse::FailedToLoadOrDecode, generation);
            },
        }
    }

    fn register_image_listener(&self, id: PendingImageId, generation: u64) {
        let target = Trusted::new(self);
        let window = self.owner_window();
        let callback = window.register_image_cache_listener(id, move |response, _| {
            let target = target.clone();
            let window = target.root().owner_window();
            window.as_global_scope().task_manager().networking_task_source().queue(
                task!(svg_image_response: move |cx| {
                    target.root().process_image_response(cx, response.response, generation);
                }),
            );
        });
        window.image_cache().add_listener(ImageLoadListener::new(callback, window.pipeline_id(), id));
    }

    fn process_image_response(&self, cx: &mut JSContext, response: ImageResponse, generation: u64) {
        if self.generation.get() != generation {
            return;
        }
        match response {
            ImageResponse::MetadataLoaded(_) => {},
            ImageResponse::Loaded(image, _) => {
                // Retain original CORS status. The serialized source is private;
                // it does not replace the public href or authorize canvas reads.
                *self.image.borrow_mut() = Some(image.clone());
                let target = Trusted::new(self);
                let task_source = self.owner_global().task_manager().networking_task_source().to_sendable();
                self.owner_window().image_cache().prepare_image_for_svg_snapshot(image, Box::new(move |source| {
                    task_source.queue(task!(svg_image_prepared: move |cx| {
                        let element = target.root();
                        if element.generation.get() != generation { return; }
                        let loaded = source.is_some();
                        *element.snapshot_source.borrow_mut() = source;
                        if !loaded { element.image.borrow_mut().take(); }
                        element.queue_image_event(cx, generation, loaded);
                    }));
                }));
            },
            ImageResponse::FailedToLoadOrDecode => {
                self.image.borrow_mut().take();
                self.snapshot_source.borrow_mut().take();
                self.queue_image_event(cx, generation, false);
            },
        }
    }

    fn queue_image_event(&self, cx: &mut JSContext, generation: u64, loaded: bool) {
        let event = if loaded { atom!("load") } else { atom!("error") };
        self.upcast::<SVGElement>().invalidate_enclosing_svg_viewports(cx);
        let target = Trusted::new(self);
        self.owner_global().task_manager().dom_manipulation_task_source().queue(
            task!(svg_image_load_event: move |cx| {
                let element = target.root();
                if element.generation.get() != generation { return; }
                // Finish this request before author code can replace the source;
                // the document's queued load event cannot run during this task.
                LoadBlocker::terminate(cx, &element.blocker);
                element.upcast::<EventTarget>().fire_event(cx, event);
            }),
        );
    }

    pub(crate) fn snapshot_source(&self) -> Option<ServoUrl> {
        self.snapshot_source.borrow().clone()
    }

}

impl VirtualMethods for SVGImageElement {
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
        if (attr.local_name() == &local_name!("href") &&
            matches!(attr.namespace(), &ns!() | &ns!(xlink))) ||
            (attr.namespace() == &ns!() && matches!(attr.local_name().as_ref(), "crossorigin" | "referrerpolicy"))
        {
            self.queue_image_update(cx);
        }
    }

    fn bind_to_tree(&self, cx: &mut JSContext, context: &BindContext) {
        self.super_type().unwrap().bind_to_tree(cx, context);
        if context.tree_connected {
            let current_url = self.image_href().filter(|href| !href.is_empty())
                .and_then(|href| self.owner_document().base_url().join(&href).ok());
            let same_request = self.request_started.get() &&
                self.request_url.borrow().as_ref() == current_url.as_ref();
            if !same_request { self.queue_image_update(cx); }
        }
    }

    fn adopting_steps(&self, cx: &mut JSContext, old_doc: &Document) {
        self.super_type().unwrap().adopting_steps(cx, old_doc);
        self.queue_image_update(cx);
    }

    fn cloning_steps(&self, cx: &mut JSContext, copy: &Node, document: Option<&Document>, children: CloneChildrenFlag) {
        self.super_type().unwrap().cloning_steps(cx, copy, document, children);
        if copy.owner_doc().is_svg_snapshot_document() {
            let copy = copy.downcast::<SVGImageElement>().unwrap();
            *copy.image.borrow_mut() = self.image.borrow().clone();
            *copy.snapshot_source.borrow_mut() = self.snapshot_source.borrow().clone();
        }
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
            local_name!("width") => AttrValue::from_u32(value.into(), DEFAULT_WIDTH),
            local_name!("height") => AttrValue::from_u32(value.into(), DEFAULT_HEIGHT),
            _ => self
                .super_type()
                .unwrap()
                .parse_plain_attribute(name, value),
        }
    }
}

struct SVGImageContext {
    /// Reference to the script thread image cache.
    image_cache: Arc<dyn ImageCache>,
    /// Indicates whether the request failed, and why
    status: Result<(), NetworkError>,
    /// The cache ID for this request.
    id: PendingImageId,
    /// Used to mark abort
    aborted: bool,
    url: ServoUrl,
    element: Trusted<SVGImageElement>,
}

impl FetchResponseListener for SVGImageContext {
    fn should_invoke(&self) -> bool {
        !self.aborted
    }

    fn process_request_body(&mut self, _: &GlobalScope, _: RequestId) {}

    fn process_response(
        &mut self,
        _: &mut JSContext,
        _: &GlobalScope,
        request_id: RequestId,
        metadata: Result<FetchMetadata, NetworkError>,
    ) {
        debug!("got {:?} for {:?}", metadata.as_ref().map(|_| ()), self.url);
        self.image_cache.notify_pending_response(
            self.id,
            FetchResponseMsg::ProcessResponse(request_id, metadata.clone()),
        );

        let metadata: Option<Metadata> = metadata.ok().map(Into::into);

        // Step 14.5 of https://html.spec.whatwg.org/multipage/#img-environment-changes
        if let Some(metadata) = metadata.as_ref() &&
            let Some(ref content_type) = metadata.content_type
        {
            let mime: Mime = content_type.clone().into_inner().into();
            if mime.type_() == mime::MULTIPART && mime.subtype().as_str() == "x-mixed-replace" {
                self.aborted = true;
            }
        }

        // The HTTP status code is ignored here. Ok NetworkError is treated
        // as real error
        self.status = match metadata.as_ref().map(|m| m.status.clone()) {
            None => Err(NetworkError::ResourceLoadError(
                "No http status code received".to_owned(),
            )),
            Some(_) => Ok(()),
        };
    }

    fn process_response_chunk(
        &mut self,
        _: &mut JSContext,
        _: &GlobalScope,
        request_id: RequestId,
        payload: Bytes,
    ) {
        if self.status.is_ok() {
            self.image_cache.notify_pending_response(
                self.id,
                FetchResponseMsg::ProcessResponseChunk(request_id, payload),
            );
        }
    }

    fn process_response_eof(
        self,
        cx: &mut JSContext,
        global: &GlobalScope,
        request_id: RequestId,
        response: Result<(), NetworkError>,
        timing: ResourceFetchTiming,
    ) {
        self.image_cache.notify_pending_response(
            self.id,
            FetchResponseMsg::ProcessResponseEOF(request_id, response.clone(), timing.clone()),
        );
        network_listener::submit_timing(cx, global, &self, &response, &timing);
    }

    fn process_csp_violations(
        &mut self,
        cx: &mut JSContext,
        global: &GlobalScope,
        _request_id: RequestId,
        violations: Vec<Violation>,
    ) {
        let elem = self.element.root();
        let source_position = elem
            .upcast::<Element>()
            .compute_source_position(0);
        global.report_csp_violations(cx, violations, None, Some(source_position));
    }

    fn process_content_length(&mut self, request_id: RequestId, size: usize) {
        self.image_cache.notify_pending_response(
            self.id,
            FetchResponseMsg::ProcessContentLength(request_id, size),
        );
    }
}

impl ResourceTimingListener for SVGImageContext {
    fn resource_timing_information(&self) -> (InitiatorType, ServoUrl) {
        (InitiatorType::LocalName("image".into()), self.url.clone())
    }
}


impl SVGImageElementMethods<crate::DomTypeHolder> for SVGImageElement {
    fn GetCrossOrigin(&self) -> Option<DOMString> {
        reflect_cross_origin_attribute(self.upcast())
    }

    fn SetCrossOrigin(&self, cx: &mut JSContext, value: Option<DOMString>) {
        set_cross_origin_attribute(cx, self.upcast(), value);
    }
}
