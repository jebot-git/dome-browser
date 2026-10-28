//! Functionality common to backends using EGL contexts.

use super::device::EGL_FUNCTIONS;
use super::error::ToWindowingApiError;
use super::ffi::EGL_CONTEXT_OPENGL_PROFILE_MASK;
use super::ffi::{EGL_CONTEXT_MINOR_VERSION_KHR, EGL_CONTEXT_OPENGL_COMPATIBILITY_PROFILE_BIT};
use super::robustness;
use super::surface::{EGLBackedSurface, ExternalEGLSurfaces};
use crate::context::{self, CREATE_CONTEXT_MUTEX};
use crate::egl;
use crate::egl::types::{EGLConfig, EGLContext, EGLDisplay, EGLSurface, EGLenum, EGLint};
use crate::surface::Framebuffer;
use crate::{ContextAttributeFlags, ContextAttributes, ContextID, Error, GLApi, GLVersion};
use crate::{Gl, SurfaceInfo};
use euclid::default::Size2D;
use glow::HasContext;

use std::cell::Cell;
use std::ffi::{CStr, CString};
use std::mem;
use std::os::raw::c_void;
use std::ptr;
use std::rc::Rc;
use std::thread;

#[allow(dead_code)]
const DUMMY_PBUFFER_SIZE: EGLint = 16;
const RGB_CHANNEL_BIT_DEPTH: EGLint = 8;

pub(crate) struct EGLBackedContext {
    pub(crate) egl_context: EGLContext,
    pub(crate) id: ContextID,
    pbuffer: EGLSurface,
    pub(crate) framebuffer: Framebuffer<EGLBackedSurface, ExternalEGLSurfaces>,
    context_is_owned: bool,
    pub(crate) lost: Rc<Cell<bool>>,
}

/// Wrapper for a native `EGLContext`.
#[derive(Clone, Copy)]
pub struct NativeContext {
    /// The EGL context.
    pub egl_context: EGLContext,
    /// The EGL read surface that is to be attached to that context.
    pub egl_read_surface: EGLSurface,
    /// The EGL draw surface that is to be attached to that context.
    pub egl_draw_surface: EGLSurface,
}

/// Information needed to create a context. Some APIs call this a "config" or a "pixel format".
///
/// These are local to a device.
#[derive(Clone)]
pub struct ContextDescriptor {
    pub(crate) egl_config_id: EGLint,
    pub(crate) gl_version: GLVersion,
    pub(crate) compatibility_profile: bool,
    pub(crate) robustness_flags: ContextAttributeFlags,
    pixel_format_flags: ContextAttributeFlags,
}

#[must_use]
pub(crate) struct CurrentContextGuard {
    active: bool,
    old_client_api: EGLenum,
    egl_display: EGLDisplay,
    old_egl_draw_surface: EGLSurface,
    old_egl_read_surface: EGLSurface,
    old_egl_context: EGLContext,
}

impl CurrentContextGuard {
    pub(crate) fn new() -> CurrentContextGuard {
        EGL_FUNCTIONS.with(|egl| unsafe {
            CurrentContextGuard {
                active: true,
                old_client_api: egl.QueryAPI(),
                egl_display: egl.GetCurrentDisplay(),
                old_egl_draw_surface: egl.GetCurrentSurface(egl::DRAW as EGLint),
                old_egl_read_surface: egl.GetCurrentSurface(egl::READ as EGLint),
                old_egl_context: egl.GetCurrentContext(),
            }
        })
    }
}

impl CurrentContextGuard {
    pub(crate) fn keep_current(mut self) {
        self.active = false;
    }
}

impl Drop for CurrentContextGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        EGL_FUNCTIONS.with(|egl| unsafe {
            // EGL binding queries and releasing a context depend on the selected
            // client API. Context creation may have switched GL/GLES meanwhile.
            if egl.QueryAPI() != self.old_client_api
                && self.old_client_api != egl::NONE
                && egl.BindAPI(self.old_client_api) == egl::FALSE
            {
                return;
            }
            if self.egl_display == egl::NO_DISPLAY {
                // A temporary binding may have started with no current context.
                // Restore that absence using the display bound in the meantime.
                let current_display = egl.GetCurrentDisplay();
                if current_display != egl::NO_DISPLAY {
                    egl.MakeCurrent(
                        current_display,
                        egl::NO_SURFACE,
                        egl::NO_SURFACE,
                        egl::NO_CONTEXT,
                    );
                }
                return;
            }

            // Only call eglMakeCurrent again if anything has changed.
            if egl.GetCurrentDisplay() == self.egl_display
                && egl.GetCurrentSurface(egl::DRAW as EGLint) == self.old_egl_draw_surface
                && egl.GetCurrentSurface(egl::READ as EGLint) == self.old_egl_read_surface
                && egl.GetCurrentContext() == self.old_egl_context
            {
                return;
            }

            egl.MakeCurrent(
                self.egl_display,
                self.old_egl_draw_surface,
                self.old_egl_read_surface,
                self.old_egl_context,
            );
        })
    }
}

