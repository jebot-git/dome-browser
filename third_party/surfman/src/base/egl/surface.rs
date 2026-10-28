//! Functionality common to backends using EGL surfaces.

use super::context::CurrentContextGuard;
use super::device::EGL_FUNCTIONS;
use crate::Gl;
use crate::base::egl::error::ToWindowingApiError;
use crate::base::egl::ffi::{
    EGL_EXTENSION_FUNCTIONS, EGL_GL_TEXTURE_2D_KHR, EGL_IMAGE_PRESERVED_KHR, EGL_NO_IMAGE_KHR,
    EGLClientBuffer, EGLImageKHR,
};
use crate::egl;
use crate::egl::types::{EGLAttrib, EGLConfig, EGLContext, EGLDisplay, EGLSurface, EGLint};
use crate::gl;
use crate::gl_utils;
use crate::renderbuffers::Renderbuffers;
use crate::{ContextAttributes, ContextID, Error, SurfaceID, SurfaceInfo};

use euclid::default::Size2D;
use glow::{Framebuffer, HasContext, PixelUnpackData, Texture};
use std::fmt::{self, Debug, Formatter};
use std::marker::PhantomData;
use std::mem;
use std::os::raw::c_void;
use std::ptr;

#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct ExternalEGLSurfaces {
    pub(crate) draw: EGLSurface,
    pub(crate) read: EGLSurface,
}

pub struct EGLBackedSurface {
    pub(crate) context_id: ContextID,
    pub(crate) size: Size2D<i32>,
    pub(crate) objects: EGLSurfaceObjects,
    pub(crate) destroyed: bool,
}

impl Debug for EGLBackedSurface {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        write!(f, "Surface({:x})", self.id().0)
    }
}

unsafe impl Send for EGLBackedSurface {}

#[allow(dead_code)]
pub(crate) enum EGLSurfaceObjects {
    TextureImage {
        egl_image: EGLImageKHR,
        framebuffer_object: Option<Framebuffer>,
        texture_object: Option<Texture>,
        renderbuffers: Renderbuffers,
    },
    Window {
        native_window: *const c_void,
        egl_surface: EGLSurface,
    },
}

pub(crate) struct EGLSurfaceTexture {
    pub(crate) surface: EGLBackedSurface,
    pub(crate) texture_object: Option<Texture>,
    pub(crate) phantom: PhantomData<*const ()>,
}

impl Debug for EGLSurfaceTexture {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        write!(f, "SurfaceTexture({:?})", self.surface)
    }
}

// Allocation failure restores all caller bindings. Success retains the
// historical new-surface framebuffer binding, while restoring other state.
struct AllocationBindings<'a> {
    gl: &'a Gl,
    read: Option<Framebuffer>,
    draw: Option<Framebuffer>,
    texture: Option<Texture>,
    renderbuffer: Option<gl::Renderbuffer>,
    unpack: Option<gl::Buffer>,
    has_unpack: bool,
    separate_framebuffers: bool,
    restore: bool,
}
impl<'a> AllocationBindings<'a> {
    unsafe fn save(gl: &'a Gl) -> Self {
        let version = gl.version();
        let separate = version.major >= 3
            || (!version.is_embedded
                && gl
                    .supported_extensions()
                    .contains("GL_ARB_framebuffer_object"));
        let has_unpack = if version.is_embedded {
            version.major >= 3
        } else {
            (version.major, version.minor) >= (2, 1)
                || gl
                    .supported_extensions()
                    .contains("GL_ARB_pixel_buffer_object")
        };
        Self {
            gl,
            read: if separate {
                unsafe { gl.get_parameter_framebuffer(gl::READ_FRAMEBUFFER_BINDING) }
            } else {
                unsafe { gl.get_parameter_framebuffer(gl::FRAMEBUFFER_BINDING) }
            },
            draw: unsafe { gl.get_parameter_framebuffer(gl::FRAMEBUFFER_BINDING) },
            texture: unsafe { gl.get_parameter_texture(gl::TEXTURE_BINDING_2D) },
            renderbuffer: unsafe { gl.get_parameter_renderbuffer(gl::RENDERBUFFER_BINDING) },
            unpack: if has_unpack {
                unsafe { gl.get_parameter_buffer(gl::PIXEL_UNPACK_BUFFER_BINDING) }
            } else {
                None
            },
            has_unpack,
            separate_framebuffers: separate,
            restore: true,
        }
    }
}
impl Drop for AllocationBindings<'_> {
    fn drop(&mut self) {
        if !self.restore {
            return;
        }
        unsafe {
            self.gl.bind_texture(gl::TEXTURE_2D, self.texture);
            if self.has_unpack {
                self.gl.bind_buffer(gl::PIXEL_UNPACK_BUFFER, self.unpack);
            }
            self.gl
                .bind_renderbuffer(gl::RENDERBUFFER, self.renderbuffer);
            if self.separate_framebuffers {
                self.gl.bind_framebuffer(gl::READ_FRAMEBUFFER, self.read);
                self.gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, self.draw);
            } else {
                self.gl.bind_framebuffer(gl::FRAMEBUFFER, self.draw);
            }
        }
    }
}
struct AllocatedGlSurface {
    texture: Option<Texture>,
    framebuffer: Option<Framebuffer>,
    renderbuffers: Renderbuffers,
}
impl AllocatedGlSurface {
    fn destroy(&mut self, gl: &Gl) {
        self.renderbuffers.destroy(gl);
        unsafe {
            if let Some(framebuffer) = self.framebuffer.take() {
                gl.delete_framebuffer(framebuffer);
            }
            if let Some(texture) = self.texture.take() {
                gl.delete_texture(texture);
            }
        }
    }
    fn retire_with_context(&mut self) {
        self.renderbuffers.forget_after_loss();
        self.framebuffer = None;
        self.texture = None;
    }
}

