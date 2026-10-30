/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use js::context::JSContext;
use servo_arc::Arc;
use servo_config::pref;
use style::media_queries::MediaList;
use style::stylesheets::{AllowImportRules, Origin, Stylesheet, UrlExtraData};
use super::Document;

#[derive(Default, JSTraceable, MallocSizeOf)]
pub(super) struct State {
    css: String,
    #[no_trace]
    #[ignore_malloc_size_of = "Stylesheets are measured by layout"]
    sheet: Option<Arc<Stylesheet>>,
}

impl Document {
    /// A browser-owned user stylesheet follows the host bitmap selection even
    /// in inert smallnet documents. It creates no author nodes or script APIs.
    pub(crate) fn apply_dome_font_style(&self, cx: &mut JSContext) {
        if !fonts::bitmap_only() || self.window_detached() { return; }
        let family = pref!(fonts_default);
        if family.is_empty() { return; }
        let mut quoted = String::new();
        cssparser::serialize_string(&family, &mut quoted).unwrap();
        let pixels = pref!(fonts_default_size).clamp(4, 128);
        let css = format!("*,*::before,*::after{{font-family:{quoted},monospace!important;font-synthesis:none!important}}html,body{{font-size:{pixels}px!important}}");
        let mut state = self.dome_font_style_state().borrow_mut();
        if state.css == css { return; }
        let lock = self.shared_style_locks().ua_or_user.clone();
        let sheet = Arc::new(Stylesheet::from_str(&css, UrlExtraData(self.url().get_arc()), Origin::User,
            Arc::new(lock.wrap(MediaList::empty())), lock, None, None, self.quirks_mode(), AllowImportRules::No));
        state.css = css;
        let previous = state.sheet.replace(sheet.clone());
        drop(state);
        if let Some(previous) = previous { self.window().layout_mut().remove_stylesheet(previous); }
        self.add_stylesheet_to_stylist(sheet, None);
        self.dirty_all_nodes(cx.no_gc());
        self.invalidate_stylesheets(cx.no_gc());
    }
}