impl Drop for EGLBackedContext {
    #[inline]
    fn drop(&mut self) {
        if self.egl_context != egl::NO_CONTEXT && !thread::panicking() {
            panic!("Contexts must be destroyed explicitly with `destroy_context`!")
        }
    }
}

impl EGLBackedContext {
    pub(crate) unsafe fn new(
        egl_display: EGLDisplay,
        descriptor: &ContextDescriptor,
        share_with: Option<&EGLBackedContext>,
        gl_api: GLApi,
        connection_owner: Box<dyn std::any::Any + Send>,
    ) -> Result<EGLBackedContext, Error> {
        collect_failed_creations();
        if share_with.is_some_and(|context| context.lost.get()) {
            return Err(Error::MakeCurrentFailed(
                crate::WindowingApiError::ContextLost,
            ));
        }
        let mut next_context_id = CREATE_CONTEXT_MUTEX.lock().unwrap();

        // Create the context.
        let egl_context = create_context(
            egl_display,
            descriptor,
            share_with.map_or(egl::NO_CONTEXT, |ctx| ctx.egl_context),
            gl_api,
        )?;

        // Establish native ownership before any further fallible EGL query.
        let mut context = EGLBackedContext {
            egl_context,
            id: *next_context_id,
            framebuffer: Framebuffer::None,
            context_is_owned: true,
            lost: share_with
                .map(|context| context.lost.clone())
                .unwrap_or_else(|| Rc::new(Cell::new(false))),
            pbuffer: egl::NO_SURFACE,
        };
        match try_create_dummy_pbuffer(egl_display, descriptor.egl_config_id) {
            Ok(pbuffer) => context.pbuffer = pbuffer.unwrap_or(egl::NO_SURFACE),
            Err(error) => {
                if matches!(
                    error,
                    Error::SurfaceCreationFailed(crate::WindowingApiError::ContextLost)
                ) {
                    context.lost.set(true);
                }
                context.retire_failed_creation(egl_display, connection_owner);
                return Err(error);
            },
        }
        next_context_id.0 += 1;
        Ok(context)
    }

    pub(crate) unsafe fn from_native_context(native_context: NativeContext) -> EGLBackedContext {
        let mut next_context_id = CREATE_CONTEXT_MUTEX.lock().unwrap();
        let context = EGLBackedContext {
            egl_context: native_context.egl_context,
            id: *next_context_id,
            framebuffer: Framebuffer::External(ExternalEGLSurfaces {
                draw: native_context.egl_draw_surface,
                read: native_context.egl_read_surface,
            }),
            context_is_owned: false,
            lost: Rc::new(Cell::new(false)),
            pbuffer: egl::NO_SURFACE,
        };
        next_context_id.0 += 1;
        context
    }

    pub(crate) unsafe fn destroy(&mut self, egl_display: EGLDisplay) -> Result<(), Error> {
        destroy_native_handles(
            egl_display,
            &mut self.egl_context,
            &mut self.pbuffer,
            self.context_is_owned,
            self.lost.get(),
        )
    }

    // A failed initialization never exposes this context or attaches a surface.
    // Transfer native ownership to the retry queue if EGL cannot release it yet.
    pub(crate) unsafe fn retire_failed_creation(
        mut self,
        egl_display: EGLDisplay,
        connection_owner: Box<dyn std::any::Any + Send>,
    ) {
        if destroy_native_handles(
            egl_display,
            &mut self.egl_context,
            &mut self.pbuffer,
            true,
            true,
        )
        .is_err()
        {
            PENDING_NATIVE_CONTEXTS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(PendingNativeContext {
                    display: egl_display as usize,
                    context: self.egl_context as usize,
                    pbuffer: self.pbuffer as usize,
                    _connection_owner: connection_owner,
                });
            self.egl_context = egl::NO_CONTEXT;
            self.pbuffer = egl::NO_SURFACE;
        }
    }