impl EGLBackedSurface {
    pub(crate) fn new_generic(
        gl: &Gl,
        egl_display: EGLDisplay,
        egl_context: EGLContext,
        context_id: ContextID,
        context_attributes: &ContextAttributes,
        size: &Size2D<i32>,
    ) -> Result<EGLBackedSurface, Error> {
        let create_image = EGL_EXTENSION_FUNCTIONS
            .CreateImageKHR
            .ok_or(Error::RequiredExtensionUnavailable)?;
        if EGL_EXTENSION_FUNCTIONS.DestroyImageKHR.is_none()
            || EGL_EXTENSION_FUNCTIONS.ImageTargetTexture2DOES.is_none()
        {
            return Err(Error::RequiredExtensionUnavailable);
        }
        Self::new_generic_with_image(gl, context_id, context_attributes, size, |texture| {
            let attributes = [
                EGL_IMAGE_PRESERVED_KHR as EGLint,
                egl::FALSE as EGLint,
                egl::NONE as EGLint,
            ];
            let image = create_image(
                egl_display,
                egl_context,
                EGL_GL_TEXTURE_2D_KHR,
                texture.0.get() as usize as EGLClientBuffer,
                attributes.as_ptr(),
            );
            if image == EGL_NO_IMAGE_KHR {
                return Err(Error::SurfaceCreationFailed(
                    EGL_FUNCTIONS
                        .with(|egl| unsafe { egl.GetError() })
                        .to_windowing_api_error(),
                ));
            }
            Ok(image)
        })
    }

    // The image is acquired last: no fallible allocation remains after it is
    // created, so every successful EGL image is returned in an owning surface.
    fn new_generic_with_image(
        gl: &Gl,
        context_id: ContextID,
        attributes: &ContextAttributes,
        size: &Size2D<i32>,
        create_image: impl FnOnce(Texture) -> Result<EGLImageKHR, Error>,
    ) -> Result<Self, Error> {
        if size.width <= 0 || size.height <= 0 {
            return Err(Error::GLResourceAllocationFailed(
                "Invalid surface size".into(),
            ));
        }
        if !gl.supports_framebuffer_allocation() {
            return Err(Error::RequiredExtensionUnavailable);
        }
        let mut bindings = unsafe { AllocationBindings::save(gl) };
        let mut objects = AllocatedGlSurface {
            texture: None,
            framebuffer: None,
            renderbuffers: Renderbuffers::empty(),
        };
        let operation = (|| unsafe {
            let texture = gl
                .create_texture()
                .map_err(Error::GLResourceAllocationFailed)?;
            objects.texture = Some(texture);
            gl.bind_texture(gl::TEXTURE_2D, Some(texture));
            if bindings.has_unpack {
                gl.bind_buffer(gl::PIXEL_UNPACK_BUFFER, None);
            }
            gl.tex_image_2d(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                size.width,
                size.height,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                PixelUnpackData::Slice(None),
            );
            objects.framebuffer = Some(gl_utils::try_create_and_bind_framebuffer(
                gl,
                gl::TEXTURE_2D,
                Some(texture),
            )?);
            objects.renderbuffers = Renderbuffers::new(gl, size, attributes)?;
            objects.renderbuffers.bind_to_current_framebuffer(gl);
            if gl.check_framebuffer_status(gl::FRAMEBUFFER) != gl::FRAMEBUFFER_COMPLETE {
                return Err(Error::GLResourceAllocationFailed(
                    "Incomplete surface framebuffer".into(),
                ));
            }
            create_image(texture)
        })();
        let image = match operation {
            Ok(image) => image,
            Err(error) => {
                if error.is_context_lost() {
                    // The caller marks the native context lost before doing any
                    // further work. Its destruction releases these private GL names.
                    bindings.restore = false;
                    objects.retire_with_context();
                } else {
                    objects.destroy(gl);
                }
                return Err(error);
            },
        };
        bindings.read = objects.framebuffer;
        bindings.draw = objects.framebuffer;
        let AllocatedGlSurface {
            texture,
            framebuffer,
            renderbuffers,
        } = objects;
        Ok(Self {
            context_id,
            size: *size,
            objects: EGLSurfaceObjects::TextureImage {
                egl_image: image,
                texture_object: texture,
                framebuffer_object: framebuffer,
                renderbuffers,
            },
            destroyed: false,
        })
    }

