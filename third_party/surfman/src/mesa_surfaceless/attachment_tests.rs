// SPDX-License-Identifier: MPL-2.0
// Every context/surface here belongs to this offscreen test process.
use super::{Connection, Context, Device, Surface};
use crate::base::egl::device::EGL_FUNCTIONS;
use crate::{
    ContextAttributeFlags, ContextAttributes, Error, GLVersion, SurfaceAccess, SurfaceType,
    WindowingApiError, egl,
};
use euclid::default::Size2D;
use glow::HasContext;
fn device() -> Device {
    let connection = Connection::new().unwrap();
    let adapter = if std::env::var_os("DOME_NATIVE_INIT_SOFTWARE").is_some() {
        connection.create_software_adapter().unwrap()
    } else {
        connection.create_low_power_adapter().unwrap()
    };
    connection.create_device(&adapter).unwrap()
}
fn context(device: &Device) -> Context {
    let descriptor = device
        .create_context_descriptor(&ContextAttributes {
            version: GLVersion::new(3, 3),
            flags: ContextAttributeFlags::ALPHA,
        })
        .unwrap();
    device.create_context(&descriptor, None).unwrap()
}
fn surface(device: &Device, context: &Context) -> Surface {
    device
        .create_surface(
            context,
            SurfaceAccess::GPUOnly,
            SurfaceType::Generic {
                size: Size2D::new(8, 8),
            },
        )
        .unwrap()
}
fn bind(device: &Device, context: &mut Context) {
    let surface = surface(device, context);
    device.bind_surface_to_context(context, surface).unwrap();
    device.make_context_current(context).unwrap();
    unsafe {
        println!(
            "DOME_ATTACHMENT_RENDERER={}",
            context.1.get_parameter_string(glow::RENDERER)
        );
    }
}
fn current() -> usize {
    EGL_FUNCTIONS.with(|egl| unsafe { egl.GetCurrentContext() as usize })
}
fn lost_attachment(current_target: bool) {
    let device = device();
    let mut target = context(&device);
    let surface = surface(&device, &target);
    let mut other = context(&device);
    if current_target {
        device.make_context_current(&target).unwrap();
    } else {
        bind(&device, &mut other);
    }
    let before = current();
    device.mark_context_lost(&mut target).unwrap();
    let mut correct_error = false;
    let mut returned = false;
    if let Err((error, mut surface)) = device.bind_surface_to_context(&mut target, surface) {
        correct_error = matches!(
            error,
            Error::MakeCurrentFailed(WindowingApiError::ContextLost)
        );
        returned = true;
        device.destroy_surface(&mut target, &mut surface).unwrap();
    }
    let absent = device.context_surface_info(&target).unwrap().is_none();
    let binding_unchanged = current() == before;
    device.destroy_context(&mut target).unwrap();
    device.destroy_context(&mut other).unwrap();
    assert!(
        correct_error && returned && absent && binding_unchanged,
        "lost bind: error={correct_error} returned={returned} absent={absent} binding_unchanged={binding_unchanged}"
    );
}
#[test]
fn attachment_lost_current_context_returns_surface() {
    lost_attachment(true);
}
#[test]
fn attachment_lost_noncurrent_context_returns_surface() {
    lost_attachment(false);
}
#[test]
fn attachment_failed_rebind_returns_surface_and_preserves_binding() {
    let device = device();
    let mut target = context(&device);
    let surface = surface(&device, &target);
    device.make_context_current(&target).unwrap();
    let before = current();
    // Invalid display is used only for this one attempted binding. The owning
    // connection and native handles stay valid for deterministic cleanup.
    let result = unsafe { target.0.bind_surface(egl::NO_DISPLAY, surface.0) };
    let returned = result.is_err();
    if let Err((_, surface)) = result {
        device
            .destroy_surface(&mut target, &mut Surface(surface))
            .unwrap();
    }
    let absent = device.context_surface_info(&target).unwrap().is_none();
    let binding_unchanged = current() == before;
    device.destroy_context(&mut target).unwrap();
    assert!(
        returned && absent && binding_unchanged,
        "failed bind: returned={returned} absent={absent} binding_unchanged={binding_unchanged}"
    );
}
fn failed_unbind(noncurrent: bool) {
    let device = device();
    let mut target = context(&device);
    bind(&device, &mut target);
    let original_id = device.context_surface_info(&target).unwrap().unwrap().id;
    let mut other = context(&device);
    if noncurrent {
        bind(&device, &mut other);
    } else {
        device.make_context_current(&target).unwrap();
    }
    let before = current();
    let result = unsafe { target.0.unbind_surface(&target.1, egl::NO_DISPLAY) };
    let rejected = result.is_err();
    if let Ok(Some(surface)) = result {
        device.make_context_current(&target).unwrap();
        device
            .destroy_surface(&mut target, &mut Surface(surface))
            .unwrap();
        if noncurrent {
            device.make_context_current(&other).unwrap();
        }
    }
    let retained = device
        .context_surface_info(&target)
        .unwrap()
        .is_some_and(|s| s.id == original_id);
    let binding_unchanged = current() == before;
    device.destroy_context(&mut target).unwrap();
    device.destroy_context(&mut other).unwrap();
    assert!(
        rejected && retained && binding_unchanged,
        "failed unbind: error={rejected} retained={retained} binding_unchanged={binding_unchanged}"
    );
}
#[test]
fn attachment_failed_current_unbind_retains_surface() {
    failed_unbind(false);
}
#[test]
fn attachment_failed_noncurrent_unbind_retains_surface() {
    failed_unbind(true);
}