    pub(crate) fn native_context(&self) -> NativeContext {
        let egl_surfaces = match self.framebuffer {
            Framebuffer::Surface(ref surface) => surface.egl_surfaces(),
            Framebuffer::External(ref surfaces) => (*surfaces).clone(),
            Framebuffer::None => ExternalEGLSurfaces::default(),
        };

        NativeContext {
            egl_context: self.egl_context,
            egl_draw_surface: egl_surfaces.draw,
            egl_read_surface: egl_surfaces.read,
        }
    }

    pub(crate) unsafe fn make_current(&self, egl_display: EGLDisplay) -> Result<(), Error> {
        collect_failed_creations();
        if self.lost.get() {
            return Err(Error::MakeCurrentFailed(
                crate::WindowingApiError::ContextLost,
            ));
        }

        let egl_surfaces = match self.framebuffer {
            Framebuffer::Surface(ref surface) => surface.egl_surfaces(),
            Framebuffer::External(ref surfaces) => (*surfaces).clone(),
            Framebuffer::None => ExternalEGLSurfaces {
                draw: self.pbuffer,
                read: self.pbuffer,
            },
        };

        EGL_FUNCTIONS.with(|egl| {
            let result = egl.MakeCurrent(
                egl_display,
                egl_surfaces.draw,
                egl_surfaces.read,
                self.egl_context,
            );
            if result == egl::FALSE {
                let err = egl.GetError().to_windowing_api_error();
                return Err(Error::MakeCurrentFailed(err));
            }
            Ok(())
        })
    }

    #[inline]
    pub(crate) fn is_current(&self) -> bool {
        unsafe { EGL_FUNCTIONS.with(|egl| egl.GetCurrentContext() == self.egl_context) }
    }

    pub(crate) unsafe fn bind_surface(
        &mut self,
        egl_display: EGLDisplay,
        surface: EGLBackedSurface,
    ) -> Result<(), (Error, EGLBackedSurface)> {
        if self.id != surface.context_id {
            return Err((Error::IncompatibleSurface, surface));
        }

        match self.framebuffer {
            Framebuffer::None => self.framebuffer = Framebuffer::Surface(surface),
            Framebuffer::External(_) => return Err((Error::ExternalRenderTarget, surface)),
            Framebuffer::Surface(_) => return Err((Error::SurfaceAlreadyBound, surface)),
        }

        // If we're current, call `make_context_current()` again to switch to the new framebuffer.
        if self.is_current() {
            drop(self.make_current(egl_display))
        }

        Ok(())
    }

    pub(crate) unsafe fn unbind_surface(
        &mut self,
        gl: &Gl,
        egl_display: EGLDisplay,
    ) -> Result<Option<EGLBackedSurface>, Error> {
        // Flush to avoid races on Mesa/Intel and possibly other GPUs.
        if !self.lost.get() {
            gl.flush();
        }

        match self.framebuffer {
            Framebuffer::None => return Ok(None),
            Framebuffer::Surface(_) => {},
            Framebuffer::External(_) => return Err(Error::ExternalRenderTarget),
        }

        let surface = match mem::replace(&mut self.framebuffer, Framebuffer::None) {
            Framebuffer::Surface(surface) => surface,
            Framebuffer::None | Framebuffer::External(_) => unreachable!(),
        };

        // If we're current, we stay current, but with no surface attached.
        if !self.lost.get() {
            surface.unbind(gl, egl_display, self.egl_context);
        }

        Ok(Some(surface))
    }

    pub(crate) fn present_bound_surface(&self, egl_display: EGLDisplay) -> Result<(), Error> {
        match &self.framebuffer {
            Framebuffer::Surface(surface) => surface.present(egl_display, self.egl_context),
            Framebuffer::None | Framebuffer::External(_) => Ok(()),
        }
    }

    pub(crate) fn resize_bound_surface(&mut self, size: Size2D<i32>) -> Result<(), Error> {
        if let Framebuffer::Surface(surface) = &mut self.framebuffer {
            surface.resize(size);
        }
        Ok(())
    }