    pub(crate) fn new_window(
        egl_display: EGLDisplay,
        egl_config: EGLConfig,
        native_window: *mut c_void,
        context_id: ContextID,
        size: &Size2D<i32>,
    ) -> EGLBackedSurface {
        EGL_FUNCTIONS.with(|egl| unsafe {
            let window_surface_attribs = [egl::NONE as EGLAttrib];
            let egl_surface = egl.CreatePlatformWindowSurface(
                egl_display,
                egl_config,
                native_window,
                window_surface_attribs.as_ptr(),
            );
            assert_ne!(egl_surface, egl::NO_SURFACE);

            EGLBackedSurface {
                context_id,
                size: *size,
                objects: EGLSurfaceObjects::Window {
                    native_window,
                    egl_surface,
                },
                destroyed: false,
            }
        })
    }

    pub(crate) fn to_surface_texture(
        self,
        gl: &Gl,
    ) -> Result<EGLSurfaceTexture, (Error, EGLBackedSurface)> {
        unsafe {
            let egl_image = match self.objects {
                EGLSurfaceObjects::TextureImage { egl_image, .. } => egl_image,
                EGLSurfaceObjects::Window { .. } => return Err((Error::WidgetAttached, self)),
            };
            let texture_object = match try_bind_egl_image_to_gl_texture(gl, egl_image) {
                Ok(texture) => texture,
                Err(error) => return Err((error, self)),
            };
            Ok(EGLSurfaceTexture {
                surface: self,
                texture_object: Some(texture_object),
                phantom: PhantomData,
            })
        }
    }

    pub(crate) fn destroy(
        &mut self,
        gl: &Gl,
        egl_display: EGLDisplay,
        context_id: ContextID,
    ) -> Result<Option<*const c_void>, Error> {
        if context_id != self.context_id {
            return Err(Error::IncompatibleSurface);
        }
        if self.destroyed {
            return Ok(None);
        }

        unsafe {
            match self.objects {
                EGLSurfaceObjects::TextureImage {
                    ref mut egl_image,
                    ref mut framebuffer_object,
                    ref mut texture_object,
                    ref mut renderbuffers,
                } => {
                    gl.bind_framebuffer(gl::FRAMEBUFFER, None);
                    if let Some(framebuffer) = framebuffer_object.take() {
                        gl.delete_framebuffer(framebuffer);
                    }
                    renderbuffers.destroy(gl);

                    let result = (EGL_EXTENSION_FUNCTIONS
                        .DestroyImageKHR
                        .ok_or(Error::RequiredExtensionUnavailable)?)(
                        egl_display, *egl_image
                    );
                    if result == egl::FALSE {
                        let error = EGL_FUNCTIONS.with(|egl| egl.GetError());
                        return Err(Error::SurfaceDestructionFailed(
                            error.to_windowing_api_error(),
                        ));
                    }
                    *egl_image = EGL_NO_IMAGE_KHR;

                    if let Some(texture) = texture_object.take() {
                        gl.delete_texture(texture);
                    }

                    self.destroyed = true;
                    Ok(None)
                },
                EGLSurfaceObjects::Window {
                    ref mut egl_surface,
                    ref mut native_window,
                } => {
                    EGL_FUNCTIONS.with(|egl| {
                        if egl.DestroySurface(egl_display, *egl_surface) == egl::FALSE {
                            return Err(Error::SurfaceDestructionFailed(
                                egl.GetError().to_windowing_api_error(),
                            ));
                        }
                        *egl_surface = egl::NO_SURFACE;
                        Ok(())
                    })?;

                    self.destroyed = true;
                    Ok(Some(mem::replace(native_window, ptr::null())))
                },
            }
        }
    }

