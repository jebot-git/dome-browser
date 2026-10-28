// SPDX-License-Identifier: MPL-2.0
// Production glow dispatch with in-memory GL names; no native driver calls.
use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[derive(Clone, Copy)]
pub(crate) enum SurfaceFailure {
    None,
    TextureName,
    TextureStorage,
    FramebufferName,
    FramebufferIncomplete,
    RenderbufferName,
    RenderbufferStorage,
    MissingFramebufferFunction,
}
#[derive(Default)]
struct Framebuffer {
    texture: u32,
    depth: u32,
    stencil: u32,
}
#[derive(Default)]
struct Driver {
    es2: bool,
    bound: u32,
    names: u32,
    storage: u32,
    fail_name: u32,
    fail_storage: u32,
    alive: HashMap<u32, (i32, i32)>,
    deleted: Vec<u32>,
    error_reads: u32,
    unknown: Vec<u32>,
    texture: u32,
    read: u32,
    draw: u32,
    unpack: u32,
    next_texture: u32,
    next_framebuffer: u32,
    textures: HashMap<u32, (i32, i32)>,
    framebuffers: HashMap<u32, Framebuffer>,
    fail_texture: bool,
    fail_texture_storage: bool,
    fail_framebuffer: bool,
    incomplete: bool,
    native_lost: bool,
}
thread_local! { static DRIVER: RefCell<Driver> = RefCell::new(Driver::default()); }
fn driver<T>(operation: impl FnOnce(&mut Driver) -> T) -> T {
    DRIVER.with(|d| {
        let mut d = d.borrow_mut();
        assert!(
            !d.native_lost,
            "ordinary GL command after known context loss"
        );
        operation(&mut d)
    })
}
unsafe extern "system" fn string(name: u32) -> *const u8 {
    if name == gl::VERSION {
        driver(|d| {
            if d.es2 {
                c"OpenGL ES 2.0 allocation fixture".as_ptr().cast()
            } else {
                c"3.3 allocation fixture".as_ptr().cast()
            }
        })
    } else {
        c"".as_ptr().cast()
    }
}
unsafe extern "system" fn string_i(_: u32, _: u32) -> *const u8 {
    c"".as_ptr().cast()
}
unsafe extern "system" fn integer(name: u32, out: *mut i32) {
    let value = driver(|d| match name {
        gl::NUM_EXTENSIONS => 0,
        gl::RENDERBUFFER_BINDING => d.bound as i32,
        gl::TEXTURE_BINDING_2D => d.texture as i32,
        gl::DRAW_FRAMEBUFFER_BINDING => d.draw as i32,
        gl::READ_FRAMEBUFFER_BINDING if !d.es2 => d.read as i32,
        gl::PIXEL_UNPACK_BUFFER_BINDING if !d.es2 => d.unpack as i32,
        _ => {
            d.unknown.push(name);
            0
        },
    });
    unsafe { out.write(value) };
}
unsafe extern "system" fn generate(n: i32, out: *mut u32) {
    assert_eq!(n, 1);
    let id = driver(|d| {
        d.names += 1;
        if d.names == d.fail_name {
            0
        } else {
            let id = 100 + d.names;
            d.alive.insert(id, (0, 0));
            id
        }
    });
    unsafe { out.write(id) };
}
unsafe extern "system" fn bind(target: u32, id: u32) {
    assert_eq!(target, gl::RENDERBUFFER);
    driver(|d| d.bound = id);
}
unsafe extern "system" fn storage(target: u32, format: u32, w: i32, h: i32) {
    assert_eq!(target, gl::RENDERBUFFER);
    driver(|d| {
        d.storage += 1;
        let supported = !d.es2 || matches!(format, gl::DEPTH_COMPONENT16 | gl::STENCIL_INDEX8);
        let size = if d.storage == d.fail_storage || !supported {
            (0, 0)
        } else {
            (w, h)
        };
        assert!(d.alive.contains_key(&d.bound));
        d.alive.insert(d.bound, size);
    });
}
unsafe extern "system" fn parameter(target: u32, name: u32, out: *mut i32) {
    assert_eq!(target, gl::RENDERBUFFER);
    let value = driver(|d| {
        let size = d.alive.get(&d.bound).copied().unwrap_or_default();
        match name {
            gl::RENDERBUFFER_WIDTH => size.0,
            gl::RENDERBUFFER_HEIGHT => size.1,
            _ => {
                d.unknown.push(name);
                0
            },
        }
    });
    unsafe { out.write(value) };
}
unsafe extern "system" fn delete(n: i32, ids: *const u32) {
    assert_eq!(n, 1);
    let id = unsafe { *ids };
    driver(|d| {
        assert!(d.alive.remove(&id).is_some());
        d.deleted.push(id);
        if d.bound == id {
            d.bound = 0
        }
    });
}
unsafe extern "system" fn attach(target: u32, point: u32, rb_target: u32, name: u32) {
    assert_eq!((target, rb_target), (gl::FRAMEBUFFER, gl::RENDERBUFFER));
    driver(|d| {
        if let Some(fbo) = d.framebuffers.get_mut(&d.draw) {
            match point {
                gl::DEPTH_ATTACHMENT => fbo.depth = name,
                gl::STENCIL_ATTACHMENT => fbo.stencil = name,
                gl::DEPTH_STENCIL_ATTACHMENT if !d.es2 => {
                    fbo.depth = name;
                    fbo.stencil = name
                },
                _ => d.unknown.push(point),
            }
        }
    });
}
unsafe extern "system" fn gen_texture(n: i32, out: *mut u32) {
    assert_eq!(n, 1);
    let id = driver(|d| {
        d.next_texture += 1;
        if d.fail_texture {
            0
        } else {
            let id = 200 + d.next_texture;
            d.textures.insert(id, (0, 0));
            id
        }
    });
    unsafe { out.write(id) };
}
unsafe extern "system" fn bind_texture(target: u32, id: u32) {
    assert_eq!(target, gl::TEXTURE_2D);
    driver(|d| d.texture = id);
}
unsafe extern "system" fn tex_image(
    target: u32,
    _: i32,
    _: i32,
    w: i32,
    h: i32,
    _: i32,
    _: u32,
    _: u32,
    _: *const c_void,
) {
    assert_eq!(target, gl::TEXTURE_2D);
    driver(|d| {
        assert_eq!(d.unpack, 0);
        assert!(d.textures.contains_key(&d.texture));
        d.textures.insert(
            d.texture,
            if d.fail_texture_storage {
                (0, 0)
            } else {
                (w, h)
            },
        );
    });
}
unsafe extern "system" fn delete_texture(n: i32, ids: *const u32) {
    assert_eq!(n, 1);
    let id = unsafe { *ids };
    driver(|d| {
        assert!(d.textures.remove(&id).is_some());
        if d.texture == id {
            d.texture = 0
        }
    });
}
unsafe extern "system" fn gen_framebuffer(n: i32, out: *mut u32) {
    assert_eq!(n, 1);
    let id = driver(|d| {
        d.next_framebuffer += 1;
        if d.fail_framebuffer {
            0
        } else {
            let id = 300 + d.next_framebuffer;
            d.framebuffers.insert(id, Framebuffer::default());
            id
        }
    });
    unsafe { out.write(id) };
}
unsafe extern "system" fn bind_framebuffer(target: u32, id: u32) {
    driver(|d| match target {
        gl::FRAMEBUFFER => {
            d.read = id;
            d.draw = id
        },
        gl::READ_FRAMEBUFFER if !d.es2 => d.read = id,
        gl::DRAW_FRAMEBUFFER if !d.es2 => d.draw = id,
        _ => d.unknown.push(target),
    });
}
unsafe extern "system" fn attach_texture(
    target: u32,
    point: u32,
    tex_target: u32,
    name: u32,
    _: i32,
) {
    assert_eq!(
        (target, point, tex_target),
        (gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D)
    );
    driver(|d| {
        d.framebuffers.get_mut(&d.draw).unwrap().texture = name;
    });
}
unsafe extern "system" fn status(target: u32) -> u32 {
    assert_eq!(target, gl::FRAMEBUFFER);
    driver(|d| {
        if d.incomplete {
            return gl::FRAMEBUFFER_INCOMPLETE_ATTACHMENT;
        }
        let Some(fbo) = d.framebuffers.get(&d.draw) else {
            return gl::FRAMEBUFFER_UNDEFINED;
        };
        let size = d.textures.get(&fbo.texture).copied().unwrap_or_default();
        if size.0 <= 0
            || size.1 <= 0
            || [fbo.depth, fbo.stencil]
                .iter()
                .any(|id| *id != 0 && d.alive.get(id) != Some(&size))
        {
            gl::FRAMEBUFFER_INCOMPLETE_ATTACHMENT
        } else {
            gl::FRAMEBUFFER_COMPLETE
        }
    })
}
unsafe extern "system" fn delete_framebuffer(n: i32, ids: *const u32) {
    assert_eq!(n, 1);
    let id = unsafe { *ids };
    driver(|d| {
        assert!(d.framebuffers.remove(&id).is_some());
        if d.read == id {
            d.read = 0
        }
        if d.draw == id {
            d.draw = 0
        }
    });
}
unsafe extern "system" fn bind_buffer(target: u32, id: u32) {
    assert_eq!(target, gl::PIXEL_UNPACK_BUFFER);
    driver(|d| {
        assert!(!d.es2);
        d.unpack = id
    });
}
unsafe extern "system" fn error() -> u32 {
    driver(|d| d.error_reads += 1);
    gl::INVALID_ENUM
}
fn load(missing: Option<&str>) -> Gl {
    unsafe {
        Gl::try_from_loader_function(|name| {
            if missing == Some(name) {
                return std::ptr::null();
            }
            match name {
                "glGetString" => string as *const c_void,
                "glGetStringi" => string_i as *const c_void,
                "glGetIntegerv" => integer as *const c_void,
                "glGenRenderbuffers" => generate as *const c_void,
                "glBindRenderbuffer" => bind as *const c_void,
                "glRenderbufferStorage" => storage as *const c_void,
                "glGetRenderbufferParameteriv" => parameter as *const c_void,
                "glDeleteRenderbuffers" => delete as *const c_void,
                "glFramebufferRenderbuffer" => attach as *const c_void,
                "glGenTextures" => gen_texture as *const c_void,
                "glBindTexture" => bind_texture as *const c_void,
                "glTexImage2D" => tex_image as *const c_void,
                "glDeleteTextures" => delete_texture as *const c_void,
                "glGenFramebuffers" => gen_framebuffer as *const c_void,
                "glBindFramebuffer" => bind_framebuffer as *const c_void,
                "glFramebufferTexture2D" => attach_texture as *const c_void,
                "glCheckFramebufferStatus" => status as *const c_void,
                "glDeleteFramebuffers" => delete_framebuffer as *const c_void,
                "glBindBuffer" => bind_buffer as *const c_void,
                "glGetError" => error as *const c_void,
                _ => std::ptr::null(),
            }
        })
        .unwrap()
    }
}
fn initialize(es2: bool, fail_name: u32, fail_storage: u32) {
    DRIVER.with(|d| {
        *d.borrow_mut() = Driver {
            es2,
            bound: 7,
            texture: 11,
            read: if es2 { 23 } else { 19 },
            draw: 23,
            unpack: if es2 { 0 } else { 5 },
            fail_name,
            fail_storage,
            ..Driver::default()
        }
    });
}
fn context(es2: bool, fail_name: u32, fail_storage: u32) -> Gl {
    initialize(es2, fail_name, fail_storage);
    load(None)
}
pub(crate) fn surface_context(es2: bool, failure: SurfaceFailure) -> Gl {
    initialize(es2, 0, 0);
    DRIVER.with(|d| {
        let mut d = d.borrow_mut();
        match failure {
            SurfaceFailure::TextureName => d.fail_texture = true,
            SurfaceFailure::TextureStorage => d.fail_texture_storage = true,
            SurfaceFailure::FramebufferName => d.fail_framebuffer = true,
            SurfaceFailure::FramebufferIncomplete => d.incomplete = true,
            SurfaceFailure::RenderbufferName => d.fail_name = 1,
            SurfaceFailure::RenderbufferStorage => d.fail_storage = 1,
            _ => {},
        }
    });
    load(
        if matches!(failure, SurfaceFailure::MissingFramebufferFunction) {
            Some("glGenFramebuffers")
        } else {
            None
        },
    )
}
pub(crate) fn mark_native_loss() {
    DRIVER.with(|d| d.borrow_mut().native_lost = true);
}
pub(crate) fn assert_loss_deferred() {
    DRIVER.with(|d| {
        let mut d = d.borrow_mut();
        assert!(d.native_lost);
        assert!(!d.textures.is_empty());
        assert!(!d.framebuffers.is_empty());
        assert_eq!(d.error_reads, 0);
        d.alive.clear();
        d.framebuffers.clear();
        d.textures.clear();
    });
}
pub(crate) fn assert_surface_clean() {
    DRIVER.with(|d| {
        let d = d.borrow();
        assert!(
            d.alive.is_empty() && d.textures.is_empty() && d.framebuffers.is_empty(),
            "partial GL names leaked"
        );
        assert_eq!(d.bound, 7);
        assert_eq!(d.texture, 11);
        assert_eq!(d.draw, 23);
        assert_eq!(d.read, if d.es2 { 23 } else { 19 });
        assert_eq!(d.unpack, if d.es2 { 0 } else { 5 });
        assert_eq!(d.error_reads, 0);
        assert!(
            d.unknown.is_empty(),
            "unmodelled query/target: {:?}",
            d.unknown
        );
    });
}
fn attributes(flags: ContextAttributeFlags) -> ContextAttributes {
    ContextAttributes {
        version: crate::GLVersion::new(3, 3),
        flags,
    }
}
trait AllocationOutcome {
    fn rejected(self, gl: &Gl) -> bool;
}
impl AllocationOutcome for Renderbuffers {
    fn rejected(mut self, gl: &Gl) -> bool {
        self.destroy(gl);
        false
    }
}
impl AllocationOutcome for Result<Renderbuffers, crate::Error> {
    fn rejected(self, gl: &Gl) -> bool {
        match self {
            Err(_) => true,
            Ok(mut value) => {
                value.destroy(gl);
                false
            },
        }
    }
}
fn rejected(gl: &Gl, flags: ContextAttributeFlags) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        Renderbuffers::new(gl, &Size2D::new(8, 8), &attributes(flags)).rejected(gl)
    }))
    .unwrap_or(false)
}
fn clean_and_preserved() {
    DRIVER.with(|d| {
        let d = d.borrow();
        assert!(d.alive.is_empty());
        assert_eq!(d.bound, 7);
        assert_eq!(d.error_reads, 0);
        assert!(d.unknown.is_empty());
    });
}
#[test]
fn allocation_renderbuffer_control_allocates_and_releases_names() {
    let gl = context(false, 0, 0);
    assert!(!rejected(&gl, ContextAttributeFlags::DEPTH));
    DRIVER.with(|d| {
        let d = d.borrow();
        assert!(d.alive.is_empty());
        assert_eq!(d.deleted.len(), 1);
        assert_eq!(d.error_reads, 0)
    });
}
#[test]
fn allocation_renderbuffer_name_failure_is_fallible() {
    let gl = context(false, 1, 0);
    assert!(rejected(&gl, ContextAttributeFlags::DEPTH));
    clean_and_preserved();
}
#[test]
fn allocation_renderbuffer_storage_failure_is_fallible() {
    let gl = context(false, 0, 1);
    assert!(rejected(&gl, ContextAttributeFlags::DEPTH));
    clean_and_preserved();
}
#[test]
fn allocation_second_renderbuffer_failure_releases_the_first() {
    let gl = context(true, 2, 0);
    assert!(rejected(
        &gl,
        ContextAttributeFlags::DEPTH | ContextAttributeFlags::STENCIL
    ));
    clean_and_preserved();
    DRIVER.with(|d| assert_eq!(d.borrow().names, 2));
}
#[test]
fn allocation_no_renderbuffers_preserves_binding() {
    let gl = context(false, 0, 0);
    assert!(!rejected(&gl, ContextAttributeFlags::empty()));
    clean_and_preserved();
}
#[test]
fn allocation_missing_renderbuffer_entry_point_is_fallible() {
    initialize(false, 0, 0);
    let gl = load(Some("glGenRenderbuffers"));
    assert!(rejected(&gl, ContextAttributeFlags::DEPTH));
    clean_and_preserved();
}
