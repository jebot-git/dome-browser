// SPDX-License-Identifier: MPL-2.0
//! Independent, offscreen Surfman context creation/rendering probe. This does not
//! reset the GPU or exercise browser context-loss recovery.
use euclid::default::Size2D;
use glow::HasContext;
use surfman::mesa_surfaceless::connection::Connection;
use surfman::mesa_surfaceless::context::{Context, NativeContext};
use surfman::mesa_surfaceless::device::Device;
use surfman::{
    ContextAttributeFlags as Flags, ContextAttributes, GLVersion, SurfaceAccess, SurfaceType,
};

fn check_context(device: &Device, context: &mut Context, flags: Flags) -> String {
    device.make_context_current(context).unwrap();
    let gl = unsafe {
        glow::Context::from_loader_function(|name| device.get_proc_address(context, name))
    };
    let renderer;
    unsafe {
        renderer = gl.get_parameter_string(glow::RENDERER);
        let v = gl.version();
        assert!(
            (v.major, v.minor) >= (4, 5)
                || gl.supported_extensions().contains("GL_KHR_robustness")
                || gl.supported_extensions().contains("GL_ARB_robustness")
        );
        let strategy = gl.get_parameter_i32(0x8256);
        assert_eq!(
            strategy,
            if flags.contains(Flags::RESET_NOTIFICATION) {
                0x8252
            } else {
                0x8261
            }
        );
        assert_eq!(
            gl.get_parameter_i32(0x90f3) != 0,
            flags.contains(Flags::ROBUST_ACCESS)
        );
        assert_eq!(gl.get_error(), glow::NO_ERROR);
    }
    assert_eq!(
        device.graphics_reset_status(context).unwrap(),
        if flags.contains(Flags::RESET_NOTIFICATION) {
            Some(surfman::GraphicsResetStatus::NoError)
        } else {
            None
        }
    );
    let actual = device.context_descriptor(context);
    let actual = device.context_descriptor_attributes(&actual);
    let mask = Flags::RESET_NOTIFICATION | Flags::ROBUST_ACCESS;
    assert_eq!(actual.flags & mask, flags & mask);
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
    unsafe {
        let info = device.context_surface_info(context).unwrap().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, info.framebuffer_object);
        gl.viewport(0, 0, 8, 8);
        gl.clear_color(0.25, 0.5, 0.75, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let mut pixel = [0_u8; 4];
        gl.read_pixels(
            0,
            0,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixel)),
        );
        for (actual, expected) in pixel.into_iter().zip([64_i32, 128, 191, 255]) {
            assert!((i32::from(actual) - expected).abs() <= 1);
        }
        assert_eq!(gl.get_error(), glow::NO_ERROR);
    }
    renderer
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
    for (name, flags) in [
        ("default", Flags::empty()),
        ("notified", Flags::RESET_NOTIFICATION),
        ("robust", Flags::ROBUST_ACCESS),
        (
            "robust-notified",
            Flags::RESET_NOTIFICATION | Flags::ROBUST_ACCESS,
        ),
    ] {
        let flags = flags | Flags::ALPHA;
        let descriptor = device
            .create_context_descriptor(&ContextAttributes {
                version: GLVersion::new(3, 3),
                flags,
            })
            .unwrap();
        let mut context = device.create_context(&descriptor, None).unwrap();
        let renderer = check_context(&device, &mut context, flags);
        // Query and borrow the target while a different context is current.
        // Native wrapping must restore the caller's binding as well.
        let mask = Flags::RESET_NOTIFICATION | Flags::ROBUST_ACCESS;
        let other_descriptor = device
            .create_context_descriptor(&ContextAttributes {
                version: GLVersion::new(3, 3),
                flags: flags ^ mask,
            })
            .unwrap();
        let mut other = device.create_context(&other_descriptor, None).unwrap();
        let previous = NativeContext::current().unwrap().egl_context;
        assert_eq!(
            device.graphics_reset_status(&context).unwrap(),
            if flags.contains(Flags::RESET_NOTIFICATION) {
                Some(surfman::GraphicsResetStatus::NoError)
            } else {
                None
            }
        );
        assert_eq!(NativeContext::current().unwrap().egl_context, previous);

        let actual_flags = |context: &Context| {
            device
                .context_descriptor_attributes(&device.context_descriptor(context))
                .flags
                & mask
        };
        assert_eq!(actual_flags(&context), flags & mask);
        assert_eq!(NativeContext::current().unwrap().egl_context, previous);
        let mut borrowed = unsafe {
            device
                .create_context_from_native_context(device.native_context(&context))
                .unwrap()
        };
        assert_eq!(actual_flags(&borrowed), flags & mask);
        assert_eq!(NativeContext::current().unwrap().egl_context, previous);
        device.destroy_context(&mut borrowed).unwrap();
        device.make_no_context_current().unwrap();
        assert_eq!(actual_flags(&context), flags & mask);
        let mut borrowed = unsafe {
            device
                .create_context_from_native_context(device.native_context(&context))
                .unwrap()
        };
        assert_eq!(actual_flags(&borrowed), flags & mask);
        let restored_none = matches!(
            NativeContext::current(),
            Err(surfman::Error::NoCurrentContext)
        );
        device.destroy_context(&mut borrowed).unwrap();
        device.destroy_context(&mut other).unwrap();
        if !restored_none {
            device.destroy_context(&mut context).unwrap();
        }
        assert!(
            restored_none,
            "native wrapping must restore the original absent context"
        );

        // The descriptor read from the live GL context must retain its actual
        // notification strategy when creating another context in the group.
        let actual = device.context_descriptor(&context);
        let mut shared = device.create_context(&actual, Some(&context)).unwrap();
        assert_eq!(check_context(&device, &mut shared, flags), renderer);
        device.destroy_context(&mut shared).unwrap();
        // EGL must reject sharing between different notification strategies.
        let incompatible = device
            .create_context_descriptor(&ContextAttributes {
                version: GLVersion::new(3, 3),
                flags: flags ^ Flags::RESET_NOTIFICATION,
            })
            .unwrap();
        match device.create_context(&incompatible, Some(&context)) {
            Err(surfman::Error::ContextCreationFailed(surfman::WindowingApiError::BadMatch)) => (),
            Err(error) => panic!("unexpected sharing error: {error:?}"),
            Ok(mut unexpected) => {
                device.destroy_context(&mut unexpected).unwrap();
                panic!("incompatible strategies shared");
            },
        }
        device.destroy_context(&mut context).unwrap();
        println!(
            "{}",
            serde_json::json!({
                "mode": name, "renderer": renderer, "software_requested": software,
                "actual_strategy_verified": true, "actual_robust_access_verified": true,
            "reset_query_verified": true,
                "descriptor_roundtrip": true, "noncurrent_descriptor_verified": true,
                "borrowed_native_descriptor_verified": true, "native_import_binding_restored": true,
                "shared_context_verified": true, "incompatible_share_rejected": true,
                "render_readback_verified": true,
            })
        );
    }
}
