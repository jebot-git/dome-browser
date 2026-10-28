/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! An implementation of layer management using surfman

use std::collections::HashMap;
use std::rc::Rc;

use euclid::{Point2D, Rect, Size2D};
use glow::{self as gl, Context as Gl, HasContext};
use surfman::chains::{PreserveBuffer, SwapChains, SwapChainsAPI};
use surfman::{Context as SurfmanContext, Device as SurfmanDevice, SurfaceAccess, SurfaceTexture};
use webxr_api::{
    ContextId, Error, GLContexts, GLTypes, LayerAttributes, LayerGeometry, LayerId, LayerInfo,
    LayerInit, LayerManagerAPI, MAX_FRAMEBUFFER_DIMENSION, SubImage, SubImages, Viewport,
    Viewports,
};

use crate::compatibility::preserve_bindings;
use crate::gl_utils::{GlClearer, LayerColorBuffer, create_depth_stencil};

#[derive(Clone, Copy, Debug)]
pub enum SurfmanGL {}

impl GLTypes for SurfmanGL {
    type Device = Rc<SurfmanDevice>;
    type Context = SurfmanContext;
    type Bindings = Gl;
}

pub struct SurfmanLayerManager {
    layers: Vec<(ContextId, LayerId)>,
    swap_chains: SwapChains<LayerId, SurfmanDevice>,
    surface_textures: HashMap<LayerId, SurfaceTexture>,
    depth_stencil_textures: HashMap<LayerId, Option<gl::NativeTexture>>,
    layer_geometry: HashMap<LayerId, LayerGeometry>,
    color_buffers: HashMap<LayerId, LayerColorBuffer>,
    framebuffer_limit: Size2D<i32, Viewport>,
    clearer: GlClearer,
}

impl SurfmanLayerManager {
    pub fn new(swap_chains: SwapChains<LayerId, SurfmanDevice>) -> SurfmanLayerManager {
        let layers = Vec::new();
        let surface_textures = HashMap::new();
        let depth_stencil_textures = HashMap::new();
        let clearer = GlClearer::new(false);
        SurfmanLayerManager {
            layers,
            swap_chains,
            surface_textures,
            depth_stencil_textures,
            layer_geometry: HashMap::new(),
            color_buffers: HashMap::new(),
            framebuffer_limit: Size2D::new(MAX_FRAMEBUFFER_DIMENSION, MAX_FRAMEBUFFER_DIMENSION),
            clearer,
        }
    }
    /// Apply transport/device limits before creating any GL resources.
    pub fn with_framebuffer_limit(mut self, limit: Size2D<i32, Viewport>) -> Self {
        self.framebuffer_limit = limit;
        self
    }

    // Native texture wrappers are not Send. Keep a failed release on the WebGL
    // producer rather than storing it in a cross-thread swap chain or dropping it.
    fn recycle_layer_texture(
        &mut self,
        layer_id: LayerId,
        device: &SurfmanDevice,
        context: &mut SurfmanContext,
    ) -> Result<(), Error> {
        if !self.surface_textures.contains_key(&layer_id) {
            return Ok(());
        }
        let swap_chain = self
            .swap_chains
            .get(layer_id)
            .ok_or(Error::NoMatchingDevice)?;
        let mut texture = self.surface_textures.remove(&layer_id);
        let result = swap_chain.recycle_surface_texture(device, context, &mut texture);
        if let Some(texture) = texture {
            self.surface_textures.insert(layer_id, texture);
        }
        result.map_err(|error| Error::BackendSpecific(format!("{error:?}")))
    }
}

