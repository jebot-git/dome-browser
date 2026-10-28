// SPDX-License-Identifier: MPL-2.0
//! Offscreen GL names stay with their owner context through allocation and retirement.
use super::{Error, RenderingContext};
use dpi::PhysicalSize;
use gleam::gl::{self, Gl};
use image::RgbaImage;
use log::{trace, warn};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
    sync::{Arc, Mutex, Weak as SyncWeak},
    thread::{self, ThreadId},
};
use webrender_api::units::DeviceIntRect;

type CallbackContexts = HashMap<usize, (SyncWeak<CallbackLease>, Weak<dyn RenderingContext>)>;
thread_local! { static CALLBACK_CONTEXTS: RefCell<CallbackContexts> = RefCell::new(HashMap::new()); }
/// Sendable presentation callbacks carry only a lease. Native contexts remain
/// weakly registered on their owner thread and are selected anew for each call.
pub(super) struct CallbackLease {
    thread: ThreadId,
    pub active: Mutex<bool>,
}
impl CallbackLease {
    fn new(context: &Rc<dyn RenderingContext>) -> Result<Arc<Self>, Error> {
        let lease = Arc::new(Self {
            thread: thread::current().id(),
            active: Mutex::new(true),
        });
        CALLBACK_CONTEXTS
            .try_with(|contexts| {
                let mut contexts = contexts.borrow_mut();
                contexts.retain(|_, (lease, context)| {
                    lease.strong_count() != 0 && context.strong_count() != 0
                });
                contexts.insert(
                    Arc::as_ptr(&lease) as usize,
                    (Arc::downgrade(&lease), Rc::downgrade(context)),
                );
            })
            .map_err(|_| Error::Failed)?;
        Ok(lease)
    }
    pub fn context(&self) -> Option<Rc<dyn RenderingContext>> {
        if thread::current().id() != self.thread {
            return None;
        }
        let context = CALLBACK_CONTEXTS
            .try_with(|contexts| {
                contexts
                    .borrow()
                    .get(&(self as *const Self as usize))
                    .and_then(|(_, context)| context.upgrade())
            })
            .ok()
            .flatten()?;
        select(&context).ok()?;
        Some(context)
    }
}
fn select(context: &Rc<dyn RenderingContext>) -> Result<(), Error> {
    let work = || {
        if servo_base::deferred_cleanup::retention_active() || context.native_context_is_lost() {
            return Err(Error::Failed);
        }
        context.make_current()?;
        if context.native_context_is_lost() {
            return Err(Error::Failed);
        }
        Ok(())
    };
    if let Some(scope) = context.cleanup_scope() {
        scope.run(work)
    } else {
        work()
    }
}
fn retire(context: &Rc<dyn RenderingContext>, cleanup: impl FnMut() -> bool + 'static) {
    if let Some(scope) = context.cleanup_scope() {
        scope.run(|| servo_base::deferred_cleanup::retire(cleanup));
    } else {
        servo_base::deferred_cleanup::retire(cleanup);
    }
}
fn failure(label: &str) -> Error {
    Error::GLResourceAllocationFailed(label.to_owned())
}
fn check(gl: &dyn Gl, label: &str) -> Result<(), Error> {
    let error = gl.get_error();
    if error == gl::NO_ERROR {
        Ok(())
    } else {
        Err(failure(&format!("{label}: GL {error:#x}")))
    }
}
#[expect(unsafe_code)]
fn integer(gl: &dyn Gl, name: u32) -> i32 {
    let mut value = [0];
    unsafe {
        gl.get_integer_v(name, &mut value);
    }
    value[0]
}
#[expect(unsafe_code)]
fn zero_attachments(gl: &glow::Context) {
    use glow::HasContext;
    unsafe {
        gl.clear_buffer_f32_slice(gl::COLOR, 0, &[0., 0., 0., 0.]);
        gl.clear_buffer_f32_slice(gl::DEPTH, 0, &[1.]);
    }
}
struct Bindings {
    context: Rc<dyn RenderingContext>,
    gl: Rc<dyn Gl>,
    read: u32,
    draw: u32,
    texture: u32,
    renderbuffer: u32,
    color_mask: [u8; 4],
    depth_mask: bool,
    scissor: bool,
}
impl Bindings {
    #[expect(unsafe_code)]
    fn new(context: Rc<dyn RenderingContext>, gl: Rc<dyn Gl>) -> Self {
        let mut color_mask = [0; 4];
        let mut depth_mask = [0];
        unsafe {
            gl.get_boolean_v(gl::COLOR_WRITEMASK, &mut color_mask);
            gl.get_boolean_v(gl::DEPTH_WRITEMASK, &mut depth_mask);
        }
        Self {
            color_mask,
            depth_mask: depth_mask[0] != 0,
            scissor: gl.is_enabled(gl::SCISSOR_TEST) != 0,
            read: integer(&*gl, gl::READ_FRAMEBUFFER_BINDING) as u32,
            draw: integer(&*gl, gl::DRAW_FRAMEBUFFER_BINDING) as u32,
            texture: integer(&*gl, gl::TEXTURE_BINDING_2D) as u32,
            renderbuffer: integer(&*gl, gl::RENDERBUFFER_BINDING) as u32,
            context,
            gl,
        }
    }
}
impl Drop for Bindings {
    fn drop(&mut self) {
        // If the native context became unavailable, leave restoration to the
        // next renderer bind. Never restore these names into a peer context.
        if select(&self.context).is_err() {
            return;
        }
        self.gl.bind_framebuffer(gl::READ_FRAMEBUFFER, self.read);
        self.gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, self.draw);
        self.gl.bind_texture(gl::TEXTURE_2D, self.texture);
        self.gl
            .bind_renderbuffer(gl::RENDERBUFFER, self.renderbuffer);
    }
}
pub(super) struct Framebuffer {
    context: Rc<dyn RenderingContext>,
    pub gl: Rc<dyn Gl>,
    pub framebuffer_id: u32,
    renderbuffer_id: u32,
    texture_id: u32,
    size: PhysicalSize<u32>,
    pub lease: Arc<CallbackLease>,
}
impl Drop for Framebuffer {
    fn drop(&mut self) {
        *self
            .lease
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = false;
        let context = self.context.clone();
        let owner = context.clone();
        let gl = self.gl.clone();
        let (texture, renderbuffer, framebuffer) =
            (self.texture_id, self.renderbuffer_id, self.framebuffer_id);
        retire(&context, move || {
            if owner.native_context_is_lost() {
                return true;
            }
            if owner.make_current().is_err() {
                return false;
            }
            if !owner.native_context_is_lost() {
                if texture != 0 {
                    gl.delete_textures(&[texture]);
                }
                if renderbuffer != 0 {
                    gl.delete_renderbuffers(&[renderbuffer]);
                }
                if framebuffer != 0 {
                    gl.delete_framebuffers(&[framebuffer]);
                }
            }
            true
        });
    }
}
impl Framebuffer {
    pub fn new(context: Rc<dyn RenderingContext>, size: PhysicalSize<u32>) -> Result<Self, Error> {
        Self::new_with_probe(context, size, |_| Ok(()))
    }
    fn new_with_probe(
        context: Rc<dyn RenderingContext>,
        size: PhysicalSize<u32>,
        mut probe: impl FnMut(&Self) -> Result<(), Error>,
    ) -> Result<Self, Error> {
        if size.width == 0
            || size.height == 0
            || size.width > i32::MAX as u32
            || size.height > i32::MAX as u32
        {
            return Err(failure("invalid offscreen dimensions"));
        }
        select(&context)?;
        let gl = context.gleam_gl_api();
        check(&*gl, "offscreen entry state")?;
        let limit =
            integer(&*gl, gl::MAX_TEXTURE_SIZE).min(integer(&*gl, gl::MAX_RENDERBUFFER_SIZE));
        check(&*gl, "offscreen limits")?;
        if limit <= 0 || size.width > limit as u32 || size.height > limit as u32 {
            return Err(failure("offscreen dimensions exceed native limits"));
        }
        let _bindings = Bindings::new(context.clone(), gl.clone());
        let lease = CallbackLease::new(&context)?;
        let mut target = Self {
            context,
            gl,
            framebuffer_id: 0,
            renderbuffer_id: 0,
            texture_id: 0,
            size,
            lease,
        };
        target.framebuffer_id = target
            .gl
            .gen_framebuffers(1)
            .first()
            .copied()
            .filter(|id| *id != 0)
            .ok_or_else(|| failure("offscreen framebuffer name"))?;
        target
            .gl
            .bind_framebuffer(gl::FRAMEBUFFER, target.framebuffer_id);
        probe(&target)?;
        target.texture_id = target
            .gl
            .gen_textures(1)
            .first()
            .copied()
            .filter(|id| *id != 0)
            .ok_or_else(|| failure("offscreen texture name"))?;
        target.gl.bind_texture(gl::TEXTURE_2D, target.texture_id);
        probe(&target)?;
        target.gl.tex_image_2d(
            gl::TEXTURE_2D,
            0,
            gl::RGBA8 as i32,
            size.width as i32,
            size.height as i32,
            0,
            gl::RGBA,
            gl::UNSIGNED_BYTE,
            None,
        );
        target
            .gl
            .tex_parameter_i(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
        target
            .gl
            .tex_parameter_i(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
        target.gl.framebuffer_texture_2d(
            gl::FRAMEBUFFER,
            gl::COLOR_ATTACHMENT0,
            gl::TEXTURE_2D,
            target.texture_id,
            0,
        );
        target.renderbuffer_id = target
            .gl
            .gen_renderbuffers(1)
            .first()
            .copied()
            .filter(|id| *id != 0)
            .ok_or_else(|| failure("offscreen depth name"))?;
        target
            .gl
            .bind_renderbuffer(gl::RENDERBUFFER, target.renderbuffer_id);
        probe(&target)?;
        target.gl.renderbuffer_storage(
            gl::RENDERBUFFER,
            gl::DEPTH_COMPONENT24,
            size.width as i32,
            size.height as i32,
        );
        target.gl.framebuffer_renderbuffer(
            gl::FRAMEBUFFER,
            gl::DEPTH_ATTACHMENT,
            gl::RENDERBUFFER,
            target.renderbuffer_id,
        );
        check(&*target.gl, "offscreen storage")?;
        if target.gl.check_frame_buffer_status(gl::FRAMEBUFFER) != gl::FRAMEBUFFER_COMPLETE {
            return Err(failure("offscreen framebuffer incomplete"));
        }
        // New storage is defined even when presented before its first frame.
        target.gl.disable(gl::SCISSOR_TEST);
        target.gl.color_mask(true, true, true, true);
        target.gl.depth_mask(true);
        zero_attachments(&target.context.glow_gl_api());
        check(&*target.gl, "offscreen initial contents")?;
        probe(&target)?;
        Ok(target)
    }
    pub fn bind(&self) -> Result<(), Error> {
        select(&self.context)?;
        trace!("Binding FBO {}", self.framebuffer_id);
        self.gl
            .bind_framebuffer(gl::FRAMEBUFFER, self.framebuffer_id);
        Ok(())
    }
    pub fn read_to_image(&self, rect: DeviceIntRect) -> Option<RgbaImage> {
        if rect.min.x < 0
            || rect.min.y < 0
            || rect.max.x > self.size.width as i32
            || rect.max.y > self.size.height as i32
        {
            return None;
        }
        select(&self.context).ok()?;
        Self::read_framebuffer_to_image(&self.gl, self.framebuffer_id, rect)
    }
    pub(super) fn read_framebuffer_to_image(
        gl: &Rc<dyn Gl>,
        framebuffer_id: u32,
        source_rectangle: DeviceIntRect,
    ) -> Option<RgbaImage> {
        if source_rectangle.min.x < 0
            || source_rectangle.min.y < 0
            || source_rectangle.max.x.checked_sub(source_rectangle.min.x)? <= 0
            || source_rectangle.max.y.checked_sub(source_rectangle.min.y)? <= 0
        {
            return None;
        }
        gl.bind_framebuffer(gl::FRAMEBUFFER, framebuffer_id);

        // For some reason, OSMesa fails to render on the 3rd
        // attempt in headless mode, under some conditions.
        // I think this can only be some kind of synchronization
        // bug in OSMesa, but explicitly un-binding any vertex
        // array here seems to work around that bug.
        // See https://github.com/servo/servo/issues/18606.
        gl.bind_vertex_array(0);

        let mut pixels = gl.read_pixels(
            source_rectangle.min.x,
            source_rectangle.min.y,
            source_rectangle.width(),
            source_rectangle.height(),
            gl::RGBA,
            gl::UNSIGNED_BYTE,
        );
        let gl_error = gl.get_error();
        if gl_error != gl::NO_ERROR {
            warn!("GL error code 0x{gl_error:x} set after read_pixels");
        }

        // flip image vertically (texture is upside down)
        let source_rectangle = source_rectangle.to_usize();
        let stride = source_rectangle.width() * 4;
        let height = source_rectangle.height();
        for y in 0..height / 2 {
            let (top, bottom) = pixels.split_at_mut((height - y - 1) * stride);
            top[y * stride..(y + 1) * stride].swap_with_slice(&mut bottom[..stride]);
        }

        RgbaImage::from_raw(
            source_rectangle.width() as u32,
            source_rectangle.height() as u32,
            pixels,
        )
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "offscreen_framebuffer_tests.rs"]
mod tests;
