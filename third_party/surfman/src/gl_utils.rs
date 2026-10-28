//! Various OpenGL utilities used by the different backends.

use glow::{HasContext, NativeFramebuffer};

use crate::Gl;
use crate::gl;

#[allow(dead_code)]
pub(crate) fn create_and_bind_framebuffer(
    gl: &Gl,
    texture_target: u32,
    texture_object: Option<glow::NativeTexture>,
) -> NativeFramebuffer {
    try_create_and_bind_framebuffer(gl, texture_target, texture_object)
        .expect("Framebuffer allocation failed")
}

pub(crate) fn try_create_and_bind_framebuffer(
    gl: &Gl,
    texture_target: u32,
    texture_object: Option<glow::NativeTexture>,
) -> Result<NativeFramebuffer, crate::Error> {
    unsafe {
        let framebuffer = gl
            .create_framebuffer()
            .map_err(crate::Error::GLResourceAllocationFailed)?;
        gl.bind_framebuffer(gl::FRAMEBUFFER, Some(framebuffer));
        gl.framebuffer_texture_2d(
            gl::FRAMEBUFFER,
            gl::COLOR_ATTACHMENT0,
            texture_target,
            texture_object,
            0,
        );
        Ok(framebuffer)
    }
}

pub(crate) fn unbind_framebuffer_if_necessary(gl: &Gl, framebuffer_object: NativeFramebuffer) {
    unsafe {
        // Unbind the framebuffer if it's bound.
        let current_draw_framebuffer = gl.get_parameter_framebuffer(gl::DRAW_FRAMEBUFFER_BINDING);
        let current_read_framebuffer = gl.get_parameter_framebuffer(gl::READ_FRAMEBUFFER_BINDING);
        if current_draw_framebuffer == Some(framebuffer_object) {
            gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, None);
        }
        if current_read_framebuffer == Some(framebuffer_object) {
            gl.bind_framebuffer(gl::READ_FRAMEBUFFER, None);
        }
    }
}

#[allow(dead_code)]
pub(crate) fn destroy_framebuffer(gl: &Gl, framebuffer_object: NativeFramebuffer) {
    unbind_framebuffer_if_necessary(gl, framebuffer_object);
    unsafe {
        gl.delete_framebuffer(framebuffer_object);
    }
}