    pub(crate) fn destroy_after_loss(
        &mut self,
        egl_display: EGLDisplay,
        context_id: ContextID,
    ) -> Result<Option<*const c_void>, Error> {
        if context_id != self.context_id {
            return Err(Error::IncompatibleSurface);
        }
        if self.destroyed {
            return Ok(None);
        }
        unsafe {
            match &mut self.objects {
                EGLSurfaceObjects::TextureImage {
                    egl_image,
                    framebuffer_object,
                    texture_object,
                    renderbuffers,
                } => {
                    if *egl_image != EGL_NO_IMAGE_KHR {
                        let result = (EGL_EXTENSION_FUNCTIONS
                            .DestroyImageKHR
                            .ok_or(Error::RequiredExtensionUnavailable)?)(
                            egl_display, *egl_image
                        );
                        if result == egl::FALSE {
                            let error = EGL_FUNCTIONS.with(|egl| egl.GetError());
                            if !matches!(
                                error as u32,
                                egl::BAD_DISPLAY | egl::NOT_INITIALIZED | egl::BAD_PARAMETER
                            ) {
                                return Err(Error::SurfaceDestructionFailed(
                                    error.to_windowing_api_error(),
                                ));
                            }
                        }
                        *egl_image = EGL_NO_IMAGE_KHR;
                    }
                    *framebuffer_object = None;
                    *texture_object = None;
                    renderbuffers.forget_after_loss();
                    self.destroyed = true;
                    Ok(None)
                },
                EGLSurfaceObjects::Window {
                    egl_surface,
                    native_window,
                } => {
                    let result =
                        EGL_FUNCTIONS.with(|egl| egl.DestroySurface(egl_display, *egl_surface));
                    if result == egl::FALSE {
                        let error = EGL_FUNCTIONS.with(|egl| egl.GetError());
                        if !matches!(
                            error as u32,
                            egl::BAD_DISPLAY | egl::NOT_INITIALIZED | egl::BAD_SURFACE
                        ) {
                            return Err(Error::SurfaceDestructionFailed(
                                error.to_windowing_api_error(),
                            ));
                        }
                    }
                    *egl_surface = egl::NO_SURFACE;
                    self.destroyed = true;
                    Ok(Some(mem::replace(native_window, ptr::null())))
                },
            }
        }
    }

    // TODO(pcwalton): Damage regions.
    pub(crate) fn present(
        &self,
        egl_display: EGLDisplay,
        egl_context: EGLContext,
    ) -> Result<(), Error> {
        unsafe {
            match self.objects {
                EGLSurfaceObjects::Window { egl_surface, .. } => {
                    // The surface must be bound to the current context in EGL 1.4. Temporarily
                    // make this surface current to enforce this.
                    let _guard = CurrentContextGuard::new();

                    EGL_FUNCTIONS.with(|egl| {
                        let result =
                            egl.MakeCurrent(egl_display, egl_surface, egl_surface, egl_context);
                        if result == egl::FALSE {
                            let err = egl.GetError().to_windowing_api_error();
                            return Err(Error::MakeCurrentFailed(err));
                        }

                        let ok = egl.SwapBuffers(egl_display, egl_surface);
                        if ok != egl::FALSE {
                            Ok(())
                        } else {
                            Err(Error::PresentFailed(
                                egl.GetError().to_windowing_api_error(),
                            ))
                        }
                    })
                },
                EGLSurfaceObjects::TextureImage { .. } => Err(Error::NoWidgetAttached),
            }
        }
    }

    pub(crate) fn info(&self) -> SurfaceInfo {
        SurfaceInfo {
            size: self.size,
            id: self.id(),
            context_id: self.context_id,
            framebuffer_object: match self.objects {
                EGLSurfaceObjects::TextureImage {
                    framebuffer_object, ..
                } => framebuffer_object,
                EGLSurfaceObjects::Window { .. } => None,
            },
        }
    }

    pub(crate) fn id(&self) -> SurfaceID {
        match self.objects {
            EGLSurfaceObjects::TextureImage { egl_image, .. } => SurfaceID(egl_image as usize),
            EGLSurfaceObjects::Window { egl_surface, .. } => SurfaceID(egl_surface as usize),
        }
    }

    pub(crate) fn native_window(&self) -> Result<*const c_void, Error> {
        match self.objects {
            EGLSurfaceObjects::TextureImage { .. } => Err(Error::NoWidgetAttached),
            EGLSurfaceObjects::Window { native_window, .. } => Ok(native_window),
        }
    }