#[test]
fn attachment_noncurrent_unbind_preserves_other_context_and_surface_owner() {
    let device = device();
    let mut target = context(&device);
    bind(&device, &mut target);
    let id = device.context_surface_info(&target).unwrap().unwrap().id;
    let mut other = context(&device);
    bind(&device, &mut other);
    let before = current();
    let fbo = unsafe {
        other
            .1
            .get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING)
    };
    let mut surface = device
        .unbind_surface_from_context(&mut target)
        .unwrap()
        .unwrap();
    let preserved = current() == before
        && unsafe {
            other
                .1
                .get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING)
                == fbo
                && other.1.get_error() == glow::NO_ERROR
        };
    let owned = device.surface_info(&surface).id == id
        && device.context_surface_info(&target).unwrap().is_none();
    device.make_context_current(&target).unwrap();
    device.destroy_surface(&mut target, &mut surface).unwrap();
    device.destroy_context(&mut target).unwrap();
    device.destroy_context(&mut other).unwrap();
    assert!(preserved && owned);
}
#[test]
fn attachment_lost_noncurrent_unbind_returns_owner_without_touching_other_context() {
    let device = device();
    let mut target = context(&device);
    bind(&device, &mut target);
    let id = device.context_surface_info(&target).unwrap().unwrap().id;
    let mut other = context(&device);
    bind(&device, &mut other);
    let before = current();
    let fbo = unsafe {
        other
            .1
            .get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING)
    };
    device.mark_context_lost(&mut target).unwrap();
    let mut surface = device
        .unbind_surface_from_context(&mut target)
        .unwrap()
        .unwrap();
    let preserved = current() == before
        && unsafe {
            other
                .1
                .get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING)
                == fbo
                && other.1.get_error() == glow::NO_ERROR
        };
    let owned = device.surface_info(&surface).id == id
        && device.context_surface_info(&target).unwrap().is_none();
    device.destroy_surface(&mut target, &mut surface).unwrap();
    device.destroy_context(&mut target).unwrap();
    device.destroy_context(&mut other).unwrap();
    assert!(preserved && owned);
}
#[test]
fn attachment_failed_binding_can_retry_with_same_surface() {
    let device = device();
    let mut target = context(&device);
    let surface = surface(&device, &target);
    let id = device.surface_info(&surface).id;
    device.make_context_current(&target).unwrap();
    let result = unsafe { target.0.bind_surface(egl::NO_DISPLAY, surface.0) };
    let failed = result.is_err();
    if let Err((_, surface)) = result {
        device
            .bind_surface_to_context(&mut target, Surface(surface))
            .unwrap();
    }
    let retained = device
        .context_surface_info(&target)
        .unwrap()
        .is_some_and(|surface| surface.id == id);
    let mut other = context(&device);
    bind(&device, &mut other);
    let before = current();
    let surface = device
        .unbind_surface_from_context(&mut target)
        .unwrap()
        .unwrap();
    device
        .bind_surface_to_context(&mut target, surface)
        .unwrap();
    let unchanged = current() == before;
    device.destroy_context(&mut target).unwrap();
    device.destroy_context(&mut other).unwrap();
    assert!(failed && retained && unchanged);
}

