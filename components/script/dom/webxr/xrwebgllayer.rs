/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::convert::TryInto;

use dom_struct::dom_struct;
use euclid::{Rect, Size2D};
use js::context::JSContext;
use js::rust::HandleObject;
use script_bindings::cell::DomRefCell;
use script_bindings::reflector::reflect_dom_object_with_proto;
use servo_canvas_traits::webgl::{WebGLCommand, WebGLContextId, WebGLTextureId};
use webxr_api::{
    ContextId as WebXRContextId, LayerAttributes, LayerGeometry, LayerId, LayerInit, Viewport,
    Viewports, scaled_viewport,
};

use crate::canvas_context::CanvasContext;
use crate::conversions::Convert;
use crate::dom::bindings::codegen::Bindings::WebGL2RenderingContextBinding::WebGL2RenderingContextConstants as constants;
use crate::dom::bindings::codegen::Bindings::WebGLRenderingContextBinding::WebGLRenderingContextMethods;
use crate::dom::bindings::codegen::Bindings::XRWebGLLayerBinding::{
    XRWebGLLayerInit, XRWebGLLayerMethods, XRWebGLRenderingContext,
};
use crate::dom::bindings::error::{Error, Fallible};
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::num::Finite;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::globalscope::GlobalScope;
use crate::dom::webgl::webglframebuffer::WebGLFramebuffer;
use crate::dom::webgl::webglrenderingcontext::WebGLRenderingContext;
use crate::dom::webgl::webgltexture::WebGLTexture;
use crate::dom::window::Window;
use crate::dom::xrframe::XRFrame;
use crate::dom::xrlayer::XRLayer;
use crate::dom::xrsession::XRSession;
use crate::dom::xrview::XRView;
use crate::dom::xrviewport::XRViewport;

impl Convert<LayerInit> for XRWebGLLayerInit {
    fn convert(self) -> LayerInit {
        LayerInit::WebGLLayer {
            alpha: self.alpha,
            antialias: self.antialias,
            depth: self.depth,
            stencil: self.stencil,
            framebuffer_scale_factor: *self.framebufferScaleFactor as f32,
            ignore_depth_values: self.ignoreDepthValues,
        }
    }
}

#[dom_struct]
pub(crate) struct XRWebGLLayer {
    xr_layer: XRLayer,
    antialias: bool,
    ignore_depth_values: bool,
    /// If none, this is an inline session (the composition disabled flag is true)
    framebuffer: Option<Dom<WebGLFramebuffer>>,
    #[no_trace]
    viewports: DomRefCell<Vec<Rect<i32, Viewport>>>,
    #[no_trace]
    source_viewports: DomRefCell<Vec<Rect<i32, Viewport>>>,
    viewport_objects: DomRefCell<Vec<Option<(Dom<XRViewport>, f64, Dom<XRFrame>, u64)>>>,
}

impl XRWebGLLayer {
    pub(crate) fn new_inherited(
        session: &XRSession,
        context: &WebGLRenderingContext,
        attributes: LayerAttributes,
        framebuffer: Option<&WebGLFramebuffer>,
        layer_id: Option<LayerId>,
        viewports: Vec<Rect<i32, Viewport>>,
    ) -> XRWebGLLayer {
        XRWebGLLayer {
            xr_layer: XRLayer::new_inherited(session, context, layer_id),
            antialias: attributes.antialias,
            ignore_depth_values: attributes.ignore_depth_values,
            framebuffer: framebuffer.map(Dom::from_ref),
            viewports: DomRefCell::new(viewports),
            source_viewports: DomRefCell::new(
                session.with_session(|session| session.viewports().to_vec()),
            ),
            viewport_objects: DomRefCell::new(Vec::new()),
        }
    }