    pub(crate) fn surface_info(&self) -> Result<Option<SurfaceInfo>, Error> {
        match self.framebuffer {
            Framebuffer::None => Ok(None),
            Framebuffer::External(_) => Err(Error::ExternalRenderTarget),
            Framebuffer::Surface(ref surface) => Ok(Some(surface.info())),
        }
    }
}

// These contexts were never exposed or made usable. EGL deletion is legal
// across threads and does not issue GL commands. Handles are retained globally
// until a later context operation can release them (or the process exits).
struct PendingNativeContext {
    display: usize,
    context: usize,
    pbuffer: usize,
    // Native handles cannot outlive the owning display connection. Keep its
    // reference until cleanup succeeds; borrowed displays retain their external
    // owner's existing unsafe lifetime requirement.
    _connection_owner: Box<dyn std::any::Any + Send>,
}
static PENDING_NATIVE_CONTEXTS: std::sync::Mutex<Vec<PendingNativeContext>> =
    std::sync::Mutex::new(Vec::new());

unsafe fn collect_failed_creations() {
    let mut pending = PENDING_NATIVE_CONTEXTS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _ = crate::retirement::retire_pending(&mut pending, |item| {
        let mut context = item.context as EGLContext;
        let mut pbuffer = item.pbuffer as EGLSurface;
        let result = destroy_native_handles(
            item.display as EGLDisplay,
            &mut context,
            &mut pbuffer,
            true,
            true,
        );
        item.context = context as usize;
        item.pbuffer = pbuffer as usize;
        result
    });
}

unsafe fn destroy_native_handles(
    egl_display: EGLDisplay,
    context: &mut EGLContext,
    pbuffer: &mut EGLSurface,
    owned: bool,
    lost: bool,
) -> Result<(), Error> {
    if *context == egl::NO_CONTEXT {
        return Ok(());
    }
    EGL_FUNCTIONS.with(|egl| {
        if egl.GetCurrentContext() == *context {
            let result = egl.MakeCurrent(
                egl_display,
                egl::NO_SURFACE,
                egl::NO_SURFACE,
                egl::NO_CONTEXT,
            );
            if result == egl::FALSE {
                let error = egl.GetError();
                if !lost {
                    return Err(Error::MakeCurrentFailed(error.to_windowing_api_error()));
                }
                // Release only this thread's retired binding. This is also
                // valid after its display/context handles were invalidated.
                egl.ReleaseThread();
            }
        }
        if *pbuffer != egl::NO_SURFACE {
            let result = egl.DestroySurface(egl_display, *pbuffer);
            if result == egl::FALSE {
                let error = egl.GetError();
                if !lost
                    || !matches!(
                        error as u32,
                        egl::BAD_DISPLAY | egl::NOT_INITIALIZED | egl::BAD_SURFACE
                    )
                {
                    return Err(Error::SurfaceDestructionFailed(
                        error.to_windowing_api_error(),
                    ));
                }
            }
            *pbuffer = egl::NO_SURFACE;
        }
        if owned {
            let result = egl.DestroyContext(egl_display, *context);
            if result == egl::FALSE {
                let error = egl.GetError();
                if !lost
                    || !matches!(
                        error as u32,
                        egl::BAD_DISPLAY | egl::NOT_INITIALIZED | egl::BAD_CONTEXT
                    )
                {
                    return Err(Error::ContextDestructionFailed(
                        error.to_windowing_api_error(),
                    ));
                }
            }
        }
        *context = egl::NO_CONTEXT;
        Ok(())
    })
}

impl NativeContext {
    /// Returns the current EGL context and surfaces, if applicable.
    ///
    /// If there is no current EGL context, this returns a `NoCurrentContext` error.
    pub fn current() -> Result<NativeContext, Error> {
        EGL_FUNCTIONS.with(|egl| unsafe {
            let egl_context = egl.GetCurrentContext();
            if egl_context == egl::NO_CONTEXT {
                Err(Error::NoCurrentContext)
            } else {
                Ok(NativeContext {
                    egl_context,
                    egl_read_surface: egl.GetCurrentSurface(egl::READ as EGLint),
                    egl_draw_surface: egl.GetCurrentSurface(egl::DRAW as EGLint),
                })
            }
        })
    }
}

