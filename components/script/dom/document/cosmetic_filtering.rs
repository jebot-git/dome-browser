/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashSet;

use cssparser::ToCss;
use embedder_traits::{CosmeticFilterRequest, EmbedderMsg};
use html5ever::local_name;
use js::context::JSContext;
use servo_arc::Arc;
use servo_base::generic_channel::GenericCallback;
use servo_config::pref;
use servo_url::ServoUrl;
use style::media_queries::MediaList;
use style::selector_parser::SelectorParser;
use style::str::split_html_space_chars;
use style::stylesheets::{AllowImportRules, Origin, Stylesheet, UrlExtraData};

use super::Document;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::refcounted::Trusted;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::Dom;
use crate::dom::element::Element;
use crate::dom::node::{Node, NodeTraits};
use crate::dom::iterators::ShadowIncluding;

/// Browser-owned state. No DOM nodes, author styles or script-visible handles
/// are created for the hiding stylesheet.
#[derive(Default, JSTraceable, MallocSizeOf)]
pub(super) struct CosmeticFilterState {
    pending: HashSet<Dom<Element>>,
    classes: HashSet<String>,
    ids: HashSet<String>,
    selectors: HashSet<String>,
    #[no_trace]
    #[ignore_malloc_size_of = "Stylesheets are measured by layout"]
    sheet: Option<Arc<Stylesheet>>,
    generation: u64,
    started: bool,
    queued: bool,
    in_flight: bool,
}

// User styles already apply in author shadow trees. Discover their tokens
// without exposing closed roots to script or treating UA widget internals as ads.
fn connected_author_element(element: &Element) -> bool {
    let node = element.upcast::<Node>();
    node.is_connected() &&
        node.containing_shadow_root().is_none_or(|root| !root.is_user_agent_widget())
}

fn cosmetic_document_url(url: &ServoUrl) -> bool {
    matches!(url.scheme(), "http" | "https" | "blob" | "data") ||
        (url.scheme() == "about" && matches!(url.path(), "blank" | "srcdoc"))
}

impl Document {
    /// Local documents can inherit an HTTP(S) security origin without having an
    /// HTTP(S) URL. Use the immutable document origin for hostname rules, never
    /// an author-controlled base URL, referrer or the text inside a blob URL.
    /// Opaque blob/data documents use a neutral context for generic rules only;
    /// their actual URL still guards asynchronous replies below.
    fn cosmetic_context_url(&self, url: &ServoUrl) -> ServoUrl {
        if matches!(url.scheme(), "http" | "https") { return url.clone(); }
        let origin = self.origin();
        if !matches!(origin.scheme(), Some("http" | "https")) {
            return if matches!(url.scheme(), "blob" | "data") {
                ServoUrl::parse("about:blank").expect("literal neutral cosmetic context")
            } else { url.clone() };
        }
        ServoUrl::parse(&format!("{}/", origin.immutable().ascii_serialization()))
            .unwrap_or_else(|_| url.clone())
    }

    /// Native cosmetic queries must settle before a display frame is built.
    /// Layout-only queries remain available, so scripts can measure their DOM.
    pub(crate) fn cosmetic_filter_render_blocked(&self) -> bool {
        if !pref!(network_cosmetic_filtering_enabled) { return false; }
        let state = self.cosmetic_filter_state().borrow();
        state.queued || state.in_flight
    }

    pub(crate) fn note_cosmetic_element(&self, element: Option<&Element>) {
        if !pref!(network_cosmetic_filtering_enabled) || !self.has_browsing_context() ||
            !cosmetic_document_url(&self.url()) {
            return;
        }
        let mut state = self.cosmetic_filter_state().borrow_mut();
        if let Some(element) = element {
            if !connected_author_element(element) { return; }
            state.pending.insert(Dom::from_ref(element));
        }
        if state.queued || state.in_flight { return; }
        state.queued = true;
        drop(state);
        let document = Trusted::new(self);
        self.owner_global().task_manager().dom_manipulation_task_source()
            .queue(task!(query_cosmetic_filters: move |cx| {
                document.root().query_cosmetic_filters(cx);
            }));
    }

    pub(crate) fn queue_cosmetic_filter_reset(&self) {
        if !pref!(network_cosmetic_filtering_enabled) { return; }
        let document = Trusted::new(self);
        self.owner_global().task_manager().dom_manipulation_task_source()
            .queue(task!(reset_cosmetic_filters: move |cx| {
                document.root().reset_cosmetic_filters(cx);
            }));
    }