impl LayerManagerAPI<SurfmanGL> for SurfmanLayerManager {
    fn create_layer(
        &mut self,
        contexts: &mut dyn GLContexts<SurfmanGL>,
        context_id: ContextId,
        init: LayerInit,
        viewports: Viewports,
    ) -> Result<LayerInfo, Error> {
        preserve_bindings(contexts, context_id, |contexts| {
            let device = contexts.device(context_id).ok_or(Error::NoMatchingDevice)?;
            let gl = contexts
                .bindings(context_id)
                .ok_or(Error::NoMatchingDevice)?;
            let gl_limit = unsafe {
                gl.get_parameter_i32(gl::MAX_TEXTURE_SIZE)
                    .min(gl.get_parameter_i32(gl::MAX_RENDERBUFFER_SIZE))
            };
            let geometry = init.geometry(
                &viewports,
                Size2D::new(
                    self.framebuffer_limit.width.min(gl_limit),
                    self.framebuffer_limit.height.min(gl_limit),
                ),
            )?;
            let size = geometry.framebuffer_size.to_untyped();
            let layer_id = LayerId::default();
            let has_depth_stencil = match init {
                LayerInit::WebGLLayer { stencil, depth, .. }
                | LayerInit::ProjectionLayer { stencil, depth, .. } => stencil || depth,
            };
            let color = LayerColorBuffer::new(
                gl,
                device.surface_gl_texture_target(),
                (size.width, size.height),
                init.alpha(),
            )?;
            let depth_stencil = if has_depth_stencil {
                let texture = match create_depth_stencil(gl, (size.width, size.height)) {
                    Ok(texture) => texture,
                    Err(error) => {
                        color.destroy(gl);
                        return Err(error);
                    },
                };
                Some(texture)
            } else {
                None
            };
            let context = contexts
                .context(context_id)
                .ok_or(Error::NoMatchingDevice)?;
            if let Err(error) = self.swap_chains.create_detached_swap_chain(
                layer_id,
                size,
                &device,
                context,
                SurfaceAccess::GPUOnly,
            ) {
                if let Some(gl) = contexts.bindings(context_id) {
                    color.destroy(gl);
                    if let Some(texture) = depth_stencil {
                        unsafe { gl.delete_texture(texture) };
                    }
                }
                return Err(Error::BackendSpecific(format!("{error:?}")));
            }
            self.color_buffers.insert(layer_id, color);
            self.depth_stencil_textures.insert(layer_id, depth_stencil);
            self.layer_geometry.insert(layer_id, geometry.clone());
            self.layers.push((context_id, layer_id));
            Ok(LayerInfo {
                id: layer_id,
                geometry,
                attributes: LayerAttributes {
                    alpha: init.alpha(),
                    antialias: false,
                    ignore_depth_values: true,
                },
            })
        })
    }

    fn destroy_layer(
        &mut self,
        contexts: &mut dyn GLContexts<SurfmanGL>,
        context_id: ContextId,
        layer_id: LayerId,
    ) {
        // Late duplicate destruction must not select or query a retired context.
        if !self.layers.contains(&(context_id, layer_id)) { return; }
        if contexts.context_lost(context_id) {
            // No ordinary GL calls are valid after native context loss. Keep
            // owners on any native cleanup error, exactly as in the live path.
            let Some(device) = contexts.device(context_id) else {
                return;
            };
            let Some(context) = contexts.context(context_id) else {
                return;
            };
            if self
                .recycle_layer_texture(layer_id, &device, context)
                .is_err()
            {
                return;
            }
            if self
                .swap_chains
                .destroy(layer_id, &device, context)
                .is_err()
            {
                return;
            }
            self.clearer.forget_layer(layer_id);
            self.color_buffers.remove(&layer_id);
            self.depth_stencil_textures.remove(&layer_id);
            self.layer_geometry.remove(&layer_id);
            self.layers.retain(|&ids| ids != (context_id, layer_id));
            return;
        }
        let _ = preserve_bindings(contexts, context_id, |contexts| {
            let Some(device) = contexts.device(context_id) else {
                return Ok(());
            };
            let Some(context) = contexts.context(context_id) else {
                return Ok(());
            };
            self.recycle_layer_texture(layer_id, &device, context)?;
            self.swap_chains
                .destroy(layer_id, &device, context)
                .map_err(|error| Error::BackendSpecific(format!("{error:?}")))?;
            self.clearer.destroy_layer(contexts, context_id, layer_id);
            let gl = contexts
                .bindings(context_id)
                .ok_or(Error::NoMatchingDevice)?;
            if let Some(color) = self.color_buffers.remove(&layer_id) {
                color.destroy(gl);
            }
            if let Some(Some(texture)) = self.depth_stencil_textures.remove(&layer_id) {
                unsafe {
                    gl.delete_texture(texture);
                }
            }
            self.layer_geometry.remove(&layer_id);
            self.layers.retain(|&ids| ids != (context_id, layer_id));
            Ok(())
        });
    }

    fn layers(&self) -> &[(ContextId, LayerId)] {
        &self.layers[..]
    }

