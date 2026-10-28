// SPDX-License-Identifier: MPL-2.0
// Private surfaceless contexts only; no display session or hardware reset.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use dpi::PhysicalSize;
use gleam::gl::{self, Gl};
use image::RgbaImage;
use servo_base::generic_channel::{self, GenericReceiver};
use servo_canvas_traits::webgl::WebGLMsg;
use surfman::chains::PreserveBuffer;
use surfman::{
    Context, ContextAttributeFlags, ContextAttributes, Error, GLVersion, Surface, SurfaceAccess,
    SurfaceType,
};
use webrender_api::units::DeviceIntRect;

use super::*;

struct Consumer {
    device: Rc<Device>,
    context: RefCell<Context>,
    gl: Rc<dyn Gl>,
    fail_exports: Cell<u32>,
    fail_current: Cell<bool>,
    ordinary_gl: Rc<Cell<u32>>,
    destroyed: Rc<Cell<bool>>,
}

impl RenderingContext for Consumer {
    fn read_to_image(&self, _: DeviceIntRect) -> Option<RgbaImage> {
        None
    }
    fn size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(8, 8)
    }
    fn resize(&self, _: PhysicalSize<u32>) {
        unreachable!()
    }
    fn present(&self) {
        unreachable!()
    }
    fn make_current(&self) -> Result<(), Error> {
        assert!(
            !self.native_context_is_lost(),
            "cleanup must not bind a lost context"
        );
        if self.fail_current.replace(false) {
            return Err(Error::Failed);
        }
        self.device.make_context_current(&self.context.borrow())
    }
    fn native_context_is_lost(&self) -> bool {
        self.device.context_is_lost(&self.context.borrow())
    }
    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        assert!(
            !self.native_context_is_lost(),
            "cleanup must not issue GL after loss"
        );
        self.ordinary_gl.set(self.ordinary_gl.get() + 1);
        self.gl.clone()
    }
    fn glow_gl_api(&self) -> Arc<glow::Context> {
        unreachable!()
    }
    fn create_texture(
        &self,
        surface: Surface,
    ) -> Result<(SurfaceTexture, u32, Size2D<i32>), (Error, Surface)> {
        let size = self.device.surface_info(&surface).size;
        let texture = self
            .device
            .create_surface_texture(&mut self.context.borrow_mut(), surface)?;
        let id = self
            .device
            .surface_texture_object(&texture)
            .unwrap()
            .0
            .get();
        Ok((texture, id, size))
    }
    fn destroy_texture(&self, texture: SurfaceTexture) -> Result<Surface, (Error, SurfaceTexture)> {
        let failures = self.fail_exports.get();
        if failures != 0 {
            self.fail_exports.set(failures - 1);
            return Err((Error::Failed, texture));
        }
        self.device
            .destroy_surface_texture(&mut self.context.borrow_mut(), texture)
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        self.device.destroy_context(self.context.get_mut()).unwrap();
        self.destroyed.set(true);
    }
}

struct Fixture {
    device: Rc<Device>,
    producer: Context,
    consumer: Rc<Consumer>,
    chains: SwapChains<WebGLContextId, Device>,
    busy: WebGLContextBusyMap,
    receiver: GenericReceiver<WebGLMsg>,
    handler: WebGLExternalImages,
}

impl Fixture {
    #[expect(unsafe_code)]
    fn new() -> Self {
        let connection = surfman::mesa_surfaceless::connection::Connection::new().unwrap();
        let adapter = if std::env::var_os("DOME_CREATION_SOFTWARE").is_some() {
            connection.create_software_adapter().unwrap()
        } else {
            connection.create_low_power_adapter().unwrap()
        };
        let device = Rc::new(Device::Alternate(
            connection.create_device(&adapter).unwrap(),
        ));
        let descriptor = device
            .create_context_descriptor(&ContextAttributes {
                version: GLVersion::new(3, 2),
                flags: ContextAttributeFlags::ALPHA,
            })
            .unwrap();
        let mut producer = device.create_context(&descriptor, None).unwrap();
        let surface = device
            .create_surface(
                &producer,
                SurfaceAccess::GPUOnly,
                SurfaceType::Generic {
                    size: Size2D::new(8, 8),
                },
            )
            .unwrap();
        device
            .bind_surface_to_context(&mut producer, surface)
            .unwrap();
        let chains: SwapChains<WebGLContextId, Device> = SwapChains::new();
        chains
            .create_attached_swap_chain(ID, &device, &mut producer, SurfaceAccess::GPUOnly)
            .unwrap();
        chains
            .get(ID)
            .unwrap()
            .swap_buffers(&device, &mut producer, PreserveBuffer::No)
            .unwrap();
        let context = device.create_context(&descriptor, None).unwrap();
        device.make_context_current(&context).unwrap();
        let gl = unsafe { gl::GlFns::load_with(|name| device.get_proc_address(&context, name)) };
        println!("DOME_IMAGE_RETIRE_RENDERER={}", gl.get_string(gl::RENDERER));
        let consumer = Rc::new(Consumer {
            device: device.clone(),
            context: RefCell::new(context),
            gl,
            fail_exports: Cell::new(0),
            fail_current: Cell::new(false),
            ordinary_gl: Rc::new(Cell::new(0)),
            destroyed: Rc::new(Cell::new(false)),
        });
        let busy = WebGLContextBusyMap::default();
        let (sender, receiver) = generic_channel::channel().unwrap();
        let handler = WebGLExternalImages::new(
            WebGLThreads(sender),
            consumer.clone(),
            chains.clone(),
            busy.clone(),
            servo_base::deferred_cleanup::CleanupScope::new(),
        );
        Self {
            device,
            producer,
            consumer,
            chains,
            busy,
            receiver,
            handler,
        }
    }
}

