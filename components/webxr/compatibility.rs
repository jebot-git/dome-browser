/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Check the surface-transfer path without opening an immersive session.

use euclid::default::Size2D;
use glow::HasContext;
use surfman::chains::SwapChains;
use surfman::{
    Adapter, Connection, ContextAttributeFlags, ContextAttributes, GLApi, GLVersion, SurfaceAccess,
    SurfaceType,
};
use webxr_api::{ContextId, Error, GLContexts, LayerGrandManager};

use crate::{SurfmanGL, SurfmanLayerManager};

fn backend(error: impl std::fmt::Debug) -> Error {
    Error::BackendSpecific(format!("XR context surface compatibility: {error:?}"))
}

/// Execute on the WebGL thread, importing a real producer surface into a
/// separate context on the adapter used by the XR compositor backend.
pub fn check(
    manager: &LayerGrandManager<SurfmanGL>,
    context: ContextId,
    connection: Connection,
    adapter: Adapter,
) -> Result<(), Error> {
    let probe = manager.create_layer_manager(move |contexts| {
        check_surface(contexts, context, connection, adapter)?;
        Ok(SurfmanLayerManager::new(SwapChains::new()))
    })?;
    // The empty manager also exercises normal factory teardown. It owns no
    // session, tracking data or persistent layer allocation.
    drop(probe);
    Ok(())
}

// Surfman operations may change native bindings without updating the WebGL
// thread's logical binding cache. Preserve them across temporary surface work.
struct Bindings {
    draw: Option<glow::NativeFramebuffer>,
    read: Option<glow::NativeFramebuffer>,
    renderbuffer: Option<glow::NativeRenderbuffer>,
    active: u32,
    texture: Option<glow::NativeTexture>,
    pack: Option<glow::NativeBuffer>,
    unpack: Option<glow::NativeBuffer>,
    pbo: bool,
}

impl Bindings {
    fn save(gl: &glow::Context) -> Self {
        unsafe {
            let pbo = gl.version().major >= 3;
            Self {
                draw: gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
                read: gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
                renderbuffer: gl.get_parameter_renderbuffer(glow::RENDERBUFFER_BINDING),
                active: gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32,
                texture: gl.get_parameter_texture(glow::TEXTURE_BINDING_2D),
                pack: if pbo {
                    gl.get_parameter_buffer(glow::PIXEL_PACK_BUFFER_BINDING)
                } else {
                    None
                },
                unpack: if pbo {
                    gl.get_parameter_buffer(glow::PIXEL_UNPACK_BUFFER_BINDING)
                } else {
                    None
                },
                pbo,
            }
        }
    }

    fn restore(self, gl: &glow::Context) {
        unsafe {
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, self.draw);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, self.read);
            gl.bind_renderbuffer(glow::RENDERBUFFER, self.renderbuffer);
            gl.active_texture(self.active);
            gl.bind_texture(glow::TEXTURE_2D, self.texture);
            if self.pbo {
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, self.pack);
                gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, self.unpack);
            }
        }
    }
}

pub(crate) fn preserve_bindings<T>(
    contexts: &mut dyn GLContexts<SurfmanGL>,
    id: ContextId,
    operation: impl FnOnce(&mut dyn GLContexts<SurfmanGL>) -> Result<T, Error>,
) -> Result<T, Error> {
    let device = contexts.device(id).ok_or(Error::NoMatchingDevice)?;
    let gl = contexts.bindings(id).ok_or(Error::NoMatchingDevice)?;
    let saved = Bindings::save(gl);
    if saved.pbo {
        unsafe {
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
        }
    }
    let result = operation(contexts);
    let context = contexts.context(id).ok_or(Error::NoMatchingDevice)?;
    device.make_context_current(context).map_err(backend)?;
    saved.restore(contexts.bindings(id).ok_or(Error::NoMatchingDevice)?);
    result
}

fn check_surface(
    contexts: &mut dyn GLContexts<SurfmanGL>,
    id: ContextId,
    connection: Connection,
    adapter: Adapter,
) -> Result<(), Error> {
    preserve_bindings(contexts, id, |contexts| {
        check_surface_inner(contexts, id, connection, adapter)
    })
}

