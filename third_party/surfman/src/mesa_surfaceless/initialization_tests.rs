// SPDX-License-Identifier: MPL-2.0
use super::{Connection, Context, ContextDescriptor, Device, NativeContext};
use crate::base::egl::device::EGL_FUNCTIONS;
use crate::{
    ContextAttributeFlags, ContextAttributes, Error, GLVersion, SurfaceAccess, SurfaceType, egl,
};
use euclid::default::Size2D;
use glow::HasContext;
use std::cell::Cell;

struct RawConfiglessContext<'a> {
    device: &'a Device,
    native: NativeContext,
    api: crate::egl::types::EGLenum,
}

impl<'a> RawConfiglessContext<'a> {
    fn new(device: &'a Device, api: crate::egl::types::EGLenum) -> Self {
        EGL_FUNCTIONS.with(|egl| unsafe {
            assert_ne!(egl.BindAPI(api), egl::FALSE);
            let attributes = [
                egl::CONTEXT_CLIENT_VERSION as i32,
                3,
                crate::base::egl::ffi::EGL_CONTEXT_MINOR_VERSION_KHR as i32,
                if api == egl::OPENGL_API { 3 } else { 0 },
                egl::NONE as i32,
            ];
            let context = egl.CreateContext(
                device.native_connection.egl_display,
                std::ptr::null(),
                egl::NO_CONTEXT,
                attributes.as_ptr(),
            );
            assert_ne!(context, egl::NO_CONTEXT);
            let result = Self {
                device,
                native: NativeContext {
                    egl_context: context,
                    egl_draw_surface: egl::NO_SURFACE,
                    egl_read_surface: egl::NO_SURFACE,
                },
                api,
            };
            result.make_current();
            result
        })
    }

    fn make_current(&self) {
        EGL_FUNCTIONS.with(|egl| unsafe {
            assert_ne!(egl.BindAPI(self.api), egl::FALSE);
            assert_ne!(
                egl.MakeCurrent(
                    self.device.native_connection.egl_display,
                    self.native.egl_draw_surface,
                    self.native.egl_read_surface,
                    self.native.egl_context,
                ),
                egl::FALSE
            );
        });
    }

    fn attach_default_pbuffer(&mut self, descriptor: &ContextDescriptor) {
        let config = unsafe {
            crate::base::egl::context::try_egl_config_from_id(
                self.device.native_connection.egl_display,
                descriptor.egl_config_id,
            )
            .unwrap()
        };
        let attributes = [
            egl::WIDTH as i32,
            8,
            egl::HEIGHT as i32,
            8,
            egl::NONE as i32,
        ];
        let surface = EGL_FUNCTIONS.with(|egl| unsafe {
            egl.CreatePbufferSurface(
                self.device.native_connection.egl_display,
                config,
                attributes.as_ptr(),
            )
        });
        assert_ne!(surface, egl::NO_SURFACE);
        self.native.egl_draw_surface = surface;
        self.native.egl_read_surface = surface;
        self.make_current();
    }
}

impl Drop for RawConfiglessContext<'_> {
    fn drop(&mut self) {
        EGL_FUNCTIONS.with(|egl| unsafe {
            egl.BindAPI(self.api);
            if egl.GetCurrentContext() == self.native.egl_context {
                egl.MakeCurrent(
                    self.device.native_connection.egl_display,
                    egl::NO_SURFACE,
                    egl::NO_SURFACE,
                    egl::NO_CONTEXT,
                );
            }
            let destroyed = egl.DestroyContext(
                self.device.native_connection.egl_display,
                self.native.egl_context,
            );
            if !std::thread::panicking() {
                assert_ne!(destroyed, egl::FALSE);
            }
            if self.native.egl_draw_surface != egl::NO_SURFACE {
                let destroyed = egl.DestroySurface(
                    self.device.native_connection.egl_display,
                    self.native.egl_draw_surface,
                );
                if !std::thread::panicking() {
                    assert_ne!(destroyed, egl::FALSE);
                }
            }
        });
    }
}