impl ContextDescriptor {
    pub(crate) unsafe fn new(
        egl_display: EGLDisplay,
        attributes: &ContextAttributes,
        extra_config_attributes: &[EGLint],
    ) -> Result<ContextDescriptor, Error> {
        let flags = attributes.flags;

        let alpha_size = if flags.contains(ContextAttributeFlags::ALPHA) {
            8
        } else {
            0
        };
        let depth_size = if flags.contains(ContextAttributeFlags::DEPTH) {
            24
        } else {
            0
        };
        let stencil_size = if flags.contains(ContextAttributeFlags::STENCIL) {
            8
        } else {
            0
        };

        let compatibility_profile = flags.contains(ContextAttributeFlags::COMPATIBILITY_PROFILE);

        // Mesa doesn't support the OpenGL compatibility profile post version 3.0. Take that into
        // account.
        if compatibility_profile
            && (attributes.version.major > 3
                || attributes.version.major == 3 && attributes.version.minor > 0)
        {
            return Err(Error::UnsupportedGLProfile);
        }

        // Android/OHOS does not support OpenGL compatibility profile at all
        if cfg!(any(android_platform, ohos_platform)) && compatibility_profile {
            return Err(Error::UnsupportedGLProfile);
        }

        // Create required config attributes.
        //
        // We check these separately because `eglChooseConfig` on its own might give us 32-bit
        // color when 24-bit color is requested, and that can break code.
        let required_config_attributes = [
            egl::RED_SIZE as EGLint,
            RGB_CHANNEL_BIT_DEPTH,
            egl::GREEN_SIZE as EGLint,
            RGB_CHANNEL_BIT_DEPTH,
            egl::BLUE_SIZE as EGLint,
            RGB_CHANNEL_BIT_DEPTH,
        ];

        // Create config attributes.
        let mut requested_config_attributes = required_config_attributes.to_vec();
        requested_config_attributes.extend_from_slice(&[
            egl::ALPHA_SIZE as EGLint,
            alpha_size,
            egl::DEPTH_SIZE as EGLint,
            depth_size,
            egl::STENCIL_SIZE as EGLint,
            stencil_size,
        ]);
        requested_config_attributes.extend_from_slice(extra_config_attributes);
        requested_config_attributes.extend_from_slice(&[egl::NONE as EGLint, 0, 0, 0]);

        EGL_FUNCTIONS.with(|egl| {
            // See how many applicable configs there are.
            let mut config_count = 0;
            let result = egl.ChooseConfig(
                egl_display,
                requested_config_attributes.as_ptr(),
                ptr::null_mut(),
                0,
                &mut config_count,
            );
            if result == egl::FALSE {
                let err = egl.GetError().to_windowing_api_error();
                return Err(Error::PixelFormatSelectionFailed(err));
            }
            if config_count <= 0 {
                return Err(Error::NoPixelFormatFound);
            }

            // Enumerate all those configs.
            let mut configs = vec![ptr::null(); config_count as usize];
            let mut real_config_count = config_count;
            let result = egl.ChooseConfig(
                egl_display,
                requested_config_attributes.as_ptr(),
                configs.as_mut_ptr(),
                config_count,
                &mut real_config_count,
            );
            if result == egl::FALSE {
                let err = egl.GetError().to_windowing_api_error();
                return Err(Error::PixelFormatSelectionFailed(err));
            }

            if real_config_count <= 0 || real_config_count > config_count {
                return Err(Error::NoPixelFormatFound);
            }
            let mut selected = None;
            for config in configs.into_iter().take(real_config_count as usize) {
                if config.is_null() {
                    continue;
                }
                let mut matches = true;
                for pair in required_config_attributes.chunks(2) {
                    if try_get_config_attr(egl_display, config, pair[0])? != pair[1] {
                        matches = false;
                        break;
                    }
                }
                if matches {
                    selected = Some(config);
                    break;
                }
            }
            let config = selected.ok_or(Error::NoPixelFormatFound)?;
            Ok(ContextDescriptor {
                egl_config_id: try_get_config_attr(egl_display, config, egl::CONFIG_ID as EGLint)?,
                gl_version: attributes.version,
                compatibility_profile,
                robustness_flags: flags
                    & (ContextAttributeFlags::RESET_NOTIFICATION
                        | ContextAttributeFlags::ROBUST_ACCESS),
                pixel_format_flags: capture_pixel_format_flags(egl_display, config)?,
            })
        })
    }

