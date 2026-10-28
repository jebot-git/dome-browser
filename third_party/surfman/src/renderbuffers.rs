//! A utility module for backends that wrap surfaces in OpenGL FBOs.

use crate::Gl;
use crate::context::{ContextAttributeFlags, ContextAttributes};
use crate::gl;
use std::thread;

use euclid::default::Size2D;
use gl::Renderbuffer;
use glow::HasContext;

pub(crate) enum Renderbuffers {
    IndividualDepthStencil {
        depth: Option<Renderbuffer>,
        stencil: Option<Renderbuffer>,
    },
    CombinedDepthStencil(Option<Renderbuffer>),
}

impl Drop for Renderbuffers {
    fn drop(&mut self) {
        match *self {
            Renderbuffers::IndividualDepthStencil {
                depth: None,
                stencil: None,
            }
            | Renderbuffers::CombinedDepthStencil(None) => {},
            _ => {
                if !thread::panicking() {
                    panic!("Should have destroyed the FBO renderbuffers with `destroy()`!")
                }
            },
        }
    }
}

impl Renderbuffers {
    pub(crate) fn empty() -> Self {
        Self::IndividualDepthStencil {
            depth: None,
            stencil: None,
        }
    }

    pub(crate) fn new(
        gl: &Gl,
        size: &Size2D<i32>,
        attributes: &ContextAttributes,
    ) -> Result<Self, crate::Error> {
        let depth = attributes.flags.contains(ContextAttributeFlags::DEPTH);
        let stencil = attributes.flags.contains(ContextAttributeFlags::STENCIL);
        if !depth && !stencil {
            return Ok(Self::empty());
        }
        let error = |message: &str| crate::Error::GLResourceAllocationFailed(message.to_owned());
        if size.width <= 0 || size.height <= 0 {
            return Err(error("Invalid renderbuffer size"));
        }
        if !gl.supports_renderbuffer_allocation() {
            return Err(error("Renderbuffer entry points unavailable"));
        }
        let packed = if gl.version().is_embedded {
            gl.version().major >= 3
                || gl
                    .supported_extensions()
                    .contains("GL_OES_packed_depth_stencil")
        } else {
            gl.version().major >= 3
                || gl
                    .supported_extensions()
                    .contains("GL_ARB_framebuffer_object")
                || gl
                    .supported_extensions()
                    .contains("GL_EXT_packed_depth_stencil")
        };
        let mut buffers = Self::empty();
        unsafe {
            let saved = gl.get_parameter_renderbuffer(gl::RENDERBUFFER_BINDING);
            let storage = |format| -> Result<(), crate::Error> {
                gl.renderbuffer_storage(gl::RENDERBUFFER, format, size.width, size.height);
                if gl.get_renderbuffer_parameter_i32(gl::RENDERBUFFER, gl::RENDERBUFFER_WIDTH)
                    != size.width
                    || gl.get_renderbuffer_parameter_i32(gl::RENDERBUFFER, gl::RENDERBUFFER_HEIGHT)
                        != size.height
                {
                    return Err(error("Renderbuffer storage unavailable"));
                }
                Ok(())
            };
            let operation = (|| {
                if depth && stencil && packed {
                    let name = gl
                        .create_renderbuffer()
                        .map_err(crate::Error::GLResourceAllocationFailed)?;
                    buffers = Self::CombinedDepthStencil(Some(name));
                    gl.bind_renderbuffer(gl::RENDERBUFFER, Some(name));
                    storage(gl::DEPTH24_STENCIL8)?;
                } else {
                    let Self::IndividualDepthStencil {
                        depth: depth_name,
                        stencil: stencil_name,
                    } = &mut buffers
                    else {
                        unreachable!()
                    };
                    if depth {
                        let name = gl
                            .create_renderbuffer()
                            .map_err(crate::Error::GLResourceAllocationFailed)?;
                        *depth_name = Some(name);
                        gl.bind_renderbuffer(gl::RENDERBUFFER, Some(name));
                        let depth24 = !gl.version().is_embedded
                            || gl.version().major >= 3
                            || gl.supported_extensions().contains("GL_OES_depth24");
                        storage(if depth24 {
                            gl::DEPTH_COMPONENT24
                        } else {
                            gl::DEPTH_COMPONENT16
                        })?;
                    }
                    if stencil {
                        let name = gl
                            .create_renderbuffer()
                            .map_err(crate::Error::GLResourceAllocationFailed)?;
                        *stencil_name = Some(name);
                        gl.bind_renderbuffer(gl::RENDERBUFFER, Some(name));
                        storage(gl::STENCIL_INDEX8)?;
                    }
                }
                Ok(())
            })();
            if operation.is_err() {
                buffers.destroy(gl);
            }
            gl.bind_renderbuffer(gl::RENDERBUFFER, saved);
            operation.map(|()| buffers)
        }
    }