    pub(crate) fn unbind(
        &self,
        gl: &Gl,
        egl_display: EGLDisplay,
        egl_context: EGLContext,
        fallback: EGLSurface,
    ) -> Result<(), Error> {
        // If we're current, we stay current, but with no surface attached.
        unsafe {
            EGL_FUNCTIONS.with(|egl| {
                if egl.GetCurrentContext() != egl_context {
                    return Err(Error::NoCurrentContext);
                }

                if egl.MakeCurrent(egl_display, fallback, fallback, egl_context) == egl::FALSE {
                    return Err(Error::MakeCurrentFailed(
                        egl.GetError().to_windowing_api_error(),
                    ));
                }

                match self.objects {
                    EGLSurfaceObjects::TextureImage {
                        framebuffer_object: Some(framebuffer_object),
                        ..
                    } => {
                        gl_utils::unbind_framebuffer_if_necessary(gl, framebuffer_object);
                    },
                    EGLSurfaceObjects::TextureImage { .. } | EGLSurfaceObjects::Window { .. } => {},
                }
                Ok(())
            })
        }
    }

    pub(crate) fn egl_surfaces(&self) -> ExternalEGLSurfaces {
        match self.objects {
            EGLSurfaceObjects::Window { egl_surface, .. } => ExternalEGLSurfaces {
                draw: egl_surface,
                read: egl_surface,
            },
            EGLSurfaceObjects::TextureImage { .. } => ExternalEGLSurfaces::default(),
        }
    }

    pub(crate) fn resize(&mut self, size: Size2D<i32>) {
        self.size = size;
    }
}

impl EGLSurfaceTexture {
    pub(crate) fn forget_after_loss(mut self) -> EGLBackedSurface {
        self.texture_object = None;
        self.surface
    }

    pub(crate) fn destroy(mut self, gl: &Gl) -> EGLBackedSurface {
        if let Some(texture) = self.texture_object.take() {
            unsafe {
                gl.delete_texture(texture);
            }
        }
        self.surface
    }
}

impl Default for ExternalEGLSurfaces {
    #[inline]
    fn default() -> ExternalEGLSurfaces {
        ExternalEGLSurfaces {
            draw: egl::NO_SURFACE,
            read: egl::NO_SURFACE,
        }
    }
}

#[allow(dead_code)]
pub(crate) unsafe fn create_pbuffer_surface(
    egl_display: EGLDisplay,
    egl_config: EGLConfig,
    size: &Size2D<i32>,
) -> EGLSurface {
    let attributes = [
        egl::WIDTH as EGLint,
        size.width as EGLint,
        egl::HEIGHT as EGLint,
        size.height as EGLint,
        egl::TEXTURE_FORMAT as EGLint,
        egl::TEXTURE_RGBA as EGLint,
        egl::TEXTURE_TARGET as EGLint,
        egl::TEXTURE_2D as EGLint,
        egl::NONE as EGLint,
        0,
        0,
        0,
    ];

    EGL_FUNCTIONS.with(|egl| {
        let egl_surface = egl.CreatePbufferSurface(egl_display, egl_config, attributes.as_ptr());
        assert_ne!(egl_surface, egl::NO_SURFACE);
        egl_surface
    })
}

#[allow(dead_code)]
pub(crate) unsafe fn bind_egl_image_to_gl_texture(gl: &Gl, egl_image: EGLImageKHR) -> Texture {
    unsafe { try_bind_egl_image_to_gl_texture(gl, egl_image) }.expect("EGL image import failed")
}

#[allow(dead_code)]
pub(crate) unsafe fn try_bind_egl_image_to_gl_texture(
    gl: &Gl,
    egl_image: EGLImageKHR,
) -> Result<Texture, Error> {
    let image_target = EGL_EXTENSION_FUNCTIONS
        .ImageTargetTexture2DOES
        .ok_or(Error::RequiredExtensionUnavailable)?;
    let texture = gl
        .create_texture()
        .map_err(Error::GLResourceAllocationFailed)?;

    let texture_binding = gl.get_parameter_texture(gl::TEXTURE_BINDING_2D);

    // FIXME(pcwalton): Should this be `GL_TEXTURE_EXTERNAL_OES`?
    gl.bind_texture(gl::TEXTURE_2D, Some(texture));
    image_target(gl::TEXTURE_2D, egl_image);
    gl.tex_parameter_i32(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as _);
    gl.tex_parameter_i32(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as _);
    gl.tex_parameter_i32(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as _);
    gl.tex_parameter_i32(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as _);
    gl.bind_texture(gl::TEXTURE_2D, texture_binding);

    Ok(texture)
}

#[cfg(test)]
#[path = "surface_allocation_tests.rs"]
mod allocation_tests;