const ID: WebGLContextId = WebGLContextId(37);

fn retirement(drop_locked: bool, fail_current: bool, lost: bool) {
    let Fixture {
        device,
        mut producer,
        consumer,
        chains,
        busy,
        receiver,
        mut handler,
    } = Fixture::new();
    // Consumer deliberately uses the default RenderingContext with no scope.
    // The handler must retain its Paint scope even when unlocked before Drop.
    assert!(consumer.cleanup_scope().is_none());
    let owner_scope = handler.cleanup_scope.clone();
    let imported = handler.lock_swap_chain(ID).is_some();
    let nested = handler.lock_swap_chain(ID).is_some();
    handler.unlock_swap_chain(ID);
    let nested_retained =
        busy.read().get(&ID).copied() == Some(1) && !servo_base::deferred_cleanup::pending();
    let weak = Rc::downgrade(&consumer);
    let destroyed = consumer.destroyed.clone();
    let ordinary_gl = consumer.ordinary_gl.clone();
    if lost {
        device
            .mark_context_lost(&mut consumer.context.borrow_mut())
            .unwrap();
    }
    consumer.fail_exports.set(1);
    consumer.fail_current.set(fail_current);
    let tracked_before_drop = if !drop_locked {
        handler.unlock_swap_chain(ID);
        owner_scope.pending()
    } else {
        true
    };
    drop(handler);
    drop(consumer);
    let owner_retained = weak.upgrade().is_some() && !destroyed.get() && owner_scope.pending();
    let busy_retained = busy.read().get(&ID).copied() == Some(1);
    let no_early_notification = receiver.try_recv().is_err();
    let producer_waits =
        chains.destroy(ID, &device, &mut producer).is_err() && chains.get(ID).is_some();
    // A failed make-current attempt precedes the deliberate export failure.
    let mut attempts = 0;
    while owner_scope.pending() && attempts < 8 {
        owner_scope.poll_scheduled();
        attempts += 1;
        if owner_scope.pending() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    let drained = !owner_scope.pending() && !servo_base::deferred_cleanup::pending();
    let released = weak.upgrade().is_none() && destroyed.get();
    let busy_released = busy.read().get(&ID).copied() == Some(0);
    let notified_once = matches!(
        receiver.try_recv(),
        Ok(WebGLMsg::FinishedRenderingToContext(ID))
    ) && receiver.try_recv().is_err();
    let gl_count = ordinary_gl.get();
    chains.destroy(ID, &device, &mut producer).unwrap();
    device.destroy_context(&mut producer).unwrap();
    assert!(imported && nested && nested_retained);
    assert!(
        tracked_before_drop
            && owner_retained
            && busy_retained
            && no_early_notification
            && producer_waits
    );
    assert!(drained && released && busy_released && notified_once);
    assert_eq!(gl_count, u32::from(drop_locked && !lost));
    assert_eq!(attempts, if fail_current { 2 } else { 1 });
}

#[test]
fn failed_unlock_export_retains_native_owner_and_producer_until_idle_retry() {
    retirement(false, false, false);
}
#[test]
fn missing_unlock_finishes_reads_and_retains_owner_until_retry() {
    retirement(true, false, false);
}
#[test]
fn missing_unlock_retries_current_before_export_without_early_release() {
    retirement(true, true, false);
}
#[test]
fn lost_consumer_cleanup_returns_loan_without_rebinding_or_gl() {
    retirement(true, false, true);
}
