// SPDX-License-Identifier: MPL-2.0
//! Validate ordinary swap-chain operations on a private offscreen EGL context.
use euclid::default::Size2D;
use glow::{HasContext, NativeFramebuffer};
use surfman::chains::SwapChain;
use surfman::mesa_surfaceless::{connection::Connection, context::Context, device::Device};
use surfman::{ContextAttributeFlags, ContextAttributes, GLVersion, SurfaceAccess, SurfaceType};

#[derive(Debug, PartialEq)]
struct State {
    read: Option<NativeFramebuffer>,
    draw: Option<NativeFramebuffer>,
    color: [f32; 4],
    depth: f32,
    stencil: i32,
    color_mask: [bool; 4],
    depth_mask: bool,
    front_mask: u32,
    back_mask: u32,
    scissor: bool,
    scissor_box: [i32; 4],
    discard: bool,
}
fn state(gl: &glow::Context) -> State {
    unsafe {
        let mut color = [0.; 4];
        gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut color);
        let mut scissor_box = [0; 4];
        gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor_box);
        State {
            read: gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
            draw: gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
            color,
            depth: gl.get_parameter_f32(glow::DEPTH_CLEAR_VALUE),
            stencil: gl.get_parameter_i32(glow::STENCIL_CLEAR_VALUE),
            color_mask: gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK),
            depth_mask: gl.get_parameter_bool(glow::DEPTH_WRITEMASK),
            front_mask: gl.get_parameter_i64(glow::STENCIL_WRITEMASK) as u32,
            back_mask: gl.get_parameter_i64(glow::STENCIL_BACK_WRITEMASK) as u32,
            scissor: gl.is_enabled(glow::SCISSOR_TEST),
            scissor_box,
            discard: gl.is_enabled(glow::RASTERIZER_DISCARD),
        }
    }
}
fn user_state(gl: &glow::Context, read: NativeFramebuffer, draw: NativeFramebuffer) {
    unsafe {
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(read));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(draw));
        gl.clear_color(0.25, 0.5, 0.75, 0.5);
        gl.clear_depth(0.25);
        gl.clear_stencil(123);
        gl.color_mask(true, false, true, false);
        gl.depth_mask(false);
        gl.stencil_mask_separate(glow::FRONT, 0x13579bdf);
        gl.stencil_mask_separate(glow::BACK, 0xa5a55a5a);
        gl.scissor(1, 1, 1, 1);
        gl.enable(glow::SCISSOR_TEST);
        gl.enable(glow::RASTERIZER_DISCARD);
    }
}
fn read_pixel(gl: &glow::Context) -> [u8; 4] {
    unsafe {
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
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        pixel
    }
}
fn current_pixel(gl: &glow::Context, device: &Device, context: &Context) -> [u8; 4] {
    let fbo = device
        .context_surface_info(context)
        .unwrap()
        .unwrap()
        .framebuffer_object;
    unsafe {
        let old = gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, fbo);
        let pixel = read_pixel(gl);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, old);
        pixel
    }
}
fn detached_pixel(
    gl: &glow::Context,
    device: &Device,
    context: &mut Context,
    chain: &SwapChain<Device>,
    reader: NativeFramebuffer,
) -> [u8; 4] {
    let mut texture = Some(chain.take_surface_texture(device, context).unwrap());
    let object = device.surface_texture_object(texture.as_ref().unwrap());
    let pixel = unsafe {
        let old = gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(reader));
        gl.framebuffer_texture_2d(
            glow::READ_FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            device.surface_gl_texture_target(),
            object,
            0,
        );
        assert_eq!(
            gl.check_framebuffer_status(glow::READ_FRAMEBUFFER),
            glow::FRAMEBUFFER_COMPLETE
        );
        let pixel = read_pixel(gl);
        gl.framebuffer_texture_2d(
            glow::READ_FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            device.surface_gl_texture_target(),
            None,
            0,
        );
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, old);
        pixel
    };
    chain
        .recycle_surface_texture(device, context, &mut texture)
        .unwrap();
    assert!(texture.is_none());
    pixel
}
fn main() {
    let software = std::env::args().any(|a| a == "--software");
    let connection = Connection::new().unwrap();
    let adapter = if software {
        connection.create_software_adapter().unwrap()
    } else {
        connection.create_low_power_adapter().unwrap()
    };
    let device = connection.create_device(&adapter).unwrap();
    let descriptor = device
        .create_context_descriptor(&ContextAttributes {
            version: GLVersion::new(3, 3),
            flags: ContextAttributeFlags::ALPHA
                | ContextAttributeFlags::DEPTH
                | ContextAttributeFlags::STENCIL,
        })
        .unwrap();
    let mut context = device.create_context(&descriptor, None).unwrap();
    let surface = device
        .create_surface(
            &context,
            SurfaceAccess::GPUOnly,
            SurfaceType::Generic {
                size: Size2D::new(32, 24),
            },
        )
        .unwrap();
    device
        .bind_surface_to_context(&mut context, surface)
        .unwrap();
    device.make_context_current(&context).unwrap();
    let gl = unsafe {
        glow::Context::try_from_loader_function(|name| device.get_proc_address(&context, name))
            .unwrap()
    };
    let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
    let mut checks = Vec::new();
    let read = unsafe { gl.create_framebuffer().unwrap() };
    let draw = unsafe { gl.create_framebuffer().unwrap() };
    let attached =
        SwapChain::create_attached(&device, &mut context, SurfaceAccess::GPUOnly).unwrap();
    let original = device.context_surface_info(&context).unwrap().unwrap().id;
    user_state(&gl, read, draw);
    let saved = state(&gl);
    assert_eq!(saved.front_mask, 0x13579bdf);
    assert_eq!(saved.back_mask, 0xa5a55a5a);
    attached
        .clear_surface(&device, &mut context, &gl, [0., 1., 0., 1.])
        .unwrap();
    assert_eq!(state(&gl), saved);
    checks.push("attached clear restores distinct native GL framebuffer and write state");
    assert_eq!(current_pixel(&gl, &device, &context), [0, 255, 0, 255]);
    checks.push("attached clear writes the requested RGBA pixel outside the saved scissor");
    let detached = SwapChain::create_detached(
        &device,
        &mut context,
        SurfaceAccess::GPUOnly,
        Size2D::new(16, 16),
    )
    .unwrap();
    user_state(&gl, read, draw);
    let saved = state(&gl);
    assert_eq!(saved.front_mask, 0x13579bdf);
    assert_eq!(saved.back_mask, 0xa5a55a5a);
    detached
        .clear_surface(&device, &mut context, &gl, [0., 0., 1., 1.])
        .unwrap();
    assert_eq!(state(&gl), saved);
    checks.push("detached clear restores native GL state including separate stencil masks");
    assert_eq!(
        device.context_surface_info(&context).unwrap().unwrap().id,
        original
    );
    checks.push("detached clear restores the original native surface attachment");
    assert_eq!(current_pixel(&gl, &device, &context), [0, 255, 0, 255]);
    checks.push("detached clear leaves the original surface pixels intact");
    assert_eq!(
        detached_pixel(&gl, &device, &mut context, &detached, read),
        [0, 0, 255, 255]
    );
    checks.push("detached clear writes the requested RGBA pixel");
    detached
        .take_attachment_from(&device, &mut context, &attached)
        .unwrap();
    assert!(detached.is_attached() && !attached.is_attached());
    assert_eq!(current_pixel(&gl, &device, &context), [0, 0, 255, 255]);
    checks.push("native attachment transfer preserves the destination surface contents");
    attached
        .take_attachment_from(&device, &mut context, &detached)
        .unwrap();
    assert_eq!(current_pixel(&gl, &device, &context), [0, 255, 0, 255]);
    checks.push("native reverse attachment transfer restores the original pixels");
    detached
        .resize(&device, &mut context, Size2D::new(8, 6))
        .unwrap();
    assert_eq!(detached.size(), Size2D::new(8, 6));
    checks.push("detached resize commits native buffer dimensions");
    attached.destroy(&device, &mut context).unwrap();
    assert!(device.context_surface_info(&context).unwrap().is_none());
    user_state(&gl, read, draw);
    let saved = state(&gl);
    assert_eq!(saved.front_mask, 0x13579bdf);
    assert_eq!(saved.back_mask, 0xa5a55a5a);
    detached
        .clear_surface(&device, &mut context, &gl, [1., 1., 0., 1.])
        .unwrap();
    assert_eq!(state(&gl), saved);
    assert!(device.context_surface_info(&context).unwrap().is_none());
    checks.push(
        "detached clear with no previous surface restores native attachment absence and GL state",
    );
    assert_eq!(
        detached_pixel(&gl, &device, &mut context, &detached, read),
        [255, 255, 0, 255]
    );
    checks
        .push("producer-owned native texture borrowing and recycling preserves the cleared pixels");
    detached.destroy(&device, &mut context).unwrap();
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.delete_framebuffer(read);
        gl.delete_framebuffer(draw);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
    }
    device.destroy_context(&mut context).unwrap();
    checks.push("all private native surfaces and context retire cleanly");
    println!(
        "{}",
        serde_json::json!({"passed":true,"renderer":renderer,"software_requested":software,"checks":checks,"hardware_reset_requested":false,"scope":"Healthy private native GL state, pixels and attachment operations; transient failures are exercised separately by in-memory ownership tests"})
    );
}