    #[expect(clippy::too_many_arguments)]
    fn new(
        cx: &mut JSContext,
        global: &GlobalScope,
        proto: Option<HandleObject>,
        session: &XRSession,
        context: &WebGLRenderingContext,
        attributes: LayerAttributes,
        framebuffer: Option<&WebGLFramebuffer>,
        layer_id: Option<LayerId>,
        viewports: Vec<Rect<i32, Viewport>>,
    ) -> DomRoot<XRWebGLLayer> {
        reflect_dom_object_with_proto(
            cx,
            Box::new(XRWebGLLayer::new_inherited(
                session,
                context,
                attributes,
                framebuffer,
                layer_id,
                viewports,
            )),
            global,
            proto,
        )
    }

    pub(crate) fn layer_id(&self) -> Option<LayerId> {
        self.xr_layer.layer_id()
    }

    pub(crate) fn context_id(&self) -> WebGLContextId {
        self.xr_layer.context_id()
    }

    pub(crate) fn session(&self) -> &XRSession {
        self.xr_layer.session()
    }

    pub(crate) fn size(&self) -> Size2D<u32, Viewport> {
        if let Some(framebuffer) = self.framebuffer.as_ref() {
            let size = framebuffer.size().unwrap_or((0, 0));
            Size2D::new(
                size.0.try_into().unwrap_or(0),
                size.1.try_into().unwrap_or(0),
            )
        } else {
            Size2D::from_untyped(self.context().size())
        }
    }

    fn texture_target(&self) -> u32 {
        if cfg!(target_os = "macos") {
            glow::TEXTURE_RECTANGLE
        } else {
            glow::TEXTURE_2D
        }
    }

    pub(crate) fn begin_frame(&self, cx: &mut JSContext, frame: &XRFrame) -> Option<()> {
        debug!("XRWebGLLayer begin frame");
        let framebuffer = self.framebuffer.as_ref()?;
        let source = self
            .session()
            .with_session(|session| session.viewports().to_vec());
        if *self.source_viewports.borrow() != source {
            let full = if source.is_empty() {
                Vec::new()
            } else {
                LayerGeometry::new(
                    &Viewports {
                        viewports: source.clone(),
                    },
                    f32::INFINITY,
                    self.size().to_i32(),
                )
                .ok()?
                .viewports
                .viewports
            };
            self.viewport_objects.borrow_mut().truncate(full.len());
            *self.viewports.borrow_mut() = full;
            *self.source_viewports.borrow_mut() = source;
        }
        let sub_images = frame.get_sub_images(self.layer_id()?)?;
        let session = self.session();
        let context = framebuffer.upcast().context()?;

        // TODO: Cache this texture
        let color_texture_id = WebGLTextureId::new(sub_images.sub_image.as_ref()?.color_texture?);
        let color_texture = WebGLTexture::new_webxr(cx, &context, color_texture_id, session);
        let target = self.texture_target();

        let saved_target = framebuffer.target();
        let saved_texture_id = context
            .textures()
            .active_texture_slot(target, context.webgl_version())
            .ok()
            .and_then(|slot| slot.get().map(|texture| texture.id()));
        context.send_command(WebGLCommand::BindTexture(target, Some(color_texture_id)));
        framebuffer.bind(constants::FRAMEBUFFER);
        let attached = (|| {
            framebuffer
                .texture2d_even_if_opaque(
                    constants::COLOR_ATTACHMENT0,
                    target,
                    Some(&color_texture),
                    0,
                )
                .ok()?;
            if let Some(id) = sub_images.sub_image.as_ref()?.depth_stencil_texture {
                let depth_texture =
                    WebGLTexture::new_webxr(cx, &context, WebGLTextureId::new(id), session);
                framebuffer
                    .texture2d_even_if_opaque(
                        constants::DEPTH_STENCIL_ATTACHMENT,
                        constants::TEXTURE_2D,
                        Some(&depth_texture),
                        0,
                    )
                    .ok()?;
            }
            Some(())
        })();
        context.send_command(WebGLCommand::BindTexture(target, saved_texture_id));
        if let Some(target) = saved_target {
            framebuffer.bind(target);
        }
        Self::restore_framebuffer_bindings(&context);
        attached
    }