    // Compatibility for platform-specific allocation transactions not yet
    // migrated to the fallible API. Linux EGL callers propagate `new` errors.
    #[allow(dead_code)]
    pub(crate) fn new_unchecked(
        gl: &Gl,
        size: &Size2D<i32>,
        attributes: &ContextAttributes,
    ) -> Self {
        Self::new(gl, size, attributes).expect("Renderbuffer allocation failed")
    }

    pub(crate) fn bind_to_current_framebuffer(&self, gl: &Gl) {
        unsafe {
            match *self {
                Renderbuffers::CombinedDepthStencil(renderbuffer) => {
                    if renderbuffer.is_some() {
                        // Separate attachment points also work on GLES 2 with
                        // OES_packed_depth_stencil, unlike DEPTH_STENCIL_ATTACHMENT.
                        for point in [gl::DEPTH_ATTACHMENT, gl::STENCIL_ATTACHMENT] {
                            gl.framebuffer_renderbuffer(
                                gl::FRAMEBUFFER,
                                point,
                                gl::RENDERBUFFER,
                                renderbuffer,
                            );
                        }
                    }
                },
                Renderbuffers::IndividualDepthStencil {
                    depth: depth_renderbuffer,
                    stencil: stencil_renderbuffer,
                } => {
                    if depth_renderbuffer.is_some() {
                        gl.framebuffer_renderbuffer(
                            gl::FRAMEBUFFER,
                            gl::DEPTH_ATTACHMENT,
                            gl::RENDERBUFFER,
                            depth_renderbuffer,
                        );
                    }
                    if stencil_renderbuffer.is_some() {
                        gl.framebuffer_renderbuffer(
                            gl::FRAMEBUFFER,
                            gl::STENCIL_ATTACHMENT,
                            gl::RENDERBUFFER,
                            stencil_renderbuffer,
                        );
                    }
                },
            }
        }
    }

    // GL names belong to a retired share group. EGL context destruction releases
    // their storage; issuing GL delete commands after reset is not required.
    pub(crate) fn forget_after_loss(&mut self) {
        match self {
            Self::CombinedDepthStencil(value) => *value = None,
            Self::IndividualDepthStencil { depth, stencil } => {
                *depth = None;
                *stencil = None;
            },
        }
    }

    pub(crate) fn destroy(&mut self, gl: &Gl) {
        if matches!(
            self,
            Self::CombinedDepthStencil(None)
                | Self::IndividualDepthStencil {
                    depth: None,
                    stencil: None
                }
        ) {
            return;
        }
        unsafe {
            gl.bind_renderbuffer(gl::RENDERBUFFER, None);

            match *self {
                Renderbuffers::CombinedDepthStencil(ref mut renderbuffer) => {
                    if let Some(renderbuffer) = renderbuffer.take() {
                        gl.delete_renderbuffer(renderbuffer);
                    }
                },
                Renderbuffers::IndividualDepthStencil {
                    depth: ref mut depth_renderbuffer,
                    stencil: ref mut stencil_renderbuffer,
                } => {
                    if let Some(stencil_renderbuffer) = stencil_renderbuffer.take() {
                        gl.delete_renderbuffer(stencil_renderbuffer);
                    }
                    if let Some(depth_renderbuffer) = depth_renderbuffer.take() {
                        gl.delete_renderbuffer(depth_renderbuffer);
                    }
                },
            }
        }
    }
}

#[cfg(test)]
#[path = "renderbuffers_allocation_tests.rs"]
pub(crate) mod allocation_tests;