    pub(crate) unsafe fn from_egl_context(
        gl: &Gl,
        egl_display: EGLDisplay,
        egl_context: EGLContext,
    ) -> ContextDescriptor {
        Self::try_from_egl_context(gl, egl_display, egl_context)
            .expect("Could not describe the current EGL context")
    }

    pub(crate) unsafe fn try_from_egl_context(
        gl: &Gl,
        egl_display: EGLDisplay,
        egl_context: EGLContext,
    ) -> Result<ContextDescriptor, Error> {
        let egl_config_id =
            try_get_context_attr(egl_display, egl_context, egl::CONFIG_ID as EGLint)?;
        let pixel_format_flags = if egl_config_id == 0 {
            // EGL_KHR_no_config_context specifies zero for a valid context
            // without an EGLConfig. Its format belongs to the current draw
            // surface, if any; a surfaceless context has no default framebuffer.
            let surface_config_id = EGL_FUNCTIONS.with(|egl| {
                let draw = egl.GetCurrentSurface(egl::DRAW as EGLint);
                if draw == egl::NO_SURFACE {
                    return Ok(None);
                }
                let mut id = 0;
                if egl.QuerySurface(egl_display, draw, egl::CONFIG_ID as EGLint, &mut id)
                    == egl::FALSE
                {
                    return Err(Error::PixelFormatSelectionFailed(
                        egl.GetError().to_windowing_api_error(),
                    ));
                }
                Ok(Some(id))
            })?;
            match surface_config_id {
                Some(id) => capture_pixel_format_flags(
                    egl_display,
                    try_egl_config_from_id(egl_display, id)?,
                )?,
                None => ContextAttributeFlags::empty(),
            }
        } else {
            capture_pixel_format_flags(
                egl_display,
                try_egl_config_from_id(egl_display, egl_config_id)?,
            )?
        };
        let descriptor = ContextDescriptor {
            egl_config_id,
            gl_version: GLVersion::current(gl),
            compatibility_profile: context::current_context_uses_compatibility_profile(gl),
            robustness_flags: robustness::current_flags(gl),
            pixel_format_flags,
        };
        gl.validate_current_version()
            .map_err(Error::GLContextInitializationFailed)?;
        Ok(descriptor)
    }

    #[allow(dead_code)]
    pub(crate) unsafe fn to_egl_config(&self, egl_display: EGLDisplay) -> EGLConfig {
        let config_attributes = [
            egl::CONFIG_ID as EGLint,
            self.egl_config_id,
            egl::NONE as EGLint,
            0,
            0,
            0,
        ];

        EGL_FUNCTIONS.with(|egl| {
            let (mut config, mut config_count) = (ptr::null(), 0);
            let result = egl.ChooseConfig(
                egl_display,
                config_attributes.as_ptr(),
                &mut config,
                1,
                &mut config_count,
            );
            assert_ne!(result, egl::FALSE);
            assert!(config_count > 0);
            config
        })
    }

    pub(crate) unsafe fn attributes(&self, _egl_display: EGLDisplay) -> ContextAttributes {
        // Actual format attributes are captured during fallible initialization,
        // so describing an existing context never queries an unavailable display.
        let mut flags = self.pixel_format_flags | self.robustness_flags;
        flags.set(
            ContextAttributeFlags::COMPATIBILITY_PROFILE,
            self.compatibility_profile,
        );
        ContextAttributes {
            flags,
            version: self.gl_version,
        }
    }
}