fn device() -> Device {
    let connection = Connection::new().unwrap();
    let adapter = if std::env::var_os("DOME_NATIVE_INIT_SOFTWARE").is_some() {
        connection.create_software_adapter().unwrap()
    } else {
        connection.create_low_power_adapter().unwrap()
    };
    connection.create_device(&adapter).unwrap()
}
fn descriptor(device: &Device) -> ContextDescriptor {
    device
        .create_context_descriptor(&ContextAttributes {
            version: GLVersion::new(3, 3),
            flags: ContextAttributeFlags::ALPHA
                | ContextAttributeFlags::DEPTH
                | ContextAttributeFlags::STENCIL,
        })
        .unwrap()
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
fn green(device: &Device, context: &Context) {
    device.make_context_current(context).unwrap();
    let gl = &context.1;
    unsafe {
        println!(
            "DOME_INITIALIZATION_RENDERER={}",
            gl.get_parameter_string(glow::RENDERER)
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
}

#[test]
fn initialization_owned_loader_failure_restores_previous_binding() {
    let device = device();
    let descriptor = descriptor(&device);
    let mut previous = device.create_context(&descriptor, None).unwrap();
    attach(&device, &mut previous);
    let binding = NativeContext::current().unwrap().egl_context;
    let failed_native = Cell::new(None);
    let result = device.create_context_with_loader(&descriptor, None, || {
        failed_native.set(Some(NativeContext::current().unwrap().egl_context));
        Err("Unavailable initialization metadata fixture".into())
    });
    assert!(matches!(
        result,
        Err(Error::GLContextInitializationFailed(_))
    ));
    assert_eq!(NativeContext::current().unwrap().egl_context, binding);
    assert_ne!(failed_native.get().unwrap(), binding);
    // EGL specifies a failed query for a context that has been destroyed.
    EGL_FUNCTIONS.with(|egl| unsafe {
        let mut id = 0;
        assert_eq!(
            egl.QueryContext(
                device.native_connection.egl_display,
                failed_native.get().unwrap(),
                egl::CONFIG_ID as i32,
                &mut id
            ),
            egl::FALSE
        );
        assert_eq!(egl.GetError() as u32, egl::BAD_CONTEXT);
    });
    green(&device, &previous);
    device.destroy_context(&mut previous).unwrap();
}

#[test]
fn initialization_owned_loader_failure_restores_absent_binding() {
    let device = device();
    let descriptor = descriptor(&device);
    device.make_no_context_current().unwrap();
    assert!(
        device
            .create_context_with_loader(&descriptor, None, || Err("Fixture".into()))
            .is_err()
    );
    assert!(matches!(
        NativeContext::current(),
        Err(Error::NoCurrentContext)
    ));
}

#[test]
fn initialization_guard_restores_client_api_selection() {
    let _device = device();
    EGL_FUNCTIONS.with(|egl| unsafe {
        assert_ne!(egl.BindAPI(egl::OPENGL_ES_API), egl::FALSE);
        {
            let _guard = crate::base::egl::context::CurrentContextGuard::new();
            assert_ne!(egl.BindAPI(egl::OPENGL_API), egl::FALSE);
        }
        assert_eq!(egl.QueryAPI(), egl::OPENGL_ES_API);
    });
}

#[test]
fn initialization_owned_failure_restores_absent_gles_selection() {
    let device = device();
    let descriptor = descriptor(&device);
    device.make_no_context_current().unwrap();
    EGL_FUNCTIONS.with(|egl| unsafe {
        assert_ne!(egl.BindAPI(egl::OPENGL_ES_API), egl::FALSE);
    });
    assert!(
        device
            .create_context_with_loader(&descriptor, None, || Err("Fixture".into()))
            .is_err()
    );
    EGL_FUNCTIONS.with(|egl| unsafe {
        assert_eq!(egl.QueryAPI(), egl::OPENGL_ES_API);
        assert_eq!(egl.GetCurrentContext(), egl::NO_CONTEXT);
    });
}

#[test]
fn initialization_owned_failure_restores_live_gles_context() {
    let device = device();
    let descriptor = descriptor(&device);
    let external = RawConfiglessContext::new(&device, egl::OPENGL_ES_API);
    assert!(
        device
            .create_context_with_loader(&descriptor, None, || Err("Fixture".into()))
            .is_err()
    );
    EGL_FUNCTIONS.with(|egl| unsafe {
        assert_eq!(egl.QueryAPI(), egl::OPENGL_ES_API);
        assert_eq!(egl.GetCurrentContext(), external.native.egl_context);
    });
    let gl = unsafe {
        crate::Gl::try_from_loader_function(crate::base::egl::context::get_proc_address).unwrap()
    };
    assert!(gl.version().is_embedded);
}

#[test]
fn initialization_configless_import_and_descriptor_reuse_preserve_owner() {
    let device = device();
    let descriptor = descriptor(&device);
    let external = RawConfiglessContext::new(&device, egl::OPENGL_API);
    let mut previous = device.create_context(&descriptor, None).unwrap();
    attach(&device, &mut previous);
    let binding = NativeContext::current().unwrap().egl_context;
    let mut imported = unsafe {
        device
            .create_context_from_native_context(external.native)
            .unwrap()
    };
    assert_eq!(NativeContext::current().unwrap().egl_context, binding);
    let actual = device.context_descriptor(&imported);
    assert_eq!(actual.egl_config_id, 0);
    let attributes = device.context_descriptor_attributes(&actual);
    assert!(!attributes.flags.intersects(
        ContextAttributeFlags::ALPHA
            | ContextAttributeFlags::DEPTH
            | ContextAttributeFlags::STENCIL
    ));
    device.destroy_context(&mut imported).unwrap();
    external.make_current();
    // Destroying the borrowed wrapper must leave the external owner usable.
    let gl = unsafe {
        crate::Gl::try_from_loader_function(crate::base::egl::context::get_proc_address).unwrap()
    };
    assert!(!gl.version().is_embedded);
    let mut recreated = device.create_context(&actual, None).unwrap();
    attach(&device, &mut recreated);
    green(&device, &recreated);
    device.destroy_context(&mut recreated).unwrap();
    green(&device, &previous);
    device.destroy_context(&mut previous).unwrap();
}

#[test]
fn initialization_configless_import_captures_default_surface_format() {
    let device = device();
    let descriptor = descriptor(&device);
    let mut external = RawConfiglessContext::new(&device, egl::OPENGL_API);
    external.attach_default_pbuffer(&descriptor);
    let mut imported = unsafe {
        device
            .create_context_from_native_context(external.native)
            .unwrap()
    };
    let actual = device.context_descriptor(&imported);
    assert_eq!(actual.egl_config_id, 0);
    let attributes = device.context_descriptor_attributes(&actual);
    assert!(attributes.flags.contains(
        ContextAttributeFlags::ALPHA
            | ContextAttributeFlags::DEPTH
            | ContextAttributeFlags::STENCIL
    ));
    green(&device, &imported);
    device.destroy_context(&mut imported).unwrap();
    external.make_current();
    // The borrowed surface survives wrapper destruction with the native owner.
    EGL_FUNCTIONS.with(|egl| unsafe {
        let mut width = 0;
        assert_ne!(
            egl.QuerySurface(
                device.native_connection.egl_display,
                external.native.egl_draw_surface,
                egl::WIDTH as i32,
                &mut width,
            ),
            egl::FALSE
        );
        assert_eq!(width, 8);
    });
}

#[test]
fn initialization_failed_import_restores_live_gles_context() {
    let device = device();
    let descriptor = descriptor(&device);
    let external = device.create_context(&descriptor, None).unwrap();
    let native = device.native_context(&external);
    let previous = RawConfiglessContext::new(&device, egl::OPENGL_ES_API);
    assert!(
        unsafe {
            device.create_context_from_native_context_with_loader(native, || Err("Fixture".into()))
        }
        .is_err()
    );
    EGL_FUNCTIONS.with(|egl| unsafe {
        assert_eq!(egl.QueryAPI(), egl::OPENGL_ES_API);
        assert_eq!(egl.GetCurrentContext(), previous.native.egl_context);
    });
    drop(previous);
    let mut external = external;
    device.destroy_context(&mut external).unwrap();
}

#[test]
fn initialization_failure_does_not_poison_a_healthy_shared_peer() {
    let device = device();
    let descriptor = descriptor(&device);
    let mut peer = device.create_context(&descriptor, None).unwrap();
    attach(&device, &mut peer);
    assert!(
        device
            .create_context_with_loader(&descriptor, Some(&peer), || Err("Fixture".into()))
            .is_err()
    );
    assert_eq!(device.graphics_reset_status(&peer).unwrap(), None);
    green(&device, &peer);
    device.destroy_context(&mut peer).unwrap();
}

#[test]
fn optional_robust_context_reports_resets_and_preserves_the_peer_binding() {
    let device = device();
    let mut owner = crate::recovery::create_context_with_optional_robustness(
        &device,
        ContextAttributes {
            version: GLVersion::new(3, 3),
            flags: ContextAttributeFlags::ALPHA,
        },
    )
    .unwrap();
    attach(&device, &mut owner);
    green(&device, &owner);
    let flags = owner.2.robustness_flags;
    let mut peer = device.create_context(&descriptor(&device), None).unwrap();
    attach(&device, &mut peer);
    let binding = NativeContext::current().unwrap().egl_context;
    let monitor = crate::recovery::ResetStatusMonitor::new();
    let healthy = monitor.check(&device, &mut owner).is_ok() && monitor.enabled();
    let preserved = NativeContext::current().unwrap().egl_context == binding;
    let status = device.graphics_reset_status(&owner).unwrap();
    device.mark_context_lost(&mut owner).unwrap();
    let rejected = monitor
        .check(&device, &mut owner)
        .unwrap_err()
        .is_context_lost();
    let still_preserved = NativeContext::current().unwrap().egl_context == binding;
    green(&device, &peer);
    device.destroy_context(&mut owner).unwrap();
    device.destroy_context(&mut peer).unwrap();
    assert!(flags.contains(
        ContextAttributeFlags::RESET_NOTIFICATION | ContextAttributeFlags::ROBUST_ACCESS
    ));
    assert_eq!(status, Some(crate::GraphicsResetStatus::NoError));
    assert!(healthy && preserved && rejected && still_preserved);
}

#[test]
fn initialization_failed_native_import_preserves_external_owner() {
    let device = device();
    let descriptor = descriptor(&device);
    let mut external = device.create_context(&descriptor, None).unwrap();
    attach(&device, &mut external);
    let native = device.native_context(&external);
    let mut previous = device.create_context(&descriptor, None).unwrap();
    attach(&device, &mut previous);
    let binding = NativeContext::current().unwrap().egl_context;
    let result = unsafe {
        device.create_context_from_native_context_with_loader(native, || {
            assert_eq!(
                NativeContext::current().unwrap().egl_context,
                native.egl_context
            );
            Err("Unavailable import metadata fixture".into())
        })
    };
    assert!(matches!(
        result,
        Err(Error::GLContextInitializationFailed(_))
    ));
    assert_eq!(NativeContext::current().unwrap().egl_context, binding);
    green(&device, &external);
    device.destroy_context(&mut external).unwrap();
    device.destroy_context(&mut previous).unwrap();
}

#[test]
fn initialization_private_display_failure_returns_error() {
    let device = device();
    let descriptor = descriptor(&device);
    device.make_no_context_current().unwrap();
    let result = device.create_context_with_loader(&descriptor, None, || unsafe {
        let gl = crate::Gl::try_from_loader_function(crate::base::egl::context::get_proc_address)?;
        // This is the test process's offscreen display, never the desktop or GPU.
        EGL_FUNCTIONS.with(|egl| {
            assert_ne!(
                egl.Terminate(device.native_connection.egl_display),
                egl::FALSE
            );
        });
        Ok(gl)
    });
    assert!(matches!(result, Err(Error::ContextCreationFailed(_))));
    assert!(matches!(
        NativeContext::current(),
        Err(Error::NoCurrentContext)
    ));
    let cached = device.context_descriptor_attributes(&descriptor);
    assert!(cached.flags.contains(ContextAttributeFlags::ALPHA));
    assert!(cached.flags.contains(ContextAttributeFlags::DEPTH));
    assert!(cached.flags.contains(ContextAttributeFlags::STENCIL));
}
