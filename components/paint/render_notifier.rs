/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use paint_api::{PaintMessage, PaintProxy};
use servo_base::id::PainterId;
use std::sync::atomic::{AtomicU64, Ordering};
use webrender_api::{DocumentId, FramePublishId, FrameReadyParams};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(crate) struct RenderNotifier {
    painter_id: PainterId,
    paint_proxy: PaintProxy,
    generation: u64,
}

impl RenderNotifier {
    pub(crate) fn new(painter_id: PainterId, paint_proxy: PaintProxy) -> RenderNotifier {
        RenderNotifier {
            painter_id,
            paint_proxy,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

impl webrender_api::RenderNotifier for RenderNotifier {
    fn clone(&self) -> Box<dyn webrender_api::RenderNotifier> {
        Box::new(Clone::clone(self))
    }

    fn wake_up(&self, _composite_needed: bool) {}

    fn new_frame_ready(
        &self,
        document_id: DocumentId,
        _: FramePublishId,
        frame_ready_params: &FrameReadyParams,
    ) {
        self.paint_proxy.send(PaintMessage::NewWebRenderFrameReady(
            self.painter_id,
            document_id,
            self.generation,
            frame_ready_params.render,
        ));
    }
}