    fn begin_frame(
        &mut self,
        contexts: &mut dyn GLContexts<SurfmanGL>,
        layers: &[(ContextId, LayerId)],
    ) -> Result<Vec<SubImages>, Error> {
        layers
            .iter()
            .map(|&(context_id, layer_id)| {
                preserve_bindings(contexts, context_id, |contexts| {
                    let device = contexts.device(context_id).ok_or(Error::NoMatchingDevice)?;
                    let context = contexts
                        .context(context_id)
                        .ok_or(Error::NoMatchingDevice)?;
                    let swap_chain = self
                        .swap_chains
                        .get(layer_id)
                        .ok_or(Error::NoMatchingDevice)?;
                    self.recycle_layer_texture(layer_id, &device, context)?;
                    // Look up all fallible metadata before borrowing a native
                    // texture from the swap chain.
                    let color = self
                        .color_buffers
                        .get(&layer_id)
                        .ok_or(Error::NoMatchingDevice)?;
                    let geometry = self
                        .layer_geometry
                        .get(&layer_id)
                        .ok_or(Error::NoMatchingDevice)?;
                    let surface_size = Size2D::from_untyped(swap_chain.size());
                    let surface_texture = swap_chain
                        .take_surface_texture(&device, context)
                        .map_err(|_| Error::NoMatchingDevice)?;
                    let color_texture =
                        color.texture(device.surface_texture_object(&surface_texture));
                    let color_target = device.surface_gl_texture_target();
                    let depth_stencil_texture = self
                        .depth_stencil_textures
                        .get(&layer_id)
                        .cloned()
                        .flatten();
                    let texture_array_index = None;
                    let origin = Point2D::new(0, 0);
                    let sub_image = Some(SubImage {
                        color_texture: color_texture.map(|nt| nt.0),
                        depth_stencil_texture: depth_stencil_texture.map(|nt| nt.0),
                        texture_array_index,
                        viewport: Rect::new(origin, surface_size),
                    });
                    let view_sub_images = geometry
                        .viewports
                        .viewports
                        .iter()
                        .map(|&viewport| SubImage {
                            color_texture: color_texture.map(|nt| nt.0),
                            depth_stencil_texture: depth_stencil_texture.map(|texture| texture.0),
                            texture_array_index,
                            viewport,
                        })
                        .collect();
                    self.surface_textures.insert(layer_id, surface_texture);
                    self.clearer.clear(
                        contexts,
                        context_id,
                        layer_id,
                        color_texture,
                        color_target,
                        depth_stencil_texture,
                    )?;
                    Ok(SubImages {
                        layer_id,
                        sub_image,
                        view_sub_images,
                    })
                })
            })
            .collect()
    }

    fn end_frame(
        &mut self,
        contexts: &mut dyn GLContexts<SurfmanGL>,
        layers: &[(ContextId, LayerId)],
    ) -> Result<(), Error> {
        for &(context_id, layer_id) in layers {
            preserve_bindings(contexts, context_id, |contexts| {
                let device = contexts.device(context_id).ok_or(Error::NoMatchingDevice)?;
                let gl = contexts
                    .bindings(context_id)
                    .ok_or(Error::NoMatchingDevice)?;
                let surface = self
                    .surface_textures
                    .get(&layer_id)
                    .ok_or(Error::NoMatchingDevice)?;
                let size = self
                    .layer_geometry
                    .get(&layer_id)
                    .ok_or(Error::NoMatchingDevice)?
                    .framebuffer_size;
                self.color_buffers
                    .get(&layer_id)
                    .ok_or(Error::NoMatchingDevice)?
                    .present(
                        gl,
                        &mut self.clearer,
                        layer_id,
                        device.surface_texture_object(surface),
                        device.surface_gl_texture_target(),
                        (size.width, size.height),
                    )?;
                self.clearer
                    .release_surface(gl, layer_id, device.surface_texture_object(surface));
                unsafe { gl.flush() };

                let context = contexts
                    .context(context_id)
                    .ok_or(Error::NoMatchingDevice)?;
                let swap_chain = self
                    .swap_chains
                    .get(layer_id)
                    .ok_or(Error::NoMatchingDevice)?;

                self.recycle_layer_texture(layer_id, &device, context)?;
                swap_chain
                    .swap_buffers(&device, context, PreserveBuffer::No)
                    .map_err(|err| Error::BackendSpecific(format!("{:?}", err)))?;
                Ok(())
            })?;
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "x11", target_os = "linux"))]
mod native_retirement_tests {
    use super::*;
    use surfman::{ContextAttributeFlags, ContextAttributes, GLVersion, SurfaceType};

