/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use js::context::JSContext;
use paint_api::display_list::PaintTimingInfo;
use script_bindings::reflector::reflect_dom_object;
use script_traits::ProgressiveWebMetricType;
use servo_base::cross_process_instant::CrossProcessInstant;
use time::Duration;

use super::performanceentry::{EntryType, PerformanceEntry};
use crate::dom::bindings::codegen::Bindings::PerformanceBinding::DOMHighResTimeStamp;
use crate::dom::bindings::codegen::Bindings::PerformancePaintTimingBinding::PerformancePaintTimingMethods;
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::globalscope::GlobalScope;

#[dom_struct]
pub(crate) struct PerformancePaintTiming {
    entry: PerformanceEntry,
    /// <https://www.w3.org/TR/paint-timing/#paint-timing-info>
    #[no_trace]
    paint_timing_info: PaintTimingInfo,
}

impl PerformancePaintTiming {
    fn new_inherited(
        metric_type: ProgressiveWebMetricType,
        paint_timing_info: PaintTimingInfo,
        time_origin: CrossProcessInstant,
    ) -> PerformancePaintTiming {
        let name = match metric_type {
            ProgressiveWebMetricType::FirstPaint => DOMString::from_static("first-paint"),
            ProgressiveWebMetricType::FirstContentfulPaint => {
                DOMString::from_static("first-contentful-paint")
            },
            _ => DOMString::new(),
        };
        PerformancePaintTiming {
            entry: PerformanceEntry::new_inherited(
                name.into(),
                EntryType::Paint,
                // From: <https://www.w3.org/TR/paint-timing/#report-paint-timing>
                //
                // Set newEntry’s startTime attribute to the default paint
                // timestamp given paintTimingInfo.
                Some(paint_timing_info.default_paint_timestamp()),
                Duration::ZERO,
                time_origin,
            ),
            paint_timing_info,
        }
    }

    #[cfg_attr(crown, expect(crown::unrooted_must_root))]
    pub(crate) fn new(
        cx: &mut JSContext,
        global: &GlobalScope,
        metric_type: ProgressiveWebMetricType,
        paint_timing_info: PaintTimingInfo,
    ) -> DomRoot<PerformancePaintTiming> {
        let time_origin = global.performance(cx).time_origin();
        let entry =
            PerformancePaintTiming::new_inherited(metric_type, paint_timing_info, time_origin);
        reflect_dom_object(cx, Box::new(entry), global)
    }
}

impl PerformancePaintTimingMethods<crate::DomTypeHolder> for PerformancePaintTiming {
    /// <https://www.w3.org/TR/paint-timing/#dom-painttimingmixin-painttime>
    fn PaintTime(&self, _cx: &mut JSContext) -> DOMHighResTimeStamp {
        self.entry
            .to_dom_high_res_time_stamp(Some(self.paint_timing_info.paint_time()))
    }

    /// <https://www.w3.org/TR/paint-timing/#dom-painttimingmixin-presentationtime>
    fn GetPresentationTime(&self, _cx: &mut JSContext) -> Option<DOMHighResTimeStamp> {
        Some(
            self.entry
                .to_dom_high_res_time_stamp(self.paint_timing_info.presentation_time()),
        )
    }
}