    pub(crate) fn end_frame(&self, _frame: &XRFrame) -> Option<()> {
        debug!("XRWebGLLayer end frame");
        let framebuffer = self.framebuffer.as_ref()?;
        let context = framebuffer.upcast().context()?;
        let saved_target = framebuffer.target();
        framebuffer.bind(constants::FRAMEBUFFER);
        let detached = (|| {
            framebuffer
                .texture2d_even_if_opaque(
                    constants::COLOR_ATTACHMENT0,
                    self.texture_target(),
                    None,
                    0,
                )
                .ok()?;
            framebuffer
                .texture2d_even_if_opaque(
                    constants::DEPTH_STENCIL_ATTACHMENT,
                    constants::TEXTURE_2D,
                    None,
                    0,
                )
                .ok()?;
            Some(())
        })();
        if let Some(target) = saved_target {
            framebuffer.bind(target);
        }
        Self::restore_framebuffer_bindings(&context);
        if let Some(layer) = self.layer_id() {
            let viewports = Viewports {
                viewports: self
                    .viewports
                    .borrow()
                    .iter()
                    .enumerate()
                    .map(|(index, &full)| {
                        scaled_viewport(full, self.session().current_viewport_scale(index))
                    })
                    .collect(),
            };
            self.session().with_session(|session| {
                session.set_layer_viewports(self.context_id().into(), layer, viewports)
            });
        }
        context.Flush();
        detached
    }

    fn restore_framebuffer_bindings(context: &WebGLRenderingContext) {
        let draw = context.get_draw_framebuffer_slot().get();
        let read = context.get_read_framebuffer_slot().get();
        if context.webgl_version() == servo_canvas_traits::webgl::WebGLVersion::WebGL2 {
            context.bind_framebuffer_to(
                constants::DRAW_FRAMEBUFFER,
                draw.as_deref(),
                context.get_draw_framebuffer_slot(),
            );
            context.bind_framebuffer_to(
                constants::READ_FRAMEBUFFER,
                read.as_deref(),
                context.get_read_framebuffer_slot(),
            );
        } else {
            context.bind_framebuffer_to(
                constants::FRAMEBUFFER,
                draw.as_deref(),
                context.get_draw_framebuffer_slot(),
            );
        }
    }

    pub(crate) fn context(&self) -> &WebGLRenderingContext {
        self.xr_layer.context()
    }
}

impl XRWebGLLayerMethods<crate::DomTypeHolder> for XRWebGLLayer {
    /// <https://immersive-web.github.io/webxr/#dom-xrwebgllayer-xrwebgllayer>
    fn Constructor(
        cx: &mut JSContext,
        global: &Window,
        proto: Option<HandleObject>,
        session: &XRSession,
        context: XRWebGLRenderingContext,
        init: &XRWebGLLayerInit,
    ) -> Fallible<DomRoot<Self>> {
        let context = match context {
            XRWebGLRenderingContext::WebGLRenderingContext(ctx) => ctx,
            XRWebGLRenderingContext::WebGL2RenderingContext(ctx) => ctx.base_context(),
        };

        // Step 2
        if session.is_ended() {
            return Err(Error::InvalidState(None));
        }
        if context.is_context_lost() { return Err(Error::InvalidState(None)); }
        if session.is_immersive() && !context.is_xr_compatible() {
            return Err(Error::InvalidState(None));
        }

        let (framebuffer, layer_id, viewports, attributes) = if session.is_immersive() {
            // The allocation result is authoritative after device/GPU clamping.
            // Its dimensions and eye rectangles must match the compositor.
            let context_id = WebXRContextId::from(context.context_id());
            let info = session
                .with_session(|session| session.create_layer(context_id, init.convert()))
                .map_err(|_| Error::Operation(None))?;
            let Some(framebuffer) = WebGLFramebuffer::maybe_new_webxr(
                cx,
                session,
                &context,
                info.geometry.framebuffer_size,
            ) else {
                session.with_session(|session| session.destroy_layer(context_id, info.id));
                return Err(Error::Operation(None));
            };
            (
                Some(framebuffer),
                Some(info.id),
                info.geometry.viewports.viewports,
                info.attributes,
            )
        } else {
            let actual = context
                .GetContextAttributes()
                .ok_or(Error::InvalidState(None))?;
            (
                None,
                None,
                Vec::new(),
                LayerAttributes {
                    antialias: actual.antialias,
                    alpha: actual.alpha,
                    ignore_depth_values: true,
                },
            )
        };

        // Ensure that we finish setting up this layer before continuing.
        context.Finish();

        // Step 10. "Return layer."
        Ok(XRWebGLLayer::new(
            cx,
            &global.global(),
            proto,
            session,
            &context,
            attributes,
            framebuffer.as_deref(),
            layer_id,
            viewports,
        ))
    }

