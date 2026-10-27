/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! An implementation of layer management using surfman

use std::collections::HashMap;
use std::rc::Rc;

use euclid::{Point2D, Rect, Size2D};
use glow::{self as gl, Context as Gl, HasContext, PixelUnpackData};
use surfman::chains::{PreserveBuffer, SwapChains, SwapChainsAPI};
use surfman::{Context as SurfmanContext, Device as SurfmanDevice, SurfaceAccess, SurfaceTexture};
use webxr_api::{
    ContextId, Error, GLContexts, GLTypes, LayerGeometry, LayerId, LayerInfo, LayerInit,
    LayerManagerAPI, MAX_FRAMEBUFFER_DIMENSION, SubImage, SubImages, Viewport, Viewports,
};

use crate::gl_utils::GlClearer;

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
            framebuffer_limit: Size2D::new(MAX_FRAMEBUFFER_DIMENSION, MAX_FRAMEBUFFER_DIMENSION),
            clearer,
        }
    }
    /// Apply transport/device limits before creating any GL resources.
    pub fn with_framebuffer_limit(mut self, limit: Size2D<i32, Viewport>) -> Self {
        self.framebuffer_limit = limit;
        self
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
        let depth_stencil = if has_depth_stencil {
            let texture = unsafe { gl.create_texture() }.map_err(Error::BackendSpecific)?;
            unsafe {
                let saved = gl.get_parameter_texture(gl::TEXTURE_BINDING_2D);
                gl.bind_texture(gl::TEXTURE_2D, Some(texture));
                gl.tex_image_2d(
                    gl::TEXTURE_2D,
                    0,
                    gl::DEPTH24_STENCIL8 as _,
                    size.width,
                    size.height,
                    0,
                    gl::DEPTH_STENCIL,
                    gl::UNSIGNED_INT_24_8,
                    PixelUnpackData::Slice(None),
                );
                gl.bind_texture(gl::TEXTURE_2D, saved);
            }
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
            if let Some(texture) = depth_stencil {
                if let Some(gl) = contexts.bindings(context_id) {
                    unsafe { gl.delete_texture(texture) };
                }
            }
            return Err(Error::BackendSpecific(format!("{error:?}")));
        }
        self.depth_stencil_textures.insert(layer_id, depth_stencil);
        self.layer_geometry.insert(layer_id, geometry.clone());
        self.layers.push((context_id, layer_id));
        Ok(LayerInfo {
            id: layer_id,
            geometry,
        })
    }

    fn destroy_layer(
        &mut self,
        contexts: &mut dyn GLContexts<SurfmanGL>,
        context_id: ContextId,
        layer_id: LayerId,
    ) {
        self.layer_geometry.remove(&layer_id);
        self.clearer.destroy_layer(contexts, context_id, layer_id);
        let Some(device) = contexts.device(context_id) else {
            return;
        };
        let Some(context) = contexts.context(context_id) else {
            return;
        };
        self.layers.retain(|&ids| ids != (context_id, layer_id));
        let _ = self.swap_chains.destroy(layer_id, &device, context);

        if let Some(surface_texture) = self.surface_textures.remove(&layer_id)
            && let Ok(mut surface) = device.destroy_surface_texture(context, surface_texture)
        {
            let _ = device.destroy_surface(context, &mut surface);
        }

        if let Some(depth_stencil_texture) = self.depth_stencil_textures.remove(&layer_id) {
            let gl = contexts.bindings(context_id).unwrap();
            if let Some(depth_stencil_texture) = depth_stencil_texture {
                unsafe {
                    gl.delete_texture(depth_stencil_texture);
                }
            }
        }
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
                let device = contexts.device(context_id).ok_or(Error::NoMatchingDevice)?;
                let context = contexts
                    .context(context_id)
                    .ok_or(Error::NoMatchingDevice)?;
                let swap_chain = self
                    .swap_chains
                    .get(layer_id)
                    .ok_or(Error::NoMatchingDevice)?;
                let surface_size = Size2D::from_untyped(swap_chain.size());
                let surface_texture = swap_chain
                    .take_surface_texture(&device, context)
                    .map_err(|_| Error::NoMatchingDevice)?;
                let color_texture = device.surface_texture_object(&surface_texture);
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
                let view_sub_images = self
                    .layer_geometry
                    .get(&layer_id)
                    .ok_or(Error::NoMatchingDevice)?
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
                );
                Ok(SubImages {
                    layer_id,
                    sub_image,
                    view_sub_images,
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
            let gl = contexts
                .bindings(context_id)
                .ok_or(Error::NoMatchingDevice)?;
            unsafe {
                gl.flush();
            }
            let device = contexts.device(context_id).ok_or(Error::NoMatchingDevice)?;
            let context = contexts
                .context(context_id)
                .ok_or(Error::NoMatchingDevice)?;
            let surface_texture = self
                .surface_textures
                .remove(&layer_id)
                .ok_or(Error::NoMatchingDevice)?;
            let swap_chain = self
                .swap_chains
                .get(layer_id)
                .ok_or(Error::NoMatchingDevice)?;

            swap_chain
                .recycle_surface_texture(&device, context, surface_texture)
                .map_err(|err| Error::BackendSpecific(format!("{:?}", err)))?;
            swap_chain
                .swap_buffers(&device, context, PreserveBuffer::No)
                .map_err(|err| Error::BackendSpecific(format!("{:?}", err)))?;
        }
        Ok(())
    }
}