#[test]
fn attachment_noncurrent_context_destruction_preserves_peer_framebuffer() {
    let device = device();
    let mut target = context(&device);
    bind(&device, &mut target);
    let mut other = context(&device);
    bind(&device, &mut other);
    let before = current();
    let framebuffer = unsafe {
        other
            .1
            .get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING)
    };
    unsafe {
        other.1.clear_color(0., 1., 0., 1.);
        other.1.clear(glow::COLOR_BUFFER_BIT);
    }
    device.destroy_context(&mut target).unwrap();
    let unchanged = current() == before
        && unsafe {
            other
                .1
                .get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING)
                == framebuffer
        };
    let mut pixel = [0; 4];
    if unchanged {
        unsafe {
            other.1.read_pixels(
                0,
                0,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixel)),
            );
        }
    }
    device.destroy_context(&mut other).unwrap();
    assert!(
        unchanged,
        "destroying a noncurrent context changed its peer's framebuffer"
    );
    assert_eq!(pixel, [0, 255, 0, 255]);
}

#[test]
fn attachment_noncurrent_surface_destruction_preserves_peer_framebuffer() {
    let device = device();
    let mut target = context(&device);
    let mut surface = surface(&device, &target);
    let mut other = context(&device);
    bind(&device, &mut other);
    let before = current();
    let framebuffer = unsafe {
        other
            .1
            .get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING)
    };
    unsafe {
        other.1.clear_color(0., 1., 0., 1.);
        other.1.clear(glow::COLOR_BUFFER_BIT);
    }
    device.destroy_surface(&mut target, &mut surface).unwrap();
    let unchanged = current() == before
        && unsafe {
            other
                .1
                .get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING)
                == framebuffer
        };
    let mut pixel = [0; 4];
    if unchanged {
        unsafe {
            other.1.read_pixels(
                0,
                0,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixel)),
            );
        }
    }
    device.destroy_context(&mut target).unwrap();
    device.destroy_context(&mut other).unwrap();
    assert!(
        unchanged,
        "destroying a noncurrent surface changed its peer's framebuffer"
    );
    assert_eq!(pixel, [0, 255, 0, 255]);
}

#[test]
fn attachment_consumer_owner_transfers_pixels_and_defers_producer_retirement() {
    use crate::chains::{PreserveBuffer, SurfaceConsumer, SwapChain};
    let device = device();
    let mut producer = context(&device);
    bind(&device, &mut producer);
    unsafe {
        producer.1.clear_color(0., 1., 0., 1.);
        producer.1.clear(glow::COLOR_BUFFER_BIT);
    }
    let chain = SwapChain::create_attached(&device, &mut producer, SurfaceAccess::GPUOnly).unwrap();
    chain
        .swap_buffers(&device, &mut producer, PreserveBuffer::No)
        .unwrap();
    let mut consumer = context(&device);
    let mut loan = SurfaceConsumer::acquire(chain.clone(), &device, &mut consumer)
        .unwrap()
        .unwrap();
    let size = loan.size();
    let texture = device.surface_texture_object(loan.texture().unwrap());
    let mut pixel = [0; 4];
    unsafe {
        let framebuffer = consumer.1.create_framebuffer().unwrap();
        consumer
            .1
            .bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        consumer.1.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            texture,
            0,
        );
        consumer.1.read_pixels(
            0,
            0,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixel)),
        );
        consumer.1.delete_framebuffer(framebuffer);
    }
    let deferred = chain.destroy(&device, &mut producer).is_err();
    loan.release(&device, &mut consumer).unwrap();
    let released = loan.texture().is_none();
    chain.destroy(&device, &mut producer).unwrap();
    device.destroy_context(&mut producer).unwrap();
    device.destroy_context(&mut consumer).unwrap();
    assert!(deferred && released);
    assert_eq!(size, Size2D::new(8, 8));
    assert_eq!(pixel, [0, 255, 0, 255]);
}
