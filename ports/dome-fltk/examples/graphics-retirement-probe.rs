// SPDX-License-Identifier: MPL-2.0
//! Private offscreen EGL retirement checks. This does not reset the GPU.
use euclid::default::Size2D;
use glow::HasContext;
use std::ffi::c_void;
use surfman::chains::{PreserveBuffer, SwapChains, SwapChainsAPI};
use surfman::mesa_surfaceless::{
    connection::Connection,
    context::{Context, NativeContext},
    device::Device,
};
use surfman::{
    ContextAttributeFlags as Flags, ContextAttributes, Error, GLVersion, SurfaceAccess,
    SurfaceType, WindowingApiError,
};
#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetCurrentDisplay() -> *mut c_void;
    fn eglTerminate(display: *mut c_void) -> u32;
}
fn context(device: &Device, share: Option<&Context>) -> Context {
    let descriptor = device
        .create_context_descriptor(&ContextAttributes {
            version: GLVersion::new(3, 3),
            flags: Flags::ALPHA | Flags::DEPTH | Flags::STENCIL,
        })
        .unwrap();
    device.create_context(&descriptor, share).unwrap()
}
fn attach(device: &Device, context: &mut Context) {
    let surface = device
        .create_surface(
            context,
            SurfaceAccess::GPUOnly,
            SurfaceType::Generic {
                size: Size2D::new(8, 8),
            },
        )
        .unwrap();
    device.bind_surface_to_context(context, surface).unwrap();
    device.make_context_current(context).unwrap();
}
fn gl(device: &Device, context: &Context) -> glow::Context {
    unsafe { glow::Context::from_loader_function(|name| device.get_proc_address(context, name)) }
}
fn rejected(device: &Device, context: &Context) {
    assert!(matches!(
        device.graphics_reset_status(context),
        Err(Error::MakeCurrentFailed(WindowingApiError::ContextLost))
    ));

    assert!(matches!(
        device.make_context_current(context),
        Err(Error::MakeCurrentFailed(WindowingApiError::ContextLost))
    ));
}
fn report(mode: &str, renderer: &str) {
    println!(
        "{}",
        serde_json::json!({"mode": mode, "renderer": renderer,
        "passed": true, "gpu_reset_requested": false})
    );
}
fn main() {
    let software = std::env::args().any(|arg| arg == "--software");
    let connection = Connection::new().unwrap();
    let adapter = if software {
        connection.create_software_adapter().unwrap()
    } else {
        connection.create_low_power_adapter().unwrap()
    };
    let device = connection.create_device(&adapter).unwrap();
    assert!(device.supports_context_loss_recovery());
    // Retire A while B is current. Cleanup must neither bind A nor alter B's
    // framebuffer, and must release A even though normal binding is rejected.
    let mut a = context(&device, None);
    attach(&device, &mut a);
    let mut b = context(&device, None);
    attach(&device, &mut b);
    let gl = gl(&device, &b);
    let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
    let current = NativeContext::current().unwrap().egl_context;
    let framebuffer = unsafe { gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING) };
    device.mark_context_lost(&mut a).unwrap();
    rejected(&device, &a);
    let mut surface = device.unbind_surface_from_context(&mut a).unwrap().unwrap();
    device.destroy_surface(&mut a, &mut surface).unwrap();
    device.destroy_context(&mut a).unwrap();
    assert_eq!(NativeContext::current().unwrap().egl_context, current);
    unsafe {
        assert_eq!(
            gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
            framebuffer
        );
        gl.clear_color(0.0, 1.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let mut pixel = [0; 4];
        gl.read_pixels(
            0,
            0,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixel)),
        );
        assert_eq!(pixel, [0, 255, 0, 255]);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
    }
    device.destroy_context(&mut b).unwrap();
    report("retirement-preserves-other-context", &renderer);
    // Loss is shared by explicitly sharing peers; no new peer may be created.
    let mut a = context(&device, None);
    let mut b = context(&device, Some(&a));
    let descriptor = device.context_descriptor(&a);
    device.mark_context_lost(&mut a).unwrap();
    rejected(&device, &a);
    rejected(&device, &b);
    assert!(device.create_context(&descriptor, Some(&a)).is_err());
    device.destroy_context(&mut a).unwrap();
    device.destroy_context(&mut b).unwrap();
    report("owned-share-group-retirement", &renderer);
    // A retired consumer must return its imported surface without requiring GL
    // deletion. The original producer still owns, and can release, that surface.
    let mut a = context(&device, None);
    attach(&device, &mut a);
    let surface = device.unbind_surface_from_context(&mut a).unwrap().unwrap();
    let mut b = context(&device, None);
    let imported = device.create_surface_texture(&mut b, surface).unwrap();
    device.mark_context_lost(&mut b).unwrap();
    let mut surface = device.destroy_surface_texture(&mut b, imported).unwrap();
    device.make_context_current(&a).unwrap();
    device.destroy_surface(&mut a, &mut surface).unwrap();
    device.destroy_context(&mut b).unwrap();
    device.destroy_context(&mut a).unwrap();
    report("retired-reader-returns-producer-surface", &renderer);
    // A failed destruction must retain its registry entry and every surface.
    // Exercise attached, pending/recycled and detached surfaces before loss.
    let mut a = context(&device, None);
    attach(&device, &mut a);
    let chains = SwapChains::<u8, Device>::new();
    chains
        .create_attached_swap_chain(1, &device, &mut a, SurfaceAccess::GPUOnly)
        .unwrap();
    let chain = chains.get(1).unwrap();
    for _ in 0..3 {
        chain
            .swap_buffers(&device, &mut a, PreserveBuffer::No)
            .unwrap();
    }
    chains
        .create_detached_swap_chain(
            2,
            Size2D::new(8, 8),
            &device,
            &mut a,
            SurfaceAccess::GPUOnly,
        )
        .unwrap();
    let mut b = context(&device, None);
    attach(&device, &mut b);
    let current = NativeContext::current().unwrap().egl_context;
    assert!(matches!(
        chains.destroy(1, &device, &mut b),
        Err(Error::IncompatibleContext)
    ));
    assert!(chains.get(1).is_some());
    assert_eq!(chains.iter(&device, &mut a).count(), 2);
    device.mark_context_lost(&mut a).unwrap();
    chains.destroy(1, &device, &mut a).unwrap();
    chains.destroy(2, &device, &mut a).unwrap();
    assert!(chains.get(1).is_none() && chains.get(2).is_none());
    assert_eq!(chains.iter(&device, &mut a).count(), 0);
    // Repeated destruction is harmless after the owned resources are released.
    chains.destroy(1, &device, &mut a).unwrap();
    assert_eq!(NativeContext::current().unwrap().egl_context, current);
    device.destroy_context(&mut a).unwrap();
    device.destroy_context(&mut b).unwrap();
    report("swap-chain-retirement-retains-failed-owner", &renderer);
    // Invalidate this process's private EGL display, not the GPU or desktop.
    // Existing handles are now deleted/invalid; retirement must consume them
    // without GL work or destructor assertions.
    let mut a = context(&device, None);
    attach(&device, &mut a);
    let display = unsafe { eglGetCurrentDisplay() };
    assert!(!display.is_null());
    device.mark_context_lost(&mut a).unwrap();
    assert_ne!(unsafe { eglTerminate(display) }, 0);
    let mut surface = device.unbind_surface_from_context(&mut a).unwrap().unwrap();
    device.destroy_surface(&mut a, &mut surface).unwrap();
    device.destroy_context(&mut a).unwrap();
    report("private-display-invalidation-retirement", &renderer);
}