fn check_surface_inner(
    contexts: &mut dyn GLContexts<SurfmanGL>,
    id: ContextId,
    connection: Connection,
    adapter: Adapter,
) -> Result<(), Error> {
    let producer = contexts.device(id).ok_or(Error::NoMatchingDevice)?;
    let context = contexts.context(id).ok_or(Error::NoMatchingDevice)?;
    let consumer = connection.create_device(&adapter).map_err(backend)?;
    let descriptor = consumer
        .create_context_descriptor(&ContextAttributes {
            version: if consumer.gl_api() == GLApi::GLES {
                GLVersion::new(3, 0)
            } else {
                GLVersion::new(3, 2)
            },
            flags: ContextAttributeFlags::ALPHA
                | ContextAttributeFlags::DEPTH
                | ContextAttributeFlags::STENCIL,
        })
        .map_err(backend)?;
    let mut reader = consumer
        .create_context(&descriptor, None)
        .map_err(backend)?;
    let result = (|| {
        let surface = consumer
            .create_surface(
                &reader,
                SurfaceAccess::GPUOnly,
                SurfaceType::Generic {
                    size: Size2D::new(1, 1),
                },
            )
            .map_err(backend)?;
        if let Err((error, mut surface)) = consumer.bind_surface_to_context(&mut reader, surface) {
            let _ = consumer.destroy_surface(&mut reader, &mut surface);
            return Err(backend(error));
        }
        // Surface creation can change the current context internally. Explicitly
        // select the producer before allocating its independently owned image.
        producer.make_context_current(context).map_err(backend)?;
        let mut surface = Some(
            producer
                .create_surface(
                    context,
                    SurfaceAccess::GPUOnly,
                    SurfaceType::Generic {
                        size: Size2D::new(1, 1),
                    },
                )
                .map_err(backend)?,
        );
        let imported = (|| {
            consumer.make_context_current(&reader).map_err(backend)?;
            let texture =
                match consumer.create_surface_texture(&mut reader, surface.take().unwrap()) {
                    Ok(texture) => texture,
                    Err((error, original)) => {
                        surface = Some(original);
                        return Err(backend(error));
                    },
                };
            let gl = unsafe {
                glow::Context::from_loader_function(|name| consumer.get_proc_address(&reader, name))
            };
            let valid = unsafe {
                if let Ok(fbo) = gl.create_framebuffer() {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        consumer.surface_gl_texture_target(),
                        consumer.surface_texture_object(&texture),
                        0,
                    );
                    let complete = gl.check_framebuffer_status(glow::FRAMEBUFFER)
                        == glow::FRAMEBUFFER_COMPLETE;
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        consumer.surface_gl_texture_target(),
                        None,
                        0,
                    );
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    gl.delete_framebuffer(fbo);
                    complete
                } else {
                    false
                }
            };
            surface = Some(
                consumer
                    .destroy_surface_texture(&mut reader, texture)
                    .map_err(|(error, _)| backend(error))?,
            );
            if valid {
                Ok(())
            } else {
                Err(backend("imported framebuffer is incomplete"))
            }
        })();
        let restored = producer.make_context_current(context).map_err(backend);
        let disposed = if let Some(mut surface) = surface {
            producer
                .destroy_surface(context, &mut surface)
                .map_err(backend)
        } else {
            Ok(())
        };
        imported.and(restored).and(disposed)
    })();
    let _ = consumer.make_context_current(&reader);
    let disposed = consumer.destroy_context(&mut reader).map_err(backend);
    // Restore even when allocation/import fails. The WebGL thread caches the
    // producer identity and must never resume commands on our temporary reader.
    let restored = producer.make_context_current(context).map_err(backend);
    result.and(disposed).and(restored)
}

/// Mock devices only need the producer surface path. They have no external
/// graphics adapter or compositor into which a surface must be imported.
pub fn check_mock(manager: &LayerGrandManager<SurfmanGL>, id: ContextId) -> Result<(), Error> {
    let probe = manager.create_layer_manager(move |contexts| {
        preserve_bindings(contexts, id, |contexts| {
            let device = contexts.device(id).ok_or(Error::NoMatchingDevice)?;
            let context = contexts.context(id).ok_or(Error::NoMatchingDevice)?;
            let mut surface = device
                .create_surface(
                    context,
                    SurfaceAccess::GPUOnly,
                    SurfaceType::Generic {
                        size: Size2D::new(1, 1),
                    },
                )
                .map_err(backend)?;
            device
                .destroy_surface(context, &mut surface)
                .map_err(backend)?;
            Ok(())
        })?;
        Ok(SurfmanLayerManager::new(SwapChains::new()))
    })?;
    drop(probe);
    Ok(())
}
