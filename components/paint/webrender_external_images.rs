/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::rc::Rc;

use euclid::default::Size2D;
use log::debug;
use paint_api::rendering_context::RenderingContext;
use paint_api::{ExternalImageSource, WebRenderExternalImageApi};
use rustc_hash::FxHashMap;
use servo_canvas_traits::webgl::{WebGLContextId, WebGLThreads};
use surfman::chains::{SwapChain, SwapChainAPI, SwapChains, SwapChainsAPI};
use surfman::{Device, SurfaceTexture};
use webgl::webgl_thread::WebGLContextBusyMap;

#[path = "imported_surface.rs"]
mod imported_surface;
use imported_surface::ImportedSurface;
struct LockedFrontBuffer {
    loan: ImportedSurface<SurfaceTexture, SwapChain<Device>>,
    texture_id: u32,
    size: Size2D<i32>,
}

// This consumer-local task keeps the native context, texture and producer busy
// mark together until export succeeds, even after the renderer has gone away.
struct RetiringImage {
    id: WebGLContextId,
    record: LockedFrontBuffer,
    context: Rc<dyn RenderingContext>,
    threads: WebGLThreads,
    busy: WebGLContextBusyMap,
    finish_reads: bool,
}
impl RetiringImage {
    fn poll(&mut self) -> bool {
        if self.finish_reads {
            if !self.context.native_context_is_lost() {
                if self.context.make_current().is_err() {
                    return false;
                }
                if !self.context.native_context_is_lost() {
                    self.context.gleam_gl_api().finish();
                }
            }
            self.finish_reads = false;
        }
        match self.record.loan.release(
            |texture| self.context.destroy_texture(texture),
            |chain, surface| chain.recycle_surface(surface),
        ) {
            Ok(true) => {
                finish_busy(&self.busy, &self.threads, self.id);
                true
            },
            _ => false,
        }
    }
}
fn finish_busy(busy: &WebGLContextBusyMap, threads: &WebGLThreads, id: WebGLContextId) {
    let mut map = busy.write();
    if let Some(count) = map.get_mut(&id) {
        debug_assert!(*count > 0);
        *count = count.saturating_sub(1);
    }
    drop(map);
    let _ = threads.finished_rendering_to_context(id);
}

/// Bridge between the webrender::ExternalImage callbacks and the WebGLThreads.
pub struct WebGLExternalImages {
    cleanup_scope: servo_base::deferred_cleanup::CleanupScope,
    webgl_threads: WebGLThreads,
    rendering_context: Rc<dyn RenderingContext>,
    swap_chains: SwapChains<WebGLContextId, Device>,
    busy_webgl_context_map: WebGLContextBusyMap,
    locked_front_buffers: FxHashMap<WebGLContextId, LockedFrontBuffer>,
}

impl WebGLExternalImages {
    pub fn new(
        webgl_threads: WebGLThreads,
        rendering_context: Rc<dyn RenderingContext>,
        swap_chains: SwapChains<WebGLContextId, Device>,
        busy_webgl_context_map: WebGLContextBusyMap,
        cleanup_scope: servo_base::deferred_cleanup::CleanupScope,
    ) -> Self {
        Self {
            cleanup_scope,
            webgl_threads,
            rendering_context,
            swap_chains,
            busy_webgl_context_map,
            locked_front_buffers: FxHashMap::default(),
        }
    }

    fn finish_busy(&self, id: WebGLContextId) {
        finish_busy(&self.busy_webgl_context_map, &self.webgl_threads, id);
    }
    fn retire_record(&self, id: WebGLContextId, record: LockedFrontBuffer, finish_reads: bool) {
        self.cleanup_scope.run(|| {
            let mut task = RetiringImage {
                id,
                record,
                context: self.rendering_context.clone(),
                threads: self.webgl_threads.clone(),
                busy: self.busy_webgl_context_map.clone(),
                finish_reads,
            };
            let cleanup = move || task.poll();
            if let Some(scope) = self.rendering_context.cleanup_scope() {
                scope.run(|| servo_base::deferred_cleanup::retire(cleanup));
            } else {
                servo_base::deferred_cleanup::retire(cleanup);
            }
        });
    }

    fn lock_swap_chain(&mut self, id: WebGLContextId) -> Option<(u32, Size2D<i32>)> {
        if let Some(record) = self.locked_front_buffers.get_mut(&id) {
            if record.loan.locks == 0 {
                return None;
            }
            record.loan.locks += 1;
            return Some((record.texture_id, record.size));
        }
        *self.busy_webgl_context_map.write().entry(id).or_default() += 1;
        let Some(chain) = self.swap_chains.get(id) else {
            self.finish_busy(id);
            return None;
        };
        let Some(surface) = chain.take_surface() else {
            self.finish_busy(id);
            return None;
        };
        let (texture, texture_id, size) = match self.rendering_context.create_texture(surface) {
            Ok(imported) => imported,
            Err((error, surface)) => {
                debug!("WebGL external image import deferred: {error:?}");
                chain.recycle_surface(surface);
                self.finish_busy(id);
                return None;
            },
        };
        self.locked_front_buffers.insert(
            id,
            LockedFrontBuffer {
                loan: ImportedSurface {
                    chain,
                    texture: Some(texture),
                    locks: 1,
                },
                texture_id,
                size,
            },
        );
        Some((texture_id, size))
    }

    fn unlock_swap_chain(&mut self, id: WebGLContextId) {
        let Some(record) = self.locked_front_buffers.get_mut(&id) else {
            return;
        };
        if record.loan.locks != 0 {
            record.loan.locks -= 1;
        }
        if record.loan.locks == 0 {
            let record = self.locked_front_buffers.remove(&id).unwrap();
            self.retire_record(id, record, false);
        }
    }
}

impl Drop for WebGLExternalImages {
    fn drop(&mut self) {
        // No more callbacks can read these records. Finish outstanding GPU reads
        // before releasing loans whose unlock callbacks did not arrive.
        for (id, mut record) in std::mem::take(&mut self.locked_front_buffers) {
            record.loan.locks = 0;
            self.retire_record(id, record, true);
        }
    }
}

impl WebRenderExternalImageApi for WebGLExternalImages {
    fn lock(&mut self, id: u64) -> (ExternalImageSource<'_>, Size2D<i32>) {
        let (texture_id, size) = self.lock_swap_chain(WebGLContextId(id)).unwrap_or_default();
        (ExternalImageSource::NativeTexture(texture_id), size)
    }

    fn unlock(&mut self, id: u64) {
        self.unlock_swap_chain(WebGLContextId(id));
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "webrender_external_images_tests.rs"]
mod tests;