pub(crate) unsafe fn create_context(
    egl_display: EGLDisplay,
    descriptor: &ContextDescriptor,
    share_with: EGLContext,
    gl_api: GLApi,
) -> Result<EGLContext, Error> {
    EGL_FUNCTIONS.with(|egl| {
        let ok = egl.BindAPI(match gl_api {
            GLApi::GL => egl::OPENGL_API,
            GLApi::GLES => egl::OPENGL_ES_API,
        });
        if ok == egl::FALSE {
            return Err(Error::ContextCreationFailed(
                egl.GetError().to_windowing_api_error(),
            ));
        }
        Ok(())
    })?;

    let egl_config = if descriptor.egl_config_id == 0 {
        // Preserve a configless native descriptor when creating a compatible
        // context. EGL reports unsupported no-config requests as normal errors.
        ptr::null()
    } else {
        try_egl_config_from_id(egl_display, descriptor.egl_config_id)?
    };

    let mut egl_context_attributes = vec![
        egl::CONTEXT_CLIENT_VERSION as EGLint,
        descriptor.gl_version.major as EGLint,
        EGL_CONTEXT_MINOR_VERSION_KHR as EGLint,
        descriptor.gl_version.minor as EGLint,
    ];

    // D3D11 ANGLE doesn't seem happy if EGL_CONTEXT_OPENGL_PROFILE_MASK is set
    // to be a core profile.
    if descriptor.compatibility_profile {
        egl_context_attributes.extend(&[
            EGL_CONTEXT_OPENGL_PROFILE_MASK as EGLint,
            EGL_CONTEXT_OPENGL_COMPATIBILITY_PROFILE_BIT,
        ]);
    }

    if !descriptor.robustness_flags.is_empty() {
        let api = EGL_FUNCTIONS.with(|egl| {
            let string = |name| {
                let ptr = egl.QueryString(egl_display, name);
                if ptr.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(ptr).to_string_lossy().into_owned()
                }
            };
            robustness::creation_api(
                &string(egl::VERSION as EGLint),
                &string(egl::EXTENSIONS as EGLint),
                gl_api,
            )
        });
        robustness::append_attributes(
            &mut egl_context_attributes,
            descriptor.robustness_flags,
            api,
        )?;
    }

    // Include some extra zeroes to work around broken implementations.
    //
    // FIXME(pcwalton): Which implementations are those? (This is copied from Gecko.)
    egl_context_attributes.extend(&[egl::NONE as EGLint, 0, 0, 0]);

    EGL_FUNCTIONS.with(|egl| {
        let egl_context = egl.CreateContext(
            egl_display,
            egl_config,
            share_with,
            egl_context_attributes.as_ptr(),
        );
        if egl_context == egl::NO_CONTEXT {
            let err = egl.GetError();
            let err = err.to_windowing_api_error();
            return Err(Error::ContextCreationFailed(err));
        }

        Ok(egl_context)
    })
}

// Bind a borrowed native context before loading/querying its GL properties.
// The caller keeps a CurrentContextGuard alive to restore the prior binding.
pub(crate) unsafe fn make_native_context_current(
    egl_display: EGLDisplay,
    native: &NativeContext,
) -> Result<(), Error> {
    EGL_FUNCTIONS.with(|egl| {
        if egl.MakeCurrent(
            egl_display,
            native.egl_draw_surface,
            native.egl_read_surface,
            native.egl_context,
        ) == egl::FALSE
        {
            return Err(Error::MakeCurrentFailed(
                egl.GetError().to_windowing_api_error(),
            ));
        }
        Ok(())
    })
}

pub(crate) unsafe fn make_no_context_current(egl_display: EGLDisplay) -> Result<(), Error> {
    EGL_FUNCTIONS.with(|egl| {
        let result = egl.MakeCurrent(
            egl_display,
            egl::NO_SURFACE,
            egl::NO_SURFACE,
            egl::NO_CONTEXT,
        );
        if result == egl::FALSE {
            let err = egl.GetError().to_windowing_api_error();
            return Err(Error::MakeCurrentFailed(err));
        }
        Ok(())
    })
}

pub(crate) unsafe fn get_config_attr(
    egl_display: EGLDisplay,
    egl_config: EGLConfig,
    attr: EGLint,
) -> EGLint {
    try_get_config_attr(egl_display, egl_config, attr).expect("Could not query EGL configuration")
}

pub(crate) unsafe fn try_get_config_attr(
    egl_display: EGLDisplay,
    egl_config: EGLConfig,
    attr: EGLint,
) -> Result<EGLint, Error> {
    EGL_FUNCTIONS.with(|egl| {
        let mut value = 0;
        if egl.GetConfigAttrib(egl_display, egl_config, attr, &mut value) == egl::FALSE {
            return Err(Error::PixelFormatSelectionFailed(
                egl.GetError().to_windowing_api_error(),
            ));
        }
        Ok(value)
    })
}

unsafe fn capture_pixel_format_flags(
    egl_display: EGLDisplay,
    config: EGLConfig,
) -> Result<ContextAttributeFlags, Error> {
    let mut flags = ContextAttributeFlags::empty();
    flags.set(
        ContextAttributeFlags::ALPHA,
        try_get_config_attr(egl_display, config, egl::ALPHA_SIZE as EGLint)? != 0,
    );
    flags.set(
        ContextAttributeFlags::DEPTH,
        try_get_config_attr(egl_display, config, egl::DEPTH_SIZE as EGLint)? != 0,
    );
    flags.set(
        ContextAttributeFlags::STENCIL,
        try_get_config_attr(egl_display, config, egl::STENCIL_SIZE as EGLint)? != 0,
    );
    Ok(flags)
}