    struct PrivateContexts {
        device: Rc<SurfmanDevice>,
        context: Option<SurfmanContext>,
        gl: Gl,
        lost: bool,
        binding_queries: usize,
    }
    impl GLContexts<SurfmanGL> for PrivateContexts {
        fn context_lost(&self, _: ContextId) -> bool {
            self.context.as_ref().is_some_and(|context| self.device.context_is_lost(context))
        }
        fn device(&self, _: ContextId) -> Option<Rc<SurfmanDevice>> {
            Some(self.device.clone())
        }
        fn context(&mut self, _: ContextId) -> Option<&mut SurfmanContext> {
            self.context.as_mut()
        }
        fn bindings(&mut self, _: ContextId) -> Option<&Gl> {
            assert!(
                !self.lost,
                "ordinary GL bindings requested during native-loss retirement"
            );
            self.binding_queries += 1;
            self.device
                .make_context_current(self.context.as_ref()?)
                .unwrap();
            Some(&self.gl)
        }
    }

    fn retire_borrowed_layer(alpha: bool) {
        // Select the surfaceless alternate directly: no desktop connection is
        // opened, and marking this private context lost never resets the GPU.
        let connection = surfman::mesa_surfaceless::connection::Connection::new().unwrap();
        let adapter = if std::env::var_os("DOME_XR_RETIRE_SOFTWARE").is_some() {
            connection.create_software_adapter().unwrap()
        } else {
            connection.create_low_power_adapter().unwrap()
        };
        let device = Rc::new(SurfmanDevice::Alternate(
            connection.create_device(&adapter).unwrap(),
        ));
        let descriptor = device
            .create_context_descriptor(&ContextAttributes {
                version: GLVersion::new(3, 3),
                flags: ContextAttributeFlags::ALPHA
                    | ContextAttributeFlags::DEPTH
                    | ContextAttributeFlags::STENCIL,
            })
            .unwrap();
        let mut context = device.create_context(&descriptor, None).unwrap();
        let surface = device
            .create_surface(
                &context,
                SurfaceAccess::GPUOnly,
                SurfaceType::Generic {
                    size: Size2D::new(16, 16),
                },
            )
            .unwrap();
        device
            .bind_surface_to_context(&mut context, surface)
            .unwrap();
        device.make_context_current(&context).unwrap();
        let gl =
            unsafe { Gl::try_from_loader_function(|name| device.get_proc_address(&context, name)) }
                .unwrap();
        unsafe {
            println!(
                "DOME_XR_RETIRE_RENDERER={}",
                gl.get_parameter_string(gl::RENDERER)
            );
        }
        let mut contexts = PrivateContexts {
            device,
            context: Some(context),
            gl,
            lost: false,
            binding_queries: 0,
        };
        let chains = SwapChains::new();
        let mut manager = SurfmanLayerManager::new(chains.clone());
        let id = ContextId(1);
        let info = manager
            .create_layer(
                &mut contexts,
                id,
                LayerInit::WebGLLayer {
                    antialias: false,
                    alpha,
                    depth: true,
                    stencil: true,
                    ignore_depth_values: true,
                    framebuffer_scale_factor: 1.,
                },
                Viewports {
                    viewports: vec![Rect::new(Point2D::origin(), Size2D::new(16, 16))],
                },
            )
            .unwrap();
        manager
            .begin_frame(&mut contexts, &[(id, info.id)])
            .unwrap();
        assert!(manager.surface_textures.contains_key(&info.id));
        assert!(manager.depth_stencil_textures.contains_key(&info.id));
        assert!(!contexts.context_lost(id));
        contexts
            .device
            .mark_context_lost(contexts.context.as_mut().unwrap())
            .unwrap();
        assert!(contexts.context_lost(id));
        contexts.lost = true;
        let queries = contexts.binding_queries;
        manager.destroy_layer(&mut contexts, id, info.id);
        let released = manager.layers().is_empty()
            && manager.surface_textures.is_empty()
            && manager.depth_stencil_textures.is_empty()
            && manager.color_buffers.is_empty()
            && manager.layer_geometry.is_empty()
            && chains.get(info.id).is_none();
        assert_eq!(contexts.binding_queries, queries);
        let mut context = contexts.context.take().unwrap();
        contexts.device.destroy_context(&mut context).unwrap();
        assert!(
            released,
            "native loss did not release all producer-owned layer resources"
        );
        manager.destroy_layer(&mut contexts, id, info.id);
    }

    #[test]
    fn native_loss_retires_a_borrowed_rgb_layer_without_gl_restoration() {
        retire_borrowed_layer(false);
    }
    #[test]
    fn native_loss_retires_a_borrowed_rgba_layer_without_gl_restoration() {
        retire_borrowed_layer(true);
    }
}
