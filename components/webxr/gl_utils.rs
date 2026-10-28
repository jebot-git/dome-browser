/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;

use glow as gl;
use glow::{Context as Gl, HasContext};
use webxr_api::{ContextId, Error, GLContexts, LayerId};

use crate::SurfmanGL;

/// RGB storage for layers without alpha. Native swapchain surfaces may still
/// require RGBA, so render into RGB and copy to the presentation surface.
pub(crate) struct LayerColorBuffer(Option<gl::NativeTexture>);

impl LayerColorBuffer {
    pub(crate) fn new(gl: &Gl, target: u32, size: (i32, i32), alpha: bool) -> Result<Self, Error> {
        if alpha {
            return Ok(Self(None));
        }
        let texture = create_layer_texture(gl, target, size, gl::RGB)?;
        Ok(Self(Some(texture)))
    }

    pub(crate) fn texture(&self, native: Option<gl::NativeTexture>) -> Option<gl::NativeTexture> {
        self.0.or(native)
    }

    pub(crate) fn present(
        &self,
        gl: &Gl,
        clearer: &mut GlClearer,
        layer_id: LayerId,
        native: Option<gl::NativeTexture>,
        target: u32,
        size: (i32, i32),
    ) -> Result<(), Error> {
        if let Some(rgb) = self.0 {
            let native = native.ok_or(Error::NoMatchingDevice)?;
            clearer.copy_color(gl, layer_id, rgb, native, target, size)?;
        }
        Ok(())
    }

    pub(crate) fn destroy(&self, gl: &Gl) {
        if let Some(texture) = self.0 {
            unsafe { gl.delete_texture(texture) };
        }
    }
}

pub(crate) fn create_depth_stencil(gl: &Gl, size: (i32, i32)) -> Result<gl::NativeTexture, Error> {
    create_layer_texture(gl, gl::TEXTURE_2D, size, gl::DEPTH_STENCIL)
}

