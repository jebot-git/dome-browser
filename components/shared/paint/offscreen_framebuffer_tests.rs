// SPDX-License-Identifier: MPL-2.0
//! Private surfaceless GL contexts only. No display session or hardware reset.
#![expect(unsafe_code)]
use super::*;
use crate::rendering_context::{
    NativeContextOwner, OffscreenRenderingContext, SurfmanRenderingContext, WindowRenderingContext,
};
use euclid::default::Rect;
use euclid::{Box2D, Point2D, Size2D};
use std::{
    cell::Cell,
    sync::atomic::{AtomicU64, Ordering},
};
use surfman::{
    Context, ContextAttributeFlags, ContextAttributes, Device, GLApi, GLVersion, SurfaceAccess,
    SurfaceType,
};

struct TestContext {
    inner: Rc<SurfmanRenderingContext>,
    fail: Rc<Cell<bool>>,
    attempts: Rc<Cell<usize>>,
}
impl RenderingContext for TestContext {
    fn read_to_image(&self, rect: DeviceIntRect) -> Option<RgbaImage> {
        self.inner.read_to_image(rect)
    }
    fn size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(16, 16)
    }
    fn resize(&self, _: PhysicalSize<u32>) {
        unreachable!()
    }
    fn present(&self) {
        unreachable!()
    }
    fn make_current(&self) -> Result<(), Error> {
        self.attempts.set(self.attempts.get() + 1);
        if self.fail.get() {
            return Err(Error::Failed);
        }
        self.inner.make_current()
    }
    fn native_context_is_lost(&self) -> bool {
        self.inner.native_context_is_lost()
    }
    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        self.inner.gleam_gl.clone()
    }
    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.inner.glow_gl.clone()
    }
    fn cleanup_scope(&self) -> Option<servo_base::deferred_cleanup::CleanupScope> {
        Some(self.inner.native.cleanup_scope())
    }
}
fn context() -> Rc<TestContext> {
    let connection = surfman::mesa_surfaceless::connection::Connection::new().unwrap();
    let adapter = if std::env::var_os("DOME_CREATION_SOFTWARE").is_some() {
        connection.create_software_adapter().unwrap()
    } else {
        connection.create_low_power_adapter().unwrap()
    };
    let device = Device::Alternate(connection.create_device(&adapter).unwrap());
    let descriptor = device
        .create_context_descriptor(&ContextAttributes {
            version: GLVersion::new(3, 2),
            flags: ContextAttributeFlags::ALPHA,
        })
        .unwrap();
    let native = device.create_context(&descriptor, None).unwrap();
    let inner = SurfmanRenderingContext::finish_initialization(
        NativeContextOwner::new(device, native),
        GLApi::GL,
        None,
        |device, context| {
            unsafe {
                glow::Context::try_from_loader_function(|name| {
                    device.get_proc_address(context, name)
                })
            }
            .map_err(Error::GLContextInitializationFailed)
        },
    )
    .unwrap();
    let surface = inner
        .create_surface(SurfaceType::Generic {
            size: Size2D::new(16, 16),
        })
        .unwrap();
    inner.bind_surface(surface).unwrap();
    inner.make_current().unwrap();
    println!(
        "DOME_OFFSCREEN_RENDERER={}",
        inner.gleam_gl.get_string(gl::RENDERER)
    );
    Rc::new(TestContext {
        inner,
        fail: Rc::new(Cell::new(false)),
        attempts: Rc::new(Cell::new(0)),
    })
}
fn framebuffer(context: &Rc<TestContext>) -> Framebuffer {
    Framebuffer::new(context.clone(), PhysicalSize::new(8, 8)).unwrap()
}
fn clear(target: &Framebuffer, rgba: [f32; 4]) {
    target.bind().unwrap();
    target.gl.color_mask(true, true, true, true);
    target.gl.disable(gl::SCISSOR_TEST);
    target.gl.clear_color(rgba[0], rgba[1], rgba[2], rgba[3]);
    target.gl.clear(gl::COLOR_BUFFER_BIT);
}
fn pixels(target: &Framebuffer, rgba: [u8; 4]) -> bool {
    let rect = Box2D::new(Point2D::new(0, 0), Point2D::new(8, 8));
    target
        .read_to_image(rect)
        .unwrap()
        .pixels()
        .all(|pixel| pixel.0 == rgba)
}
#[test]
fn dropping_offscreen_objects_selects_owner_and_preserves_peer_names() {
    let owner = context();
    let target = framebuffer(&owner);
    let id = target.framebuffer_id;
    let peer = context();
    let peer_target = framebuffer(&peer);
    assert_eq!(
        id, peer_target.framebuffer_id,
        "independent contexts have colliding names"
    );
    clear(&peer_target, [0., 0., 1., 1.]);
    drop(target);
    peer.make_current().unwrap();
    assert!(
        peer.inner
            .gleam_gl
            .is_framebuffer(peer_target.framebuffer_id)
            != 0
    );
    assert!(pixels(&peer_target, [0, 0, 255, 255]));
    owner.make_current().unwrap();
    assert_eq!(owner.inner.gleam_gl.is_framebuffer(id), 0);
    assert_eq!(owner.inner.gleam_gl.get_error(), gl::NO_ERROR);
}
#[test]
fn failed_bind_keeps_last_context_alive_until_scoped_retirement() {
    let owner = context();
    let target = framebuffer(&owner);
    let fail = owner.fail.clone();
    let scope = owner.cleanup_scope().unwrap();
    let weak = Rc::downgrade(&owner);
    let peer = context();
    let peer_target = framebuffer(&peer);
    clear(&peer_target, [0., 0., 1., 1.]);
    fail.set(true);
    drop(target);
    drop(owner);
    let retained = weak.upgrade().is_some() && scope.pending();
    assert!(pixels(&peer_target, [0, 0, 255, 255]));
    fail.set(false);
    assert!(servo_base::deferred_cleanup::poll());
    assert!(retained && weak.upgrade().is_none() && !scope.pending());
    assert!(pixels(&peer_target, [0, 0, 255, 255]));
}
#[test]
fn partial_allocation_rolls_back_names_and_restores_bindings() {
    let owner = context();
    let sentinel = framebuffer(&owner);
    clear(&sentinel, [0., 1., 0., 1.]);
    for failure_step in 1..=4 {
        sentinel.bind().unwrap();
        sentinel
            .gl
            .bind_texture(gl::TEXTURE_2D, sentinel.texture_id);
        sentinel
            .gl
            .bind_renderbuffer(gl::RENDERBUFFER, sentinel.renderbuffer_id);
        let mut allocated = vec![];
        let mut step = 0;
        let result =
            Framebuffer::new_with_probe(owner.clone(), PhysicalSize::new(4, 4), |target| {
                step += 1;
                allocated.push((
                    target.framebuffer_id,
                    target.texture_id,
                    target.renderbuffer_id,
                ));
                if step == failure_step {
                    Err(Error::Failed)
                } else {
                    Ok(())
                }
            });
        assert!(result.is_err());
        assert_eq!(
            integer(&*sentinel.gl, gl::DRAW_FRAMEBUFFER_BINDING) as u32,
            sentinel.framebuffer_id
        );
        assert_eq!(
            integer(&*sentinel.gl, gl::READ_FRAMEBUFFER_BINDING) as u32,
            sentinel.framebuffer_id
        );
        assert_eq!(
            integer(&*sentinel.gl, gl::TEXTURE_BINDING_2D) as u32,
            sentinel.texture_id
        );
        assert_eq!(
            integer(&*sentinel.gl, gl::RENDERBUFFER_BINDING) as u32,
            sentinel.renderbuffer_id
        );
        for (framebuffer, texture, depth) in allocated {
            assert!(framebuffer == 0 || sentinel.gl.is_framebuffer(framebuffer) == 0);
            assert!(texture == 0 || sentinel.gl.is_texture(texture) == 0);
            assert!(depth == 0 || sentinel.gl.is_renderbuffer(depth) == 0);
        }
        assert!(pixels(&sentinel, [0, 255, 0, 255]));
        assert_eq!(sentinel.gl.get_error(), gl::NO_ERROR);
    }
}
#[test]
fn invalid_dimensions_and_failed_current_leave_peer_contents_intact() {
    let owner = context();
    let peer = context();
    let target = framebuffer(&peer);
    clear(&target, [0., 0., 1., 1.]);
    owner.fail.set(true);
    for size in [
        PhysicalSize::new(0, 8),
        PhysicalSize::new(u32::MAX, 8),
        PhysicalSize::new(8, 8),
    ] {
        assert!(Framebuffer::new(owner.clone(), size).is_err());
        assert_eq!(
            integer(&*target.gl, gl::DRAW_FRAMEBUFFER_BINDING) as u32,
            target.framebuffer_id
        );
    }
    owner.fail.set(false);
    assert!(Framebuffer::new(owner, PhysicalSize::new(i32::MAX as u32, 8)).is_err());
    assert!(pixels(&target, [0, 0, 255, 255]));
}
#[test]
fn known_loss_retires_offscreen_without_ordinary_peer_gl_calls() {
    let owner = context();
    let target = framebuffer(&owner);
    let peer = context();
    let peer_target = framebuffer(&peer);
    clear(&peer_target, [0., 0., 1., 1.]);
    owner
        .inner
        .device
        .borrow()
        .mark_context_lost(&mut owner.inner.context.borrow_mut())
        .unwrap();
    let before = owner.attempts.get();
    drop(target);
    assert_eq!(owner.attempts.get(), before);
    drop(owner);
    assert!(pixels(&peer_target, [0, 0, 255, 255]));
}
#[test]
fn retained_scope_suppresses_the_initial_offscreen_cleanup_attempt() {
    let owner = context();
    let target = framebuffer(&owner);
    let scope = owner.cleanup_scope().unwrap();
    let attempts = owner.attempts.clone();
    let before = attempts.get();
    scope.retain_unfinished();
    drop(target);
    drop(owner);
    assert_eq!(attempts.get(), before);
    assert!(scope.pending() && servo_base::deferred_cleanup::poll());
}
fn window() -> Rc<WindowRenderingContext> {
    let native = context();
    Rc::new(WindowRenderingContext {
        size: Cell::new(PhysicalSize::new(16, 16)),
        surfman_context: native.inner.clone(),
        presentation_epoch: Arc::new(AtomicU64::new(0)),
    })
}
#[test]
fn presentation_preserves_source_and_restores_gl_state() {
    let parent = window();
    let child = OffscreenRenderingContext::new(parent.clone(), PhysicalSize::new(8, 8)).unwrap();
    clear(&child.framebuffer.borrow(), [0., 1., 0., 1.]);
    let gl = child.gleam_gl_api();
    gl.clear_color(0.2, 0.3, 0.4, 0.5);
    gl.color_mask(true, false, true, false);
    gl.scissor(1, 2, 3, 4);
    gl.enable(gl::SCISSOR_TEST);
    let bound = integer(&*gl, gl::DRAW_FRAMEBUFFER_BINDING);
    let callback = child.render_to_parent_callback().unwrap();
    callback(
        &child.glow_gl_api(),
        Rect::new(Point2D::zero(), Size2D::new(8, 8)),
    );
    assert_eq!(integer(&*gl, gl::DRAW_FRAMEBUFFER_BINDING), bound);
    assert_ne!(gl.is_enabled(gl::SCISSOR_TEST), 0);
    let mut mask = [0; 4];
    let mut scissor = [0; 4];
    let mut color = [0.; 4];
    unsafe {
        gl.get_boolean_v(gl::COLOR_WRITEMASK, &mut mask);
        gl.get_integer_v(gl::SCISSOR_BOX, &mut scissor);
        gl.get_float_v(gl::COLOR_CLEAR_VALUE, &mut color);
    }
    assert_eq!(mask, [1, 0, 1, 0]);
    assert_eq!(scissor, [1, 2, 3, 4]);
    assert_eq!(color, [0.2, 0.3, 0.4, 0.5]);
    assert!(pixels(&child.framebuffer.borrow(), [0, 255, 0, 255]));
    let rect = Box2D::new(Point2D::zero(), Point2D::new(8, 8));
    assert!(
        parent
            .read_to_image(rect)
            .unwrap()
            .pixels()
            .all(|pixel| pixel.0 == [0, 255, 0, 255])
    );
    assert_eq!(gl.get_error(), gl::NO_ERROR);
}
#[test]
fn failed_resize_preserves_pixels_and_successful_resize_invalidates_old_callback() {
    let parent = window();
    let child = OffscreenRenderingContext::new(parent, PhysicalSize::new(8, 8)).unwrap();
    clear(&child.framebuffer.borrow(), [0., 1., 0., 1.]);
    let old = child.render_to_parent_callback().unwrap();
    child.resize(PhysicalSize::new(0, 8));
    assert_eq!(child.size(), PhysicalSize::new(8, 8));
    assert!(pixels(&child.framebuffer.borrow(), [0, 255, 0, 255]));
    child.resize(PhysicalSize::new(12, 12));
    assert_eq!(child.size(), PhysicalSize::new(12, 12));
    let image = child
        .read_to_image(Box2D::new(Point2D::zero(), Point2D::new(12, 12)))
        .unwrap();
    // Readback is vertically flipped; the preserved GL lower-left square is at the bottom.
    assert_eq!(image.get_pixel(0, 11).0, [0, 255, 0, 255]);
    assert_eq!(image.get_pixel(11, 0).0, [0, 0, 0, 0]);
    let peer = context();
    let target = framebuffer(&peer);
    clear(&target, [0., 0., 1., 1.]);
    old(
        &peer.glow_gl_api(),
        Rect::new(Point2D::zero(), Size2D::new(8, 8)),
    );
    assert_eq!(
        integer(&*target.gl, gl::DRAW_FRAMEBUFFER_BINDING) as u32,
        target.framebuffer_id
    );
    assert!(pixels(&target, [0, 0, 255, 255]));
}
#[test]
fn disposed_child_and_changed_parent_surface_disable_retained_callbacks() {
    let parent = window();
    let weak = Rc::downgrade(&parent);
    let child = OffscreenRenderingContext::new(parent.clone(), PhysicalSize::new(8, 8)).unwrap();
    let old = child.render_to_parent_callback().unwrap();
    parent.presentation_epoch.fetch_add(1, Ordering::AcqRel);
    let peer = context();
    let target = framebuffer(&peer);
    clear(&target, [0., 0., 1., 1.]);
    old(
        &peer.glow_gl_api(),
        Rect::new(Point2D::zero(), Size2D::new(8, 8)),
    );
    assert_eq!(
        integer(&*target.gl, gl::DRAW_FRAMEBUFFER_BINDING) as u32,
        target.framebuffer_id
    );
    let late = child.render_to_parent_callback().unwrap();
    drop(child);
    drop(parent);
    assert!(weak.upgrade().is_none());
    late(
        &peer.glow_gl_api(),
        Rect::new(Point2D::zero(), Size2D::new(8, 8)),
    );
    assert!(pixels(&target, [0, 0, 255, 255]));
}