pub(crate) unsafe fn get_context_attr(
    egl_display: EGLDisplay,
    egl_context: EGLContext,
    attr: EGLint,
) -> EGLint {
    try_get_context_attr(egl_display, egl_context, attr).expect("Could not query EGL context")
}

pub(crate) unsafe fn try_get_context_attr(
    egl_display: EGLDisplay,
    egl_context: EGLContext,
    attr: EGLint,
) -> Result<EGLint, Error> {
    EGL_FUNCTIONS.with(|egl| {
        let mut value = 0;
        if egl.QueryContext(egl_display, egl_context, attr, &mut value) == egl::FALSE {
            return Err(Error::ContextCreationFailed(
                egl.GetError().to_windowing_api_error(),
            ));
        }
        Ok(value)
    })
}

pub(crate) unsafe fn egl_config_from_id(
    egl_display: EGLDisplay,
    egl_config_id: EGLint,
) -> EGLConfig {
    try_egl_config_from_id(egl_display, egl_config_id).expect("Could not find EGL configuration")
}

pub(crate) unsafe fn try_egl_config_from_id(
    egl_display: EGLDisplay,
    egl_config_id: EGLint,
) -> Result<EGLConfig, Error> {
    let config_attributes = [
        egl::CONFIG_ID as EGLint,
        egl_config_id,
        egl::NONE as EGLint,
        0,
        0,
        0,
    ];
    EGL_FUNCTIONS.with(|egl| {
        let (mut config, mut count) = (ptr::null(), 0);
        if egl.ChooseConfig(
            egl_display,
            config_attributes.as_ptr(),
            &mut config,
            1,
            &mut count,
        ) == egl::FALSE
        {
            return Err(Error::PixelFormatSelectionFailed(
                egl.GetError().to_windowing_api_error(),
            ));
        }
        if count == 0 || config.is_null() {
            return Err(Error::NoPixelFormatFound);
        }
        Ok(config)
    })
}

pub(crate) fn get_proc_address(symbol_name: &str) -> *const c_void {
    EGL_FUNCTIONS.with(|egl| unsafe {
        let symbol_name: CString = CString::new(symbol_name).unwrap();
        egl.GetProcAddress(symbol_name.as_ptr()).cast()
    })
}

// Creates and returns a dummy pbuffer surface for the given context. This is used as the default
// framebuffer on some backends.
#[allow(dead_code)]
pub(crate) unsafe fn create_dummy_pbuffer(
    egl_display: EGLDisplay,
    egl_context: EGLContext,
) -> Option<EGLSurface> {
    let id = try_get_context_attr(egl_display, egl_context, egl::CONFIG_ID as EGLint).ok()?;
    try_create_dummy_pbuffer(egl_display, id).ok().flatten()
}

unsafe fn try_create_dummy_pbuffer(
    egl_display: EGLDisplay,
    config_id: EGLint,
) -> Result<Option<EGLSurface>, Error> {
    if config_id == 0 {
        return Ok(None);
    }
    let config = try_egl_config_from_id(egl_display, config_id)?;
    let attributes = [
        egl::WIDTH as EGLint,
        DUMMY_PBUFFER_SIZE,
        egl::HEIGHT as EGLint,
        DUMMY_PBUFFER_SIZE,
        egl::NONE as EGLint,
        0,
        0,
        0,
    ];
    EGL_FUNCTIONS.with(|egl| {
        let pbuffer = egl.CreatePbufferSurface(egl_display, config, attributes.as_ptr());
        if pbuffer != egl::NO_SURFACE {
            return Ok(Some(pbuffer));
        }
        let error = egl.GetError();
        // A window-only config may legitimately need a surfaceless binding.
        // Do not silently use that fallback for loss, display failure or OOM.
        if matches!(error as u32, egl::BAD_MATCH | egl::BAD_CONFIG) {
            Ok(None)
        } else {
            Err(Error::SurfaceCreationFailed(error.to_windowing_api_error()))
        }
    })
}