fn create_layer_texture(
    gl: &Gl,
    target: u32,
    size: (i32, i32),
    format: u32,
) -> Result<gl::NativeTexture, Error> {
    let texture = unsafe { gl.create_texture() }.map_err(Error::BackendSpecific)?;
    unsafe {
        let binding = if target == gl::TEXTURE_RECTANGLE {
            gl::TEXTURE_BINDING_RECTANGLE
        } else {
            gl::TEXTURE_BINDING_2D
        };
        let saved = gl.get_parameter_texture(binding);
        // A null upload pointer means an offset when a pixel-unpack buffer is
        // bound. Allocate storage without reading the application's PBO.
        let supports_pbo = gl.version().major >= 3;
        let unpack = if supports_pbo {
            gl.get_parameter_buffer(gl::PIXEL_UNPACK_BUFFER_BINDING)
        } else {
            None
        };
        if supports_pbo {
            gl.bind_buffer(gl::PIXEL_UNPACK_BUFFER, None);
        }
        gl.bind_texture(target, Some(texture));
        gl.tex_parameter_i32(target, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
        gl.tex_parameter_i32(target, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
        gl.tex_parameter_i32(target, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(target, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
        let (internal, pixel_type) = if format == gl::DEPTH_STENCIL {
            (gl::DEPTH24_STENCIL8, gl::UNSIGNED_INT_24_8)
        } else {
            (gl::RGB, gl::UNSIGNED_BYTE)
        };
        gl.tex_image_2d(
            target,
            0,
            internal as i32,
            size.0,
            size.1,
            0,
            format,
            pixel_type,
            gl::PixelUnpackData::Slice(None),
        );
        gl.bind_texture(target, saved);
        if supports_pbo {
            gl.bind_buffer(gl::PIXEL_UNPACK_BUFFER, unpack);
        }
        let error = gl.get_error();
        if error != gl::NO_ERROR {
            gl.delete_texture(texture);
            return Err(Error::BackendSpecific(format!(
                "XR layer texture allocation failed: {error:#x}"
            )));
        }
    }
    Ok(texture)
}

// A utility to clear a color texture and optional depth/stencil texture
pub(crate) struct GlClearer {
    fbos: HashMap<
        (
            LayerId,
            Option<gl::NativeTexture>,
            Option<gl::NativeTexture>,
        ),
        Option<gl::NativeFramebuffer>,
    >,
    should_reverse_winding: bool,
}

impl GlClearer {
    pub(crate) fn new(should_reverse_winding: bool) -> GlClearer {
        let fbos = HashMap::new();
        GlClearer {
            fbos,
            should_reverse_winding,
        }
    }

    fn fbo(
        &mut self,
        gl: &Gl,
        layer_id: LayerId,
        color: Option<gl::NativeTexture>,
        color_target: u32,
        depth_stencil: Option<gl::NativeTexture>,
    ) -> Option<gl::NativeFramebuffer> {
        let should_reverse_winding = self.should_reverse_winding;
        *self
            .fbos
            .entry((layer_id, color, depth_stencil))
            .or_insert_with(|| {
                // Save the current GL state
                unsafe {
                    let draw_fbo = gl.get_parameter_framebuffer(gl::DRAW_FRAMEBUFFER_BINDING);
                    let read_fbo = gl.get_parameter_framebuffer(gl::READ_FRAMEBUFFER_BINDING);

                    // Generate and set attachments of a new FBO
                    let fbo = gl.create_framebuffer().ok()?;

                    gl.bind_framebuffer(gl::FRAMEBUFFER, Some(fbo));
                    gl.framebuffer_texture_2d(
                        gl::FRAMEBUFFER,
                        gl::COLOR_ATTACHMENT0,
                        color_target,
                        color,
                        0,
                    );
                    gl.framebuffer_texture_2d(
                        gl::FRAMEBUFFER,
                        gl::DEPTH_STENCIL_ATTACHMENT,
                        gl::TEXTURE_2D,
                        depth_stencil,
                        0,
                    );

                    let complete =
                        gl.check_framebuffer_status(gl::FRAMEBUFFER) == gl::FRAMEBUFFER_COMPLETE;

                    // Necessary if using an OpenXR runtime that does not support mutable FOV,
                    // as flipping the projection matrix necessitates reversing the winding order.
                    if should_reverse_winding {
                        gl.front_face(gl::CW);
                    }

                    // Restore the GL state
                    gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, draw_fbo);
                    gl.bind_framebuffer(gl::READ_FRAMEBUFFER, read_fbo);
                    debug_assert_eq!(gl.get_error(), gl::NO_ERROR);

                    if complete {
                        Some(fbo)
                    } else {
                        gl.delete_framebuffer(fbo);
                        None
                    }
                }
            })
    }

    fn copy_color(
        &mut self,
        gl: &Gl,
        layer_id: LayerId,
        source: gl::NativeTexture,
        destination: gl::NativeTexture,
        target: u32,
        size: (i32, i32),
    ) -> Result<(), Error> {
        let source = self
            .fbo(gl, layer_id, Some(source), target, None)
            .ok_or_else(|| {
                Error::BackendSpecific("RGB layer source framebuffer unavailable".into())
            })?;
        let destination = self
            .fbo(gl, layer_id, Some(destination), target, None)
            .ok_or_else(|| {
                Error::BackendSpecific("RGB layer destination framebuffer unavailable".into())
            })?;
        unsafe {
            let draw = gl.get_parameter_framebuffer(gl::DRAW_FRAMEBUFFER_BINDING);
            let read = gl.get_parameter_framebuffer(gl::READ_FRAMEBUFFER_BINDING);
            let scissor = gl.is_enabled(gl::SCISSOR_TEST);
            let supports_srgb = !gl.version().is_embedded && gl.version().major >= 3;
            let srgb = supports_srgb && gl.is_enabled(gl::FRAMEBUFFER_SRGB);
            gl.bind_framebuffer(gl::READ_FRAMEBUFFER, Some(source));
            gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, Some(destination));
            let complete = gl.check_framebuffer_status(gl::READ_FRAMEBUFFER)
                == gl::FRAMEBUFFER_COMPLETE
                && gl.check_framebuffer_status(gl::DRAW_FRAMEBUFFER) == gl::FRAMEBUFFER_COMPLETE;
            if complete {
                gl.disable(gl::SCISSOR_TEST);
                if supports_srgb {
                    gl.disable(gl::FRAMEBUFFER_SRGB);
                }
                gl.blit_framebuffer(
                    0,
                    0,
                    size.0,
                    size.1,
                    0,
                    0,
                    size.0,
                    size.1,
                    gl::COLOR_BUFFER_BIT,
                    gl::NEAREST,
                );
            }
            gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, draw);
            gl.bind_framebuffer(gl::READ_FRAMEBUFFER, read);
            if scissor {
                gl.enable(gl::SCISSOR_TEST);
            }
            if srgb {
                gl.enable(gl::FRAMEBUFFER_SRGB);
            }
            if !complete {
                return Err(Error::BackendSpecific(
                    "RGB layer presentation framebuffer incomplete".into(),
                ));
            }
        }
        Ok(())
    }

    /// Discard local names after native loss; context destruction owns GL cleanup.
    pub(crate) fn forget_layer(&mut self, layer: LayerId) {
        self.fbos.retain(|(id, _, _), _| *id != layer);
    }

    pub(crate) fn clear(
        &mut self,
        contexts: &mut dyn GLContexts<SurfmanGL>,
        context_id: ContextId,
        layer_id: LayerId,
        color: Option<glow::NativeTexture>,
        color_target: u32,
        depth_stencil: Option<glow::NativeTexture>,
    ) -> Result<(), Error> {
        let gl = contexts
            .bindings(context_id)
            .ok_or(Error::NoMatchingDevice)?;
        let fbo = self
            .fbo(gl, layer_id, color, color_target, depth_stencil)
            .ok_or_else(|| Error::BackendSpecific("XR clear framebuffer unavailable".into()))?;
        unsafe {
            // Save the current GL state
            let mut clear_color = [0., 0., 0., 0.];
            let mut clear_depth = [0.];
            let mut clear_stencil = [0];
            let scissor_enabled = gl.is_enabled(gl::SCISSOR_TEST);
            let rasterizer_enabled = gl.is_enabled(gl::RASTERIZER_DISCARD);

            let draw_fbo = gl.get_parameter_framebuffer(gl::DRAW_FRAMEBUFFER_BINDING);
            let read_fbo = gl.get_parameter_framebuffer(gl::READ_FRAMEBUFFER_BINDING);
            gl.get_parameter_f32_slice(gl::COLOR_CLEAR_VALUE, &mut clear_color[..]);
            gl.get_parameter_f32_slice(gl::DEPTH_CLEAR_VALUE, &mut clear_depth[..]);
            gl.get_parameter_i32_slice(gl::STENCIL_CLEAR_VALUE, &mut clear_stencil[..]);
            let depth_mask = gl.get_parameter_bool(gl::DEPTH_WRITEMASK);
            let stencil_mask = gl.get_parameter_u32(gl::STENCIL_WRITEMASK).ok_or_else(|| {
                Error::BackendSpecific("XR stencil mask query unavailable".into())
            })?;
            let stencil_back_mask = gl
                .get_parameter_u32(gl::STENCIL_BACK_WRITEMASK)
                .ok_or_else(|| {
                    Error::BackendSpecific("XR back stencil mask query unavailable".into())
                })?;
            let color_mask = gl.get_parameter_bool_array::<4>(gl::COLOR_WRITEMASK);

            // Clear it
            gl.bind_framebuffer(gl::FRAMEBUFFER, Some(fbo));
            gl.clear_color(0., 0., 0., 0.);
            gl.clear_depth(1.);
            gl.clear_stencil(0);
            gl.disable(gl::SCISSOR_TEST);
            gl.disable(gl::RASTERIZER_DISCARD);
            gl.depth_mask(true);
            gl.stencil_mask(0xFFFFFFFF);
            gl.color_mask(true, true, true, true);
            gl.clear(gl::COLOR_BUFFER_BIT | gl::DEPTH_BUFFER_BIT | gl::STENCIL_BUFFER_BIT);

            // Restore the GL state
            gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, draw_fbo);
            gl.bind_framebuffer(gl::READ_FRAMEBUFFER, read_fbo);
            gl.clear_color(
                clear_color[0],
                clear_color[1],
                clear_color[2],
                clear_color[3],
            );
            gl.color_mask(color_mask[0], color_mask[1], color_mask[2], color_mask[3]);
            gl.clear_depth(clear_depth[0] as f64);
            gl.clear_stencil(clear_stencil[0]);
            gl.depth_mask(depth_mask);
            gl.stencil_mask_separate(gl::FRONT, stencil_mask);
            gl.stencil_mask_separate(gl::BACK, stencil_back_mask);
            if scissor_enabled {
                gl.enable(gl::SCISSOR_TEST);
            }
            if rasterizer_enabled {
                gl.enable(gl::RASTERIZER_DISCARD);
            }
            debug_assert_eq!(gl.get_error(), gl::NO_ERROR);
        }
        Ok(())
    }

    pub(crate) fn destroy_layer(
        &mut self,
        contexts: &mut dyn GLContexts<SurfmanGL>,
        context_id: ContextId,
        layer_id: LayerId,
    ) {
        let gl = match contexts.bindings(context_id) {
            None => return,
            Some(gl) => gl,
        };
        self.fbos.retain(|&(other_id, _, _), &mut fbo| {
            if layer_id != other_id {
                true
            } else {
                if let Some(fbo) = fbo {
                    unsafe { gl.delete_framebuffer(fbo) };
                }
                false
            }
        })
    }

    /// Imported surface textures are destroyed when surfman recycles them.
    /// Release FBO references before that happens: a later texture with the
    /// same numeric name must not hit an FBO still holding the old GL object.
    pub(crate) fn release_surface(
        &mut self,
        gl: &Gl,
        layer_id: LayerId,
        color: Option<gl::NativeTexture>,
    ) {
        self.fbos.retain(|&(id, texture, _), &mut fbo| {
            if id != layer_id || texture != color {
                return true;
            }
            if let Some(fbo) = fbo {
                unsafe { gl.delete_framebuffer(fbo) };
            }
            false
        });
    }
}
