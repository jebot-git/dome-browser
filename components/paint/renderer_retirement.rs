// SPDX-License-Identifier: MPL-2.0
use std::rc::Rc;

use paint_api::rendering_context::RenderingContext;

pub(crate) fn retire_renderer(context: Rc<dyn RenderingContext>, renderer: webrender::Renderer) {
    let mut pending = Some(renderer);
    let mut cleanup = move || {
        if !context.native_context_is_lost() && context.make_current().is_err() {
            return false;
        }
        if let Some(renderer) = pending.take() {
            if context.native_context_is_lost() {
                renderer.deinit_after_context_loss();
            } else {
                renderer.deinit();
            }
        }
        true
    };
    if !cleanup() {
        servo_base::deferred_cleanup::defer(cleanup);
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "renderer_retirement_tests.rs"]
mod tests;