    /// <https://www.w3.org/TR/webxr/#dom-xrwebgllayer-getnativeframebufferscalefactor>
    fn GetNativeFramebufferScaleFactor(_window: &Window, session: &XRSession) -> Finite<f64> {
        let value: f64 = if session.is_ended() { 0.0 } else { 1.0 };
        Finite::wrap(value)
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrwebgllayer-antialias>
    fn Antialias(&self) -> bool {
        self.antialias
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrwebgllayer-ignoredepthvalues>
    fn IgnoreDepthValues(&self) -> bool {
        self.ignore_depth_values
    }

    /// <https://www.w3.org/TR/webxr/#dom-xrwebgllayer-fixedfoveation>
    fn GetFixedFoveation(&self) -> Option<Finite<f32>> {
        // Fixed foveation is only available on Quest/Pico headset runtimes
        None
    }

    /// <https://www.w3.org/TR/webxr/#dom-xrwebgllayer-fixedfoveation>
    fn SetFixedFoveation(&self, _value: Option<Finite<f32>>) {
        // no-op until fixed foveation is supported
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrwebgllayer-framebuffer>
    fn GetFramebuffer(&self) -> Option<DomRoot<WebGLFramebuffer>> {
        self.framebuffer.as_ref().map(|x| DomRoot::from_ref(&**x))
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrwebgllayer-framebufferwidth>
    fn FramebufferWidth(&self) -> u32 {
        self.size().width
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrwebgllayer-framebufferheight>
    fn FramebufferHeight(&self) -> u32 {
        self.size().height
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrwebgllayer-getviewport>
    fn GetViewport(
        &self,
        cx: &mut JSContext,
        view: &XRView,
    ) -> Fallible<Option<DomRoot<XRViewport>>> {
        if self.session() != view.session() || !view.frame().is_active() {
            return Err(Error::InvalidState(None));
        }
        let index = view.viewport_index();
        let cached = self
            .viewport_objects
            .borrow()
            .get(index)
            .and_then(|cached| {
                cached.as_ref().map(|(viewport, scale, frame, id)| {
                    (
                        DomRoot::from_ref(&**viewport),
                        *scale,
                        DomRoot::from_ref(&**frame),
                        *id,
                    )
                })
            });
        if cached
            .as_ref()
            .is_some_and(|(_, _, frame, id)| &**frame == view.frame() && *id == view.view_id())
        {
            return Ok(cached.map(|(viewport, _, _, _)| viewport));
        }
        let full = if self.framebuffer.is_none() {
            Rect::from_size(self.size().to_i32())
        } else {
            let viewports = self.viewports.borrow();
            let Some(full) = viewports.get(index) else {
                return Ok(None);
            };
            *full
        };
        let scale = self.session().commit_viewport_scale(view.view_id());
        let viewport = scaled_viewport(full, scale);
        let result = if let Some((cached, cached_scale, _, cached_id)) = cached
            && cached_id == view.view_id()
            && cached_scale == scale
            && cached.rect() == viewport
        {
            cached
        } else {
            XRViewport::new(cx, &self.global(), viewport)
        };
        let mut cache = self.viewport_objects.borrow_mut();
        let length = cache.len().max(index + 1);
        cache.resize_with(length, || None);
        cache[index] = Some((
            Dom::from_ref(&*result),
            scale,
            Dom::from_ref(view.frame()),
            view.view_id(),
        ));
        Ok(Some(result))
    }
}