    pub(crate) fn reset_cosmetic_filters(&self, cx: &mut JSContext) {
        if !self.is_fully_active() || !self.has_browsing_context() { return; }
        let mut state = self.cosmetic_filter_state().borrow_mut();
        let generation = state.generation.wrapping_add(1);
        let previous = state.sheet.take();
        *state = CosmeticFilterState { generation, ..Default::default() };
        drop(state);
        if let Some(sheet) = previous {
            self.window().layout_mut().remove_stylesheet(sheet);
            self.dirty_all_nodes(cx.no_gc());
        }
        // Refresh existing content as well as future mutations after a policy
        // update. Use the actual document, never the UI's history URL.
        for node in self.upcast::<Node>().traverse_preorder(ShadowIncluding::Yes) {
            if let Some(element) = node.downcast::<Element>() {
                self.note_cosmetic_element(Some(element));
            }
        }
        self.note_cosmetic_element(None);
    }

    fn query_cosmetic_filters(&self, _cx: &mut JSContext) {
        let mut state = self.cosmetic_filter_state().borrow_mut();
        state.queued = false;
        if !self.is_fully_active() || state.in_flight { return; }
        // Bound historical token caches. Eviction only causes repeat queries;
        // installed selectors stay active and new tokens are never discarded.
        if state.classes.len() + state.ids.len() > 8192 {
            state.classes.clear(); state.ids.clear();
        }
        let pending = std::mem::take(&mut state.pending);
        let mut classes = HashSet::new();
        let mut ids = HashSet::new();
        for element in &pending {
            if !connected_author_element(element) || !std::ptr::eq(&*element.owner_document(), self) { continue; }
            let value = element.get_string_attribute(&local_name!("class"));
            for class in split_html_space_chars(&value.str()) {
                if state.classes.insert(class.to_owned()) { classes.insert(class.to_owned()); }
            }
            let id = element.get_string_attribute(&local_name!("id"));
            if !id.is_empty() && state.ids.insert(id.to_string()) { ids.insert(id.to_string()); }
        }
        if state.started && classes.is_empty() && ids.is_empty() { return; }
        state.started = true;
        state.in_flight = true;
        let generation = state.generation;
        drop(state);
        let url = self.url();
        let request = CosmeticFilterRequest {
            url: self.cosmetic_context_url(&url),
            top_level_origin: self.cookie_site().filter(|origin| origin.is_tuple())
                .map(|origin| origin.ascii_serialization().into_owned()),
            classes: classes.into_iter().collect(), ids: ids.into_iter().collect(),
        };
        let mut root = Some(Trusted::new(self));
        let task_source = self.owner_global().task_manager().dom_manipulation_task_source().to_sendable();
        let callback = GenericCallback::new(move |reply: Result<Vec<String>, _>| {
            let Some(document) = root.take() else { return; };
            let selectors = reply.unwrap_or_default();
            let url = url.clone();
            task_source.queue(task!(apply_cosmetic_filters: move |cx| {
                let document = document.root();
                if document.cosmetic_filter_state().borrow().generation != generation { return; }
                document.cosmetic_filter_state().borrow_mut().in_flight = false;
                if document.is_fully_active() && document.url() == url {
                    document.apply_cosmetic_selectors(cx, selectors);
                    if !document.cosmetic_filter_state().borrow().pending.is_empty() {
                        document.note_cosmetic_element(None);
                    }
                }
            }));
        });
        match callback {
            Ok(callback) => self.send_to_embedder(EmbedderMsg::CosmeticFilters(self.webview_id(), request, callback)),
            Err(_) => self.cosmetic_filter_state().borrow_mut().in_flight = false,
        }
    }

    fn apply_cosmetic_selectors(&self, cx: &mut JSContext, selectors: Vec<String>) {
        let url = self.url();
        let mut state = self.cosmetic_filter_state().borrow_mut();
        let mut changed = false;
        for selector in selectors {
            // Parse and serialize selector syntax before forming a declaration.
            // This prevents a list entry from injecting CSS rules or fetches.
            if let Ok(selector) = SelectorParser::parse_author_origin_no_namespace(
                &selector, &UrlExtraData(url.get_arc()),
            ) {
                changed |= state.selectors.insert(selector.to_css_string());
            }
        }
        if !changed { return; }
        let mut selectors: Vec<_> = state.selectors.iter().collect(); selectors.sort();
        let css: String = selectors.into_iter().map(|s| format!("{s}{{display:none!important}}\n")).collect();
        let lock = self.shared_style_locks().ua_or_user.clone();
        let sheet = Arc::new(Stylesheet::from_str(
            &css, UrlExtraData(url.get_arc()), Origin::User, Arc::new(lock.wrap(MediaList::empty())), lock,
            None, None, self.quirks_mode(), AllowImportRules::No,
        ));
        let previous = state.sheet.replace(sheet.clone());
        drop(state);
        if let Some(previous) = previous { self.window().layout_mut().remove_stylesheet(previous); }
        self.add_stylesheet_to_stylist(sheet, None);
        self.dirty_all_nodes(cx.no_gc());
        self.invalidate_stylesheets(cx.no_gc());
    }
}
