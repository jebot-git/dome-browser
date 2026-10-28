/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
#![expect(unsafe_code)]
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::{slice, thread};

use bitflags::bitflags;
use byteorder::{ByteOrder, NativeEndian, WriteBytesExt};
use euclid::default::Size2D;
use glow::{
    self as gl, ActiveTransformFeedback, Context as Gl, HasContext, NativeTransformFeedback,
    NativeUniformLocation, NativeVertexArray, PixelUnpackData, ShaderPrecisionFormat,
    bytes_per_type, components_per_format,
};
use half::f16;
use itertools::Itertools;
use log::{debug, error, trace, warn};
use paint_api::{
    CrossProcessPaintApi, PainterSurfmanDetailsMap, SerializableImageData,
    WebRenderExternalImageIdManager, WebRenderImageHandlerType,
};
use parking_lot::RwLock;
use pixels::{self, PixelFormat, SnapshotAlphaMode, unmultiply_inplace};
use rustc_hash::FxHashMap;
use servo_base::Epoch;
use servo_base::generic_channel::{
    GenericReceiver, GenericSend, GenericSender, GenericSharedMemory, RoutedReceiver,
};
use servo_base::id::PainterId;
use servo_canvas_traits::webgl;
#[cfg(feature = "webxr")]
use servo_canvas_traits::webgl::WebXRCommand;
use servo_canvas_traits::webgl::{
    ActiveAttribInfo, ActiveUniformBlockInfo, ActiveUniformInfo, AlphaTreatment,
    GLContextAttributes, GLLimits, GlType, InternalFormatIntVec, ProgramLinkInfo, TexDataType,
    TexFormat, WebGLBufferId, WebGLChan, WebGLCommand, WebGLCommandBacktrace, WebGLContextId,
    WebGLCreateContextResult, WebGLFramebufferBindingRequest, WebGLFramebufferId, WebGLMsg,
    WebGLMsgSender, WebGLProgramId, WebGLQueryId, WebGLRenderbufferId, WebGLSLVersion,
    WebGLSamplerId, WebGLShaderId, WebGLSyncId, WebGLTextureId, WebGLVersion, WebGLVertexArrayId,
    YAxisTreatment,
};
use surfman::chains::{PreserveBuffer, SwapChains, SwapChainsAPI};
use surfman::{
    self, Context, ContextAttributeFlags, ContextAttributes, Device, GLVersion, SurfaceAccess,
    Surface, SurfaceType,
};
use webrender_api::units::DeviceIntSize;
use webrender_api::{
    ExternalImageData, ExternalImageId, ExternalImageType, ImageBufferKind, ImageDescriptor,
    ImageDescriptorFlags, ImageFormat, ImageKey,
};

use crate::context_binding::bind_context;
use crate::resource_retirement::{RetireOwner, RetirementPhase, retire_in_order};
use crate::webgl_limits::GLLimitsDetect;
#[cfg(feature = "webxr")]
use crate::webxr::{WebXRBridge, WebXRBridgeInit};

type GLint = i32;

fn native_uniform_location(location: i32) -> Option<NativeUniformLocation> {
    location.try_into().ok().map(NativeUniformLocation)
}

/// A map which tracks whether a given WebGL context is "busy" ie whether WebRender has
/// currently taken a surface from its [`SwapChain`] for rendering purposes. Contexts will
/// only be deleted once no WebRender instance is using it for rendering. This ensures
/// that all Surfman `Surface`s can be released properly on the [`WebGLThread`].
pub type WebGLContextBusyMap = Arc<RwLock<HashMap<WebGLContextId, usize>>>;

pub(crate) struct GLContextData {
    pub(crate) ctx: Context,
    pub(crate) gl: Rc<glow::Context>,
    device: Rc<Device>,
    state: GLState,
    attributes: GLContextAttributes,
    /// The context should be removed, but the [`WebGLThread`] is currently waiting on
    /// WebRender to finish rendering to the context in order to delete it.
    marked_for_deletion: bool,
    lost: bool,
    native_lost: bool,
    retirement: RetirementPhase,
    retiring_surfaces: Vec<Surface>,
    loss_observer: Option<servo_base::generic_channel::GenericCallback<WebGLContextId>>,
}

fn release_attached_surfaces(device: &Device, ctx: &mut Context, pending: &mut Vec<Surface>) -> Result<(), surfman::Error> {
    if let Some(surface) = device.unbind_surface_from_context(ctx)? { pending.push(surface); }
    while let Some(surface) = pending.first_mut() {
        device.destroy_surface(ctx, surface)?;
        pending.remove(0);
    }
    Ok(())
}

struct ContextRetirement<'a> {
    data: &'a mut GLContextData,
    chains: &'a SwapChains<WebGLContextId, Device>,
    id: WebGLContextId,
}
impl RetireOwner for ContextRetirement<'_> {
    type Error = surfman::Error;
    fn release_resources(&mut self) -> Result<(), Self::Error> {
        self.chains.destroy(self.id, &self.data.device, &mut self.data.ctx)?;
        release_attached_surfaces(&self.data.device, &mut self.data.ctx, &mut self.data.retiring_surfaces)
    }
    fn destroy_native(&mut self) -> Result<(), Self::Error> {
        self.data.device.destroy_context(&mut self.data.ctx)
    }
}

/// Own every acquisition until initialization commits or native rollback completes.
struct PendingCreation {
    device: Rc<Device>,
    ctx: Context,
    returned_surfaces: Vec<Surface>,
    chain_id: Option<WebGLContextId>,
    chains: SwapChains<WebGLContextId, Device>,
    gl: Option<Rc<Gl>>,
    retirement: RetirementPhase,
}
impl PendingCreation {
    fn bind_initial_surface(&mut self, surface: Surface) -> Result<(), String> {
        match self.device.bind_surface_to_context(&mut self.ctx, surface) {
            Ok(()) => Ok(()),
            Err((error, surface)) => {
                self.returned_surfaces.push(surface);
                Err(format!("Failed to bind initial surface: {error:?}"))
            },
        }
    }
    /// Failed creations are unpublished and have no shared GL context or
    /// consumers. Retire their private names with the context, without requiring
    /// another successful GL binding. This never requests a device reset.
    fn abort(&mut self) -> Result<(), surfman::Error> {
        if self.device.supports_context_loss_recovery() {
            let _ = self.device.mark_context_lost(&mut self.ctx);
        }
        self.retire()
    }
    fn retire(&mut self) -> Result<(), surfman::Error> {
        let mut phase = self.retirement;
        let result = retire_in_order(&mut phase, self);
        self.retirement = phase;
        if result.as_ref().is_err_and(surfman::Error::is_context_lost) && self.device.supports_context_loss_recovery() {
            let _ = self.device.mark_context_lost(&mut self.ctx);
        }
        result
    }
}
impl RetireOwner for PendingCreation {
    type Error = surfman::Error;
    fn release_resources(&mut self) -> Result<(), Self::Error> {
        let needs_gl_cleanup = self.chain_id.is_some() || !self.returned_surfaces.is_empty() ||
            self.device.context_surface_info(&self.ctx)?.is_some();
        if needs_gl_cleanup && !self.device.context_is_lost(&self.ctx) {
            self.device.make_context_current(&self.ctx)?;
        }
        if let Some(id) = self.chain_id {
            self.chains.destroy(id, &self.device, &mut self.ctx)?;
            self.chain_id = None;
        }
        // Also own an initial attachment that failed before chain registration.
        release_attached_surfaces(&self.device, &mut self.ctx, &mut self.returned_surfaces)
    }
    fn destroy_native(&mut self) -> Result<(), Self::Error> {
        self.device.destroy_context(&mut self.ctx)
    }
}

#[derive(Debug)]
pub struct GLState {
    webgl_version: WebGLVersion,
    _gl_version: GLVersion,
    requested_flags: ContextAttributeFlags,
    // This is the WebGL view of the color mask
    // The GL view may be different: if the GL context supports alpha
    // but the WebGL context doesn't, then color_write_mask.3 might be true
    // but the GL color write mask is false.
    color_write_mask: [bool; 4],
    clear_color: (f32, f32, f32, f32),
    scissor_test_enabled: bool,
    // The WebGL view of the stencil write mask (see comment re `color_write_mask`)
    stencil_write_mask: (u32, u32),
    // Kept here rather than read back: glGetIntegerv cannot return a full GLuint.
    stencil_value_mask: (u32, u32),
    stencil_test_enabled: bool,
    stencil_clear_value: i32,
    // The WebGL view of the depth write mask (see comment re `color_write_mask`)
    depth_write_mask: bool,
    depth_test_enabled: bool,
    depth_clear_value: f64,
    // True when the default framebuffer is bound to DRAW_FRAMEBUFFER
    drawing_to_default_framebuffer: bool,
    default_vao: Option<NativeVertexArray>,
}

impl GLState {
    // Are we faking having no alpha / depth / stencil?
    fn fake_no_alpha(&self) -> bool {
        self.drawing_to_default_framebuffer &
            !self.requested_flags.contains(ContextAttributeFlags::ALPHA)
    }

    fn fake_no_depth(&self) -> bool {
        self.drawing_to_default_framebuffer &
            !self.requested_flags.contains(ContextAttributeFlags::DEPTH)
    }

    fn fake_no_stencil(&self) -> bool {
        self.drawing_to_default_framebuffer &
            !self
                .requested_flags
                .contains(ContextAttributeFlags::STENCIL)
    }

    // We maintain invariants between the GLState object and the GL state.
    fn restore_invariant(&self, gl: &Gl) {
        self.restore_clear_invariant(gl);
        self.restore_scissor_invariant(gl);
        self.restore_alpha_invariant(gl);
        self.restore_depth_invariant(gl);
        self.restore_stencil_invariant(gl);
    }

    fn restore_clear_invariant(&self, gl: &Gl) {
        let (r, g, b, a) = self.clear_color;
        unsafe {
            gl.clear_color(r, g, b, a);
            gl.clear_depth(self.depth_clear_value);
            gl.clear_stencil(self.stencil_clear_value);
        }
    }

    fn restore_scissor_invariant(&self, gl: &Gl) {
        if self.scissor_test_enabled {
            unsafe { gl.enable(gl::SCISSOR_TEST) };
        } else {
            unsafe { gl.disable(gl::SCISSOR_TEST) };
        }
    }

    fn restore_alpha_invariant(&self, gl: &Gl) {
        let [r, g, b, a] = self.color_write_mask;
        if self.fake_no_alpha() {
            unsafe { gl.color_mask(r, g, b, false) };
        } else {
            unsafe { gl.color_mask(r, g, b, a) };
        }
    }

    fn restore_depth_invariant(&self, gl: &Gl) {
        unsafe {
            if self.fake_no_depth() {
                gl.depth_mask(false);
                gl.disable(gl::DEPTH_TEST);
            } else {
                gl.depth_mask(self.depth_write_mask);
                if self.depth_test_enabled {
                    gl.enable(gl::DEPTH_TEST);
                } else {
                    gl.disable(gl::DEPTH_TEST);
                }
            }
        }
    }

    fn restore_stencil_invariant(&self, gl: &Gl) {
        unsafe {
            if self.fake_no_stencil() {
                gl.stencil_mask(0);
                gl.disable(gl::STENCIL_TEST);
            } else {
                let (f, b) = self.stencil_write_mask;
                gl.stencil_mask_separate(gl::FRONT, f);
                gl.stencil_mask_separate(gl::BACK, b);
                if self.stencil_test_enabled {
                    gl.enable(gl::STENCIL_TEST);
                } else {
                    gl.disable(gl::STENCIL_TEST);
                }
            }
        }
    }
}

impl Default for GLState {
    fn default() -> GLState {
        GLState {
            _gl_version: GLVersion { major: 1, minor: 0 },
            webgl_version: WebGLVersion::WebGL1,
            requested_flags: ContextAttributeFlags::empty(),
            color_write_mask: [true, true, true, true],
            clear_color: (0., 0., 0., 0.),
            scissor_test_enabled: false,
            // GL's initial stencil writemask is all ones.
            stencil_write_mask: (u32::MAX, u32::MAX),
            stencil_value_mask: (u32::MAX, u32::MAX),
            stencil_test_enabled: false,
            stencil_clear_value: 0,
            depth_write_mask: true,
            depth_test_enabled: false,
            depth_clear_value: 1.,
            default_vao: None,
            drawing_to_default_framebuffer: true,
        }
    }
}

/// A WebGLThread manages the life cycle and message multiplexing of
/// a set of WebGLContexts living in the same thread.
pub(crate) struct WebGLThread {
    /// The GPU device.
    device_map: HashMap<PainterId, Rc<Device>>,
    /// Channel used to generate/update or delete `ImageKey`s.
    paint_api: CrossProcessPaintApi,
    /// Map of live WebGLContexts.
    contexts: FxHashMap<WebGLContextId, GLContextData>,
    pending_creations: Vec<PendingCreation>,
    /// Cached information for WebGLContexts.
    cached_context_info: FxHashMap<WebGLContextId, WebGLContextInfo>,
    /// Current bound context.
    bound_context_id: Option<WebGLContextId>,
    /// A [`WebRenderExternalImageIdManager`] used to generate new [`ExternalImageId`]s for our
    /// WebGL contexts.
    external_image_id_manager: WebRenderExternalImageIdManager,
    /// The receiver that will be used for processing WebGL messages.
    receiver: RoutedReceiver<WebGLMsg>,
    /// The receiver that should be used to send WebGL messages for processing.
    sender: GenericSender<WebGLMsg>,
    /// The swap chains used by webrender
    webrender_swap_chains: SwapChains<WebGLContextId, Device>,
    /// The per-painter details of the underlying surfman connection.
    painter_surfman_details_map: PainterSurfmanDetailsMap,
    /// A usage map used to delay the deletion of WebGL contexts until all WebRender
    /// rendering is finished, so that any existing `Surface`s can be properly released.
    busy_webgl_context_map: WebGLContextBusyMap,

    #[cfg(feature = "webxr")]
    /// The bridge to WebXR
    pub webxr_bridge: Option<WebXRBridge>,
}

/// The data required to initialize an instance of the WebGLThread type.
pub(crate) struct WebGLThreadInit {
    pub paint_api: CrossProcessPaintApi,
    pub external_image_id_manager: WebRenderExternalImageIdManager,
    pub sender: GenericSender<WebGLMsg>,
    pub receiver: GenericReceiver<WebGLMsg>,
    pub webrender_swap_chains: SwapChains<WebGLContextId, Device>,
    pub painter_surfman_details_map: PainterSurfmanDetailsMap,
    pub busy_webgl_context_map: WebGLContextBusyMap,
    #[cfg(feature = "webxr")]
    pub webxr_init: WebXRBridgeInit,
}

// A size at which it should be safe to create GL contexts
const SAFE_VIEWPORT_DIMS: [u32; 2] = [1024, 1024];

impl crate::retirement_loop::Worker for WebGLThread {
    type Message = WebGLMsg;
    fn receive(&mut self, timeout: Option<std::time::Duration>) -> crate::retirement_loop::Input<WebGLMsg> {
        use crate::retirement_loop::Input;
        match timeout {
            Some(timeout) => match self.receiver.recv_timeout(timeout) {
                Ok(Ok(message)) => Input::Message(message),
                Ok(Err(_)) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Input::Disconnected,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => Input::Timeout,
            },
            None => match self.receiver.recv() {
                Ok(Ok(message)) => Input::Message(message),
                _ => Input::Disconnected,
            },
        }
    }
    fn handle(&mut self, message: WebGLMsg, closing: bool) -> bool {
        if closing {
            // Refuse new rendering work; allow late consumer returns and XR
            // manager/layer destruction needed to release existing ownership.
            match message {
                WebGLMsg::FinishedRenderingToContext(id) => self.handle_finished_rendering_to_context(id),
                WebGLMsg::RemoveContext(id) => self.remove_webgl_context(id),
                #[cfg(feature = "webxr")]
                WebGLMsg::WebXRCommand(command @ (WebXRCommand::DestroyLayerManager(_) | WebXRCommand::DestroyLayer(..))) => self.handle_webxr_command(command),
                _ => {},
            }
            return false;
        }
        let chan = WebGLChan(self.sender.clone());
        self.handle_msg(message, &chan)
    }
    fn pending(&self) -> bool {
        !self.pending_creations.is_empty() || self.contexts.values().any(|data| data.marked_for_deletion)
            || { #[cfg(feature = "webxr")] { self.webxr_bridge.as_ref().is_some_and(|bridge| bridge.has_pending_retirement()) }
                 #[cfg(not(feature = "webxr"))] { false } }
    }
    fn retire(&mut self, closing: bool) -> bool { self.retry_retirement(closing) }
}

impl WebGLThread {
    /// Create a new instance of WebGLThread.
    pub(crate) fn new(
        WebGLThreadInit {
            paint_api,
            external_image_id_manager: external_images,
            sender,
            receiver,
            webrender_swap_chains,
            painter_surfman_details_map,
            busy_webgl_context_map,
            #[cfg(feature = "webxr")]
            webxr_init,
        }: WebGLThreadInit,
    ) -> Self {
        WebGLThread {
            device_map: Default::default(),
            paint_api,
            contexts: Default::default(),
            pending_creations: Vec::new(),
            cached_context_info: Default::default(),
            bound_context_id: None,
            external_image_id_manager: external_images,
            sender,
            receiver: receiver.route_preserving_errors(),
            webrender_swap_chains,
            painter_surfman_details_map,
            busy_webgl_context_map,
            #[cfg(feature = "webxr")]
            webxr_bridge: Some(WebXRBridge::new(webxr_init)),
        }
    }

    /// Perform all initialization required to run an instance of WebGLThread
    /// in parallel on its own dedicated thread.
    pub(crate) fn run_on_own_thread(init: WebGLThreadInit) -> JoinHandle<()> {
        thread::Builder::new()
            .name("WebGL".to_owned())
            .spawn(move || {
                let mut data = WebGLThread::new(init);
                data.process();
            })
            .expect("Thread spawning failed")
    }

    fn process(&mut self) {
        crate::retirement_loop::run(self);
    }

    fn retry_retirement(&mut self, closing: bool) -> bool {
        self.retry_failed_creations();
        #[cfg(feature = "webxr")]
        {
            let mut bridge = self.webxr_bridge.take();
            if let Some(bridge) = bridge.as_mut() { bridge.retry_retirement(self); }
            self.webxr_bridge = bridge;
        }
        let ids: Vec<_> = self.contexts.iter().filter_map(|(id,data)|
            (closing || data.marked_for_deletion).then_some(*id)).collect();
        for id in ids { self.remove_webgl_context(id); }
        let xr_pending = { #[cfg(feature = "webxr")] { self.webxr_bridge.as_ref().is_some_and(|bridge| bridge.has_pending_retirement()) }
                           #[cfg(not(feature = "webxr"))] { false } };
        if closing { self.contexts.is_empty() && self.pending_creations.is_empty() && !xr_pending }
        else { !crate::retirement_loop::Worker::pending(self) }
    }

    /// Enable GL_POINT_SPRITE and GL_PROGRAM_POINT_SIZE on desktop OpenGL.
    ///
    /// FIXME(nox): Should probably be done by surfman.
    /// FIXME(sagudev): Do we even need to do this?
    fn ensure_point_sprite_and_program_point_size_enabled(gl_context_data: &GLContextData) {
        // Points sprites are enabled by default in OpenGL 3.2 core
        // and in GLES.
        if gl_context_data.gl.version().is_embedded {
            return;
        }

        // Rather than doing version detection, it does not hurt to enable GL_POINT_SPRITE and
        // PROGRAM_POINT_SIZE always.
        const GL_POINT_SPRITE: u32 = 0x8861;
        unsafe { gl_context_data.gl.enable(GL_POINT_SPRITE) };
        let error = unsafe { gl_context_data.gl.get_error() };
        if error != 0 {
            warn!("Error enabling GL point sprites: {error}");
        }

        unsafe { gl_context_data.gl.enable(gl::PROGRAM_POINT_SIZE) };
        let error = unsafe { gl_context_data.gl.get_error() };
        if error != 0 {
            warn!("Error enabling GL program point size: {error}");
        }
    }

    /// Handles a generic WebGLMsg message
    fn handle_msg(&mut self, msg: WebGLMsg, webgl_chan: &WebGLChan) -> bool {
        trace!("processing {:?}", msg);
        match msg {
            WebGLMsg::CreateContext(painter_id, version, size, attributes, result_sender) => {
                let result = self.create_webgl_context(painter_id, version, size, attributes);

                let result = result.and_then(|(id, limits)| {
                        let Some(data) = self.make_current_if_needed(id) else {
                            self.remove_webgl_context(id);
                            return Err("Cannot select newly created WebGL context".into());
                        };

                        Self::ensure_point_sprite_and_program_point_size_enabled(data);

                        let glsl_version = Self::get_glsl_version(&data.gl);
                        let api_type = if data.gl.version().is_embedded {
                            GlType::Gles
                        } else {
                            GlType::Gl
                        };

                        Ok(WebGLCreateContextResult {
                            sender: WebGLMsgSender::new(id, webgl_chan.clone()),
                            limits,
                            glsl_version,
                            api_type,
                        })
                    });
                let created_id = result.as_ref().ok().map(|result| result.sender.context_id());
                if result_sender.send(result).is_err() {
                    if let Some(id) = created_id { self.remove_webgl_context(id); }
                }
            },
            WebGLMsg::ObserveContextLoss(id, callback) => {
                if let Some(data) = self.contexts.get_mut(&id) {
                    if data.lost { let _ = callback.send(id); }
                    else { data.loss_observer = Some(callback); }
                }
            },
            WebGLMsg::LoseContextForTesting(id) => {
                self.notify_context_loss(id);
            },
            WebGLMsg::SetImageKey(ctx_id, image_key) => {
                self.handle_set_image_key(ctx_id, image_key);
            },
            WebGLMsg::ResizeContext(ctx_id, size, sender) => {
                let result = self.resize_webgl_context(ctx_id, size);
                if result.is_err() { self.notify_context_loss(ctx_id); }
                let _ = sender.send(result);
            },
            WebGLMsg::RemoveContext(ctx_id) => {
                self.remove_webgl_context(ctx_id);
            },
            WebGLMsg::WebGLCommand(ctx_id, command, backtrace) => {
                self.handle_webgl_command(ctx_id, command, backtrace);
            },
            WebGLMsg::WebXRCommand(_command) => {
                #[cfg(feature = "webxr")]
                self.handle_webxr_command(_command);
            },
            WebGLMsg::SwapBuffers(swap_ids, canvas_epoch, sent_time) => {
                self.handle_swap_buffers(canvas_epoch, swap_ids, sent_time);
            },
            WebGLMsg::FinishedRenderingToContext(context_id) => {
                self.handle_finished_rendering_to_context(context_id);
            },
            WebGLMsg::ClearPainterResources(painter_id, sender) => {
                self.device_map.remove(&painter_id);
                if let Err(error) = sender.send(()) {
                    warn!("Failed to send response to WebGLMsg::ClearPainterResources ({error})");
                }
            },
            WebGLMsg::Exit => {
                return true;
            },
        }

        false
    }

    fn get_or_create_device_for_painter(
        &mut self,
        painter_id: PainterId,
    ) -> Result<Rc<Device>, String> {
        let entry = self.device_map.entry(painter_id);
        if let Entry::Occupied(entry) = entry {
            return Ok(entry.get().clone());
        }

        // This can happen if the Webview was dropped while one of its ScriptThreads
        // is still issuing asynchronous commands to the WebGL thread.
        let Some(surfman_details) = self.painter_surfman_details_map.get(painter_id) else {
            return Err(format!("No PainterSurfmanDetails found for {painter_id:?}"));
        };

        // Gracefully handle failure to create a device.
        let Ok(device) = surfman_details
            .connection
            .create_device(&surfman_details.adapter)
        else {
            return Err("Could not open WebGL device".into());
        };

        Ok(entry.or_insert(Rc::new(device)).clone())
    }

    #[cfg(feature = "webxr")]
    /// Handles a WebXR message
    fn handle_webxr_command(&mut self, command: WebXRCommand) {
        trace!("processing {:?}", command);
        // Take `webxr_bridge` from the `WebGLThread` in order to avoid a double mutable borrow.
        let Some(mut webxr_bridge) = self.webxr_bridge.take() else {
            return;
        };
        match command {
            WebXRCommand::CreateLayerManager(sender) => {
                let result = webxr_bridge.create_layer_manager(self);
                let _ = sender.send(result);
            },
            WebXRCommand::DestroyLayerManager(manager_id) => {
                webxr_bridge.destroy_layer_manager(manager_id);
            },
            WebXRCommand::CreateLayer(manager_id, context_id, layer_init, viewports, sender) => {
                let result = if self.contexts.get(&context_id.into())
                    .is_none_or(|data| data.lost || data.marked_for_deletion)
                {
                    Err(webxr_api::Error::NoMatchingDevice)
                } else {
                    webxr_bridge.create_layer(manager_id, self, context_id, layer_init, viewports)
                };
                let _ = sender.send(result);
            },
            WebXRCommand::DestroyLayer(manager_id, context_id, layer_id) => {
                webxr_bridge.destroy_layer(manager_id, self, context_id, layer_id);
            },
            WebXRCommand::BeginFrame(manager_id, layers, sender) => {
                let result = webxr_bridge.begin_frame(manager_id, self, &layers[..]);
                let _ = sender.send(result);
            },
            WebXRCommand::EndFrame(manager_id, layers, sender) => {
                let result = webxr_bridge.end_frame(manager_id, self, &layers[..]);
                let _ = sender.send(result);
            },
        }

        webxr_bridge.retry_retirement(self);
        self.webxr_bridge.replace(webxr_bridge);
    }

    fn device_for_context(&self, context_id: WebGLContextId) -> Rc<Device> {
        self.maybe_device_for_context(context_id)
            .expect("Should be called with a valid WebGLContextId")
    }

    /// A function like `Self::device_for_context`, except that it does not panic if the context
    /// cannot be found. This is useful for WebXR, which might try to access WebGL contexts after
    /// they have been cleaned up.
    pub(crate) fn maybe_device_for_context(
        &self,
        context_id: WebGLContextId,
    ) -> Option<Rc<Device>> {
        self.contexts
            .get(&context_id)
            .map(|context| context.device.clone())
    }

    fn notify_context_loss(&mut self, id: WebGLContextId) {
        let Some(data) = self.contexts.get_mut(&id) else { return; };
        if data.lost { return; }
        data.lost = true;
        if let Some(observer) = data.loss_observer.take() { let _ = observer.send(id); }
    }

    fn native_context_failed(&mut self, id: WebGLContextId, error: &surfman::Error) {
        self.bound_context_id = None;
        let native_loss = error.is_context_lost();
        if native_loss {
            if let Some(data) = self.contexts.get_mut(&id) {
                if data.device.supports_context_loss_recovery() &&
                    data.device.mark_context_lost(&mut data.ctx).is_ok()
                {
                    data.native_lost = true;
                }
            }
        }
        self.notify_context_loss(id);
    }

    /// Handles a WebGLCommand for a specific WebGLContext
    fn handle_webgl_command(
        &mut self,
        context_id: WebGLContextId,
        command: WebGLCommand,
        backtrace: WebGLCommandBacktrace,
    ) {
        if self.contexts.get(&context_id).is_some_and(|data| data.lost || data.marked_for_deletion) { return; }
        if self.cached_context_info.get_mut(&context_id).is_none() {
            return;
        }
        let data = self.make_current_if_needed_mut(context_id);
        if let Some(data) = data {
            let completed = WebGLImpl::apply(
                &data.device,
                &data.ctx,
                &data.gl,
                &mut data.state,
                &data.attributes,
                command,
                backtrace,
            );
            if !completed { self.notify_context_loss(context_id); }
        }
    }

    fn retry_failed_creations(&mut self) {
        if self.pending_creations.is_empty() { return; }
        for mut creation in std::mem::take(&mut self.pending_creations) {
            if creation.retire().is_err() { self.pending_creations.push(creation); }
        }
        self.bound_context_id = None;
    }

    /// Acquire a complete context transactionally, including its native rollback owner.
    fn create_webgl_context(
        &mut self,
        painter_id: PainterId,
        webgl_version: WebGLVersion,
        requested_size: Size2D<u32>,
        attributes: GLContextAttributes,
    ) -> Result<(WebGLContextId, webgl::GLLimits), String> {
        self.retry_failed_creations();
        self.bound_context_id = None;
        let Some(details) = self.painter_surfman_details_map.get(painter_id) else {
            return Err(format!("PainterSurfmanDetails not found for {painter_id:?}"));
        };
        let api_type = match details.connection.gl_api() {
            surfman::GLApi::GL => GlType::Gl,
            surfman::GLApi::GLES => GlType::Gles,
        };
        let requested_flags = attributes.to_surfman_context_attribute_flags(webgl_version, api_type);
        let flags = requested_flags | ContextAttributeFlags::ALPHA |
            ContextAttributeFlags::DEPTH | ContextAttributeFlags::STENCIL;
        let device = self.get_or_create_device_for_painter(painter_id)?;
        let descriptor = device.create_context_descriptor(&ContextAttributes {
            version: webgl_version.to_surfman_version(api_type), flags,
        }).map_err(|error| format!("Failed to create context descriptor: {error:?}"))?;
        let ctx = device.create_context(&descriptor, None)
            .map_err(|error| format!("Failed to create the GL context: {error:?}"))?;
        let id = WebGLContextId(self.external_image_id_manager.next_id(WebRenderImageHandlerType::WebGl).0);
        let safe_size = Size2D::new(requested_size.width.min(SAFE_VIEWPORT_DIMS[0]).max(1),
            requested_size.height.min(SAFE_VIEWPORT_DIMS[1]).max(1));
        let access = self.surface_access();
        let mut creation = PendingCreation {
            device: device.clone(), ctx, returned_surfaces: Vec::new(), chain_id: None,
            chains: self.webrender_swap_chains.clone(), gl: None, retirement: RetirementPhase::default(),
        };
        let initialization = (|| {
            let surface = device.create_surface(&creation.ctx, access,
                SurfaceType::Generic { size: safe_size.to_i32() })
                .map_err(|error| format!("Failed to create initial surface: {error:?}"))?;
            creation.bind_initial_surface(surface)?;
            device.make_context_current(&creation.ctx)
                .map_err(|error| format!("Failed to select new context: {error:?}"))?;
            creation.chains.create_attached_swap_chain(id, &*device, &mut creation.ctx, access)
                .map_err(|error| format!("Failed to create swap chain: {error:?}"))?;
            creation.chain_id = Some(id);
            let chain = creation.chains.get(id).ok_or_else(|| "Created swap chain is unavailable".to_owned())?;
            let gl = Rc::new(unsafe {
                Gl::try_from_loader_function(|name| device.get_proc_address(&creation.ctx, name))
            }.map_err(|error| format!("Failed to initialize GL metadata: {error}"))?);
            creation.gl = Some(gl.clone());
            let limits = GLLimits::detect(&gl, webgl_version);
            let size = clamp_viewport(&gl, requested_size);
            if safe_size != size {
                chain.resize(&device, &mut creation.ctx, size.to_i32())
                    .map_err(|error| format!("Failed to resize initial swap chain: {error:?}"))?;
            }
            let descriptor = device.context_descriptor(&creation.ctx);
            let gl_version = device.context_descriptor_attributes(&descriptor).version;
            let has_alpha = requested_flags.contains(ContextAttributeFlags::ALPHA);
            device.make_context_current(&creation.ctx)
                .map_err(|error| format!("Failed to select initialized context: {error:?}"))?;
            let framebuffer = device.context_surface_info(&creation.ctx)
                .map_err(|error| format!("Failed to query initial surface: {error:?}"))?
                .ok_or_else(|| "Initial context surface is unavailable".to_owned())?.framebuffer_object;
            unsafe {
                gl.bind_framebuffer(gl::FRAMEBUFFER, framebuffer);
                gl.viewport(0, 0, size.width as i32, size.height as i32);
                gl.scissor(0, 0, size.width as i32, size.height as i32);
                gl.clear_color(0., 0., 0., !has_alpha as u32 as f32);
                gl.clear_depth(1.); gl.clear_stencil(0);
                gl.clear(gl::COLOR_BUFFER_BIT | gl::DEPTH_BUFFER_BIT | gl::STENCIL_BUFFER_BIT);
                gl.clear_color(0., 0., 0., 0.);
                debug_assert_eq!(gl.get_error(), gl::NO_ERROR);
            }
            let default_vao = if let Some(vao) = WebGLImpl::create_vertex_array(&gl) {
                unsafe { gl.bind_vertex_array(Some(vao.glow())) };
                Some(vao.glow())
            } else { None };
            let state = GLState { _gl_version: gl_version, webgl_version,
                requested_flags, default_vao, ..Default::default() };
            state.restore_invariant(&gl);
            Ok::<_, String>((limits, size, has_alpha, state, gl))
        })();
        let (limits, size, has_alpha, state, gl) = match initialization {
            Ok(ready) => ready,
            Err(error) => {
                if let Err(cleanup_error) = creation.abort() {
                    warn!("Retaining failed WebGL creation for cleanup: {cleanup_error:?}");
                    self.pending_creations.push(creation);
                }
                self.bound_context_id = None;
                return Err(error);
            },
        };
        // Commit only after every fallible initialization step has succeeded.
        debug_assert!(creation.returned_surfaces.is_empty());
        let PendingCreation { ctx, device, .. } = creation;
        self.contexts.insert(id, GLContextData {
            ctx, device, gl, state, attributes, marked_for_deletion: false,
            lost: false, native_lost: false, retirement: RetirementPhase::default(),
            retiring_surfaces: Vec::new(), loss_observer: None,
        });
        self.cached_context_info.insert(id, WebGLContextInfo {
            image_key: None, size: size.to_i32(), alpha: has_alpha,
        });
        Ok((id, limits))
    }

    /// Resizes a WebGLContext
    fn resize_webgl_context(
        &mut self,
        context_id: WebGLContextId,
        requested_size: Size2D<u32>,
    ) -> Result<(), String> {
        if self.contexts.get(&context_id).is_some_and(|data| data.lost || data.marked_for_deletion) {
            return Err("WebGL context lost".into());
        }
        if !self.ensure_context_current(context_id) {
            return Err("Cannot make WebGL context current".into());
        }

        let Some(data) = self.contexts.get_mut(&context_id) else {
            return Err("Missing WebGL context!".into());
        };

        let size = clamp_viewport(&data.gl, requested_size);
        debug_assert_eq!(unsafe { data.gl.get_error() }, gl::NO_ERROR);

        // Check to see if any of the current framebuffer bindings are the surface we're about to
        // throw out. If so, we'll have to reset them after destroying the surface.
        let framebuffer_rebinding_info =
            FramebufferRebindingInfo::detect(&data.device, &data.ctx, &data.gl)
                .map_err(|error| format!("Cannot capture resized framebuffer state: {error:?}"))?;

        // Resize the swap chains
        if let Some(swap_chain) = self.webrender_swap_chains.get(context_id) {
            let alpha = data
                .state
                .requested_flags
                .contains(ContextAttributeFlags::ALPHA);
            let clear_color = [0.0, 0.0, 0.0, !alpha as i32 as f32];
            swap_chain
                .resize(&data.device, &mut data.ctx, size.to_i32())
                .map_err(|err| format!("Failed to resize swap chain: {:?}", err))?;
            swap_chain
                .clear_surface(&data.device, &mut data.ctx, &data.gl, clear_color)
                .map_err(|err| format!("Failed to clear resized swap chain: {:?}", err))?;
        } else {
            error!("Failed to find swap chain");
        }

        // Reset framebuffer bindings as appropriate.
        framebuffer_rebinding_info.apply(&data.device, &data.ctx, &data.gl)
            .map_err(|error| format!("Cannot restore resized framebuffer state: {error:?}"))?;
        debug_assert_eq!(unsafe { data.gl.get_error() }, gl::NO_ERROR);

        let has_alpha = data
            .state
            .requested_flags
            .contains(ContextAttributeFlags::ALPHA);
        self.update_webrender_image_for_context(context_id, size.to_i32(), has_alpha, None);

        Ok(())
    }

    /// Note that rendering has finished in WebRender for this context. If the context
    /// is marked for deletion, it will now be deleted.
    fn handle_finished_rendering_to_context(&mut self, context_id: WebGLContextId) {
        let marked_for_deletion = self
            .contexts
            .get(&context_id)
            .is_some_and(|context_data| context_data.marked_for_deletion);
        if marked_for_deletion {
            self.remove_webgl_context(context_id);
        }
    }

    /// Removes a WebGLContext and releases attached resources.
    fn remove_webgl_context(&mut self, context_id: WebGLContextId) {
        {
            let mut busy_webgl_context_map = self.busy_webgl_context_map.write();
            let entry = busy_webgl_context_map.entry(context_id);
            match entry {
                Entry::Vacant(..) => {},
                Entry::Occupied(occupied_entry) if *occupied_entry.get() > 0 => {
                    // WebRender is in the process of rendering this WebGL context, so wait until it
                    // finishes in order to release it.
                    if let Some(context_data) = self.contexts.get_mut(&context_id) {
                        context_data.marked_for_deletion = true;
                    }
                    return;
                },
                Entry::Occupied(occupied_entry) => {
                    occupied_entry.remove();
                },
            }
        }

        // Release webrender image keys.
        if let Some(image_key) = self
            .cached_context_info
            .remove(&context_id)
            .and_then(|info| info.image_key)
        {
            self.paint_api.delete_image(image_key);
        }

        if !self.contexts.contains_key(&context_id) {
            return;
        };

        if let Some(data) = self.contexts.get_mut(&context_id) {
            data.marked_for_deletion = true;
        }
        if self.contexts.get(&context_id).is_some_and(|data| data.retirement.needs_resources()) {
        // A loss discovered while selecting the context can retire immediately
        // through the native path; a transient non-loss failure keeps its owner.
        if !self.native_context_lost(context_id) && !self.ensure_context_current(context_id)
            && !self.native_context_lost(context_id)
        { return; }

        // Destroy WebXR layers associated with this context
        #[cfg(feature = "webxr")]
        {
            // We must temporarily take the WebXRBridge, as we are passing self to a
            // method on the bridge.
            let mut webxr_bridge = self.webxr_bridge.take();
            let released = webxr_bridge.as_mut().is_none_or(|bridge| {
                bridge.destroy_all_layers(self, context_id.into())
            });
            self.webxr_bridge = webxr_bridge;
            if !released {
                if let Some(data) = self.contexts.get_mut(&context_id) {
                    data.marked_for_deletion = true;
                }
                return;
            }
        }

        }

        let Some(data) = self.contexts.get_mut(&context_id) else { return; };
        self.bound_context_id = None;
        let mut phase = data.retirement;
        let result = retire_in_order(&mut phase, &mut ContextRetirement {
            data: &mut *data, chains: &self.webrender_swap_chains, id: context_id,
        });
        data.retirement = phase;
        if let Err(error) = result {
            warn!("Deferring WebGL context retirement {context_id:?} at {phase:?}: {error:?}");
            self.native_context_failed(context_id, &error);
            return;
        }
        self.contexts.remove(&context_id);
    }

    fn handle_swap_buffers(
        &mut self,
        canvas_epoch: Option<Epoch>,
        context_ids: Vec<WebGLContextId>,
        _sent_time: u64,
    ) {
        for context_id in context_ids {
            if self.contexts.get(&context_id)
                .is_none_or(|data| data.lost || data.marked_for_deletion)
            { continue; }
            if !self.ensure_context_current(context_id) { continue; }
            let operation = (|| -> Result<_, surfman::Error> {
                let data = self.contexts.get_mut(&context_id).ok_or(surfman::Error::Failed)?;
                let swap_chain = self.webrender_swap_chains.get(context_id)
                    .ok_or(surfman::Error::Failed)?;
                let pending_error = unsafe { data.gl.get_error() };
                debug_assert_eq!(pending_error, gl::NO_ERROR);
                if pending_error != gl::NO_ERROR {
                    warn!("Discarding GL error {pending_error:#x} before swapping {context_id:?}");
                }
                let framebuffer = FramebufferRebindingInfo::detect(&data.device, &data.ctx, &data.gl)?;
                swap_chain.swap_buffers(&data.device, &mut data.ctx,
                    if data.attributes.preserve_drawing_buffer {
                        PreserveBuffer::Yes(&data.gl)
                    } else { PreserveBuffer::No })?;
                let alpha = data.state.requested_flags.contains(ContextAttributeFlags::ALPHA);
                if !data.attributes.preserve_drawing_buffer {
                    swap_chain.clear_surface(&data.device, &mut data.ctx, &data.gl,
                        [0., 0., 0., !alpha as i32 as f32])?;
                }
                framebuffer.apply(&data.device, &data.ctx, &data.gl)?;
                let info = data.device.context_surface_info(&data.ctx)?.ok_or(surfman::Error::Failed)?;
                Ok((info.size, alpha))
            })();
            match operation {
                Ok((size, alpha)) => self.update_webrender_image_for_context(context_id, size, alpha, canvas_epoch),
                Err(error) => {
                    warn!("WebGL frame failed for {context_id:?}: {error:?}");
                    self.native_context_failed(context_id, &error);
                },
            }
        }
    }

    /// Which access mode to use
    fn surface_access(&self) -> SurfaceAccess {
        SurfaceAccess::GPUOnly
    }

    fn ensure_context_current(&mut self, context_id: WebGLContextId) -> bool {
        let Some(data) = self.contexts.get(&context_id) else { return false; };
        if !data.retirement.needs_resources() { return false; }
        let result = bind_context(&mut self.bound_context_id, context_id, || {
            if data.native_lost || data.device.context_is_lost(&data.ctx) {
                Err(surfman::Error::MakeCurrentFailed(surfman::WindowingApiError::ContextLost))
            } else { Ok(()) }
        }, || data.device.make_context_current(&data.ctx));
        if let Err(error) = result {
            warn!("Cannot select WebGL context {context_id:?}: {error:?}");
            self.native_context_failed(context_id, &error);
            return false;
        }
        true
    }

    /// Return a context only after its native binding has succeeded.
    pub(crate) fn make_current_if_needed(&mut self, context_id: WebGLContextId) -> Option<&GLContextData> {
        if !self.ensure_context_current(context_id) { return None; }
        self.contexts.get(&context_id)
    }

    /// Mutable counterpart of `make_current_if_needed`.
    pub(crate) fn make_current_if_needed_mut(&mut self, context_id: WebGLContextId) -> Option<&mut GLContextData> {
        if !self.ensure_context_current(context_id) { return None; }
        self.contexts.get_mut(&context_id)
    }

    pub(crate) fn native_context_lost(&self, context_id: WebGLContextId) -> bool {
        self.contexts.get(&context_id).is_some_and(|data| data.native_lost || data.device.context_is_lost(&data.ctx))
    }

    #[cfg(feature = "webxr")]
    pub(crate) fn context_for_retirement(&mut self, context_id: WebGLContextId) -> Option<&mut Context> {
        self.contexts.get_mut(&context_id).filter(|data| data.native_lost || data.device.context_is_lost(&data.ctx)).map(|data| &mut data.ctx)
    }

    /// Tell WebRender to invalidate any cached tiles for a given `WebGLContextId`
    /// when the underlying surface has changed e.g due to resize or buffer swap
    fn update_webrender_image_for_context(
        &mut self,
        context_id: WebGLContextId,
        size: Size2D<i32>,
        has_alpha: bool,
        canvas_epoch: Option<Epoch>,
    ) {
        let image_data = self.external_image_data(context_id);
        let info = self.cached_context_info.get_mut(&context_id).unwrap();
        info.size = size;
        info.alpha = has_alpha;

        if let Some(image_key) = info.image_key {
            self.paint_api.update_image(
                image_key,
                info.image_descriptor(),
                image_data,
                canvas_epoch,
            );
        }
    }

    /// Helper function to create a `ImageData::External` instance.
    fn external_image_data(&self, context_id: WebGLContextId) -> SerializableImageData {
        // TODO(pcwalton): Add `GL_TEXTURE_EXTERNAL_OES`?
        let device = self.device_for_context(context_id);
        let image_buffer_kind = match device.surface_gl_texture_target() {
            gl::TEXTURE_RECTANGLE => ImageBufferKind::TextureRect,
            _ => ImageBufferKind::Texture2D,
        };

        let data = ExternalImageData {
            id: ExternalImageId(context_id.0),
            channel_index: 0,
            image_type: ExternalImageType::TextureHandle(image_buffer_kind),
            normalized_uvs: false,
        };
        SerializableImageData::External(data)
    }

    /// Gets the GLSL Version supported by a GLContext.
    fn get_glsl_version(gl: &Gl) -> WebGLSLVersion {
        let version = unsafe { gl.get_parameter_string(gl::SHADING_LANGUAGE_VERSION) };
        // Fomat used by SHADING_LANGUAGE_VERSION query : major.minor[.release] [vendor info]
        let mut values = version.split(&['.', ' '][..]);
        let major = values
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(1);
        let minor = values
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(20);

        WebGLSLVersion { major, minor }
    }

    fn handle_set_image_key(&mut self, context_id: WebGLContextId, image_key: ImageKey) {
        if self.contexts.get(&context_id)
            .is_none_or(|data| data.lost || data.marked_for_deletion)
        {
            self.paint_api.delete_image(image_key);
            return;
        }
        let external_image_data = self.external_image_data(context_id);
        let Some(info) = self.cached_context_info.get_mut(&context_id) else {
            self.paint_api.delete_image(image_key);
            return;
        };

        if let Some(old_image_key) = info.image_key.replace(image_key) {
            self.paint_api.delete_image(old_image_key);
            return;
        }

        self.paint_api.add_image(
            image_key,
            info.image_descriptor(),
            external_image_data,
            false,
        );
    }
}

/// Helper struct to store cached WebGLContext information.
struct WebGLContextInfo {
    image_key: Option<ImageKey>,
    size: Size2D<i32>,
    alpha: bool,
}

impl WebGLContextInfo {
    /// Helper function to create a `ImageDescriptor`.
    fn image_descriptor(&self) -> ImageDescriptor {
        let mut flags = ImageDescriptorFlags::empty();
        flags.set(ImageDescriptorFlags::IS_OPAQUE, !self.alpha);
        ImageDescriptor {
            size: DeviceIntSize::new(self.size.width, self.size.height),
            stride: None,
            format: ImageFormat::BGRA8,
            offset: 0,
            flags,
        }
    }
}

/// WebGL Commands Implementation
pub struct WebGLImpl;

impl WebGLImpl {
    pub fn apply(
        device: &Device,
        ctx: &Context,
        gl: &Gl,
        state: &mut GLState,
        attributes: &GLContextAttributes,
        command: WebGLCommand,
        _backtrace: WebGLCommandBacktrace,
    ) -> bool {
        // Ensure there are no pending GL errors from other parts of the pipeline.
        debug_assert_eq!(unsafe { gl.get_error() }, gl::NO_ERROR);

        match command {
            WebGLCommand::GetContextAttributes(ref sender) => sender.send_or_ignore(*attributes),
            WebGLCommand::ActiveTexture(target) => unsafe { gl.active_texture(target) },
            WebGLCommand::AttachShader(program_id, shader_id) => unsafe {
                gl.attach_shader(program_id.glow(), shader_id.glow())
            },
            WebGLCommand::DetachShader(program_id, shader_id) => unsafe {
                gl.detach_shader(program_id.glow(), shader_id.glow())
            },
            WebGLCommand::BindAttribLocation(program_id, index, ref name) => unsafe {
                gl.bind_attrib_location(program_id.glow(), index, &to_name_in_compiled_shader(name))
            },
            WebGLCommand::BlendColor(r, g, b, a) => unsafe { gl.blend_color(r, g, b, a) },
            WebGLCommand::BlendEquation(mode) => unsafe { gl.blend_equation(mode) },
            WebGLCommand::BlendEquationSeparate(mode_rgb, mode_alpha) => unsafe {
                gl.blend_equation_separate(mode_rgb, mode_alpha)
            },
            WebGLCommand::BlendFunc(src, dest) => unsafe { gl.blend_func(src, dest) },
            WebGLCommand::BlendFuncSeparate(src_rgb, dest_rgb, src_alpha, dest_alpha) => unsafe {
                gl.blend_func_separate(src_rgb, dest_rgb, src_alpha, dest_alpha)
            },
            WebGLCommand::BufferData(buffer_type, ref receiver, usage) => {
                let Ok(data) = receiver.recv() else { return false; };
                unsafe { gl.buffer_data_u8_slice(buffer_type, &data, usage) }
            },
            WebGLCommand::BufferSubData(buffer_type, offset, ref receiver) => {
                let Ok(data) = receiver.recv() else { return false; };
                unsafe { gl.buffer_sub_data_u8_slice(buffer_type, offset as i32, &data) }
            },
            WebGLCommand::CopyBufferSubData(src, dst, src_offset, dst_offset, size) => {
                unsafe {
                    gl.copy_buffer_sub_data(
                        src,
                        dst,
                        src_offset as i32,
                        dst_offset as i32,
                        size as i32,
                    )
                };
            },
            WebGLCommand::GetBufferSubData(buffer_type, offset, length, ref sender) => unsafe {
                let ptr = gl.map_buffer_range(
                    buffer_type,
                    offset as i32,
                    length as i32,
                    gl::MAP_READ_BIT,
                );
                let data: &[u8] = slice::from_raw_parts(ptr as _, length);
                let buffer = GenericSharedMemory::from_bytes(data);
                sender.send_or_ignore(buffer);
                gl.unmap_buffer(buffer_type);
            },
            WebGLCommand::Clear(mask) => {
                unsafe { gl.clear(mask) };
            },
            WebGLCommand::ClearColor(r, g, b, a) => {
                state.clear_color = (r, g, b, a);
                unsafe { gl.clear_color(r, g, b, a) };
            },
            WebGLCommand::ClearDepth(depth) => {
                let value = depth.clamp(0., 1.) as f64;
                state.depth_clear_value = value;
                unsafe { gl.clear_depth(value) }
            },
            WebGLCommand::ClearStencil(stencil) => {
                state.stencil_clear_value = stencil;
                unsafe { gl.clear_stencil(stencil) };
            },
            WebGLCommand::ColorMask(r, g, b, a) => {
                state.color_write_mask = [r, g, b, a];
                state.restore_alpha_invariant(gl);
            },
            WebGLCommand::CopyTexImage2D(
                target,
                level,
                internal_format,
                x,
                y,
                width,
                height,
                border,
            ) => unsafe {
                gl.copy_tex_image_2d(target, level, internal_format, x, y, width, height, border)
            },
            WebGLCommand::CopyTexSubImage2D(
                target,
                level,
                xoffset,
                yoffset,
                x,
                y,
                width,
                height,
            ) => unsafe {
                gl.copy_tex_sub_image_2d(target, level, xoffset, yoffset, x, y, width, height)
            },
            WebGLCommand::CullFace(mode) => unsafe { gl.cull_face(mode) },
            WebGLCommand::DepthFunc(func) => unsafe { gl.depth_func(func) },
            WebGLCommand::DepthMask(flag) => {
                state.depth_write_mask = flag;
                state.restore_depth_invariant(gl);
            },
            WebGLCommand::DepthRange(near, far) => unsafe {
                gl.depth_range(near.clamp(0., 1.) as f64, far.clamp(0., 1.) as f64)
            },
            WebGLCommand::Disable(cap) => match cap {
                gl::SCISSOR_TEST => {
                    state.scissor_test_enabled = false;
                    state.restore_scissor_invariant(gl);
                },
                gl::DEPTH_TEST => {
                    state.depth_test_enabled = false;
                    state.restore_depth_invariant(gl);
                },
                gl::STENCIL_TEST => {
                    state.stencil_test_enabled = false;
                    state.restore_stencil_invariant(gl);
                },
                _ => unsafe { gl.disable(cap) },
            },
            WebGLCommand::Enable(cap) => match cap {
                gl::SCISSOR_TEST => {
                    state.scissor_test_enabled = true;
                    state.restore_scissor_invariant(gl);
                },
                gl::DEPTH_TEST => {
                    state.depth_test_enabled = true;
                    state.restore_depth_invariant(gl);
                },
                gl::STENCIL_TEST => {
                    state.stencil_test_enabled = true;
                    state.restore_stencil_invariant(gl);
                },
                _ => unsafe { gl.enable(cap) },
            },
            WebGLCommand::FramebufferRenderbuffer(target, attachment, renderbuffertarget, rb) => {
                let attach = |attachment| unsafe {
                    gl.framebuffer_renderbuffer(
                        target,
                        attachment,
                        renderbuffertarget,
                        rb.map(WebGLRenderbufferId::glow),
                    )
                };
                if attachment == gl::DEPTH_STENCIL_ATTACHMENT {
                    attach(gl::DEPTH_ATTACHMENT);
                    attach(gl::STENCIL_ATTACHMENT);
                } else {
                    attach(attachment);
                }
            },
            WebGLCommand::FramebufferTexture2D(target, attachment, textarget, texture, level) => {
                let attach = |attachment| unsafe {
                    gl.framebuffer_texture_2d(
                        target,
                        attachment,
                        textarget,
                        texture.map(WebGLTextureId::glow),
                        level,
                    )
                };
                if attachment == gl::DEPTH_STENCIL_ATTACHMENT {
                    attach(gl::DEPTH_ATTACHMENT);
                    attach(gl::STENCIL_ATTACHMENT);
                } else {
                    attach(attachment)
                }
            },
            WebGLCommand::FrontFace(mode) => unsafe { gl.front_face(mode) },
            WebGLCommand::DisableVertexAttribArray(attrib_id) => unsafe {
                gl.disable_vertex_attrib_array(attrib_id)
            },
            WebGLCommand::EnableVertexAttribArray(attrib_id) => unsafe {
                gl.enable_vertex_attrib_array(attrib_id)
            },
            WebGLCommand::Hint(name, val) => unsafe { gl.hint(name, val) },
            WebGLCommand::LineWidth(width) => {
                unsafe { gl.line_width(width) };
                // In OpenGL Core Profile >3.2, any non-1.0 value will generate INVALID_VALUE.
                if width != 1.0 {
                    let _ = unsafe { gl.get_error() };
                }
            },
            WebGLCommand::PixelStorei(name, val) => unsafe { gl.pixel_store_i32(name, val) },
            WebGLCommand::PolygonOffset(factor, units) => unsafe {
                gl.polygon_offset(factor, units)
            },
            WebGLCommand::ReadPixels(rect, format, pixel_type, ref sender) => {
                let len = bytes_per_type(pixel_type) *
                    components_per_format(format) *
                    rect.size.area() as usize;
                let mut pixels = vec![0; len];
                unsafe {
                    // We don't want any alignment padding on pixel rows.
                    gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                    gl.read_pixels(
                        rect.origin.x as i32,
                        rect.origin.y as i32,
                        rect.size.width as i32,
                        rect.size.height as i32,
                        format,
                        pixel_type,
                        glow::PixelPackData::Slice(Some(&mut pixels)),
                    )
                };
                let alpha_mode = match (attributes.alpha, attributes.premultiplied_alpha) {
                    (true, premultiplied) => SnapshotAlphaMode::Transparent { premultiplied },
                    (false, _) => SnapshotAlphaMode::Opaque,
                };
                sender
                    .send_or_ignore((GenericSharedMemory::from_vec(pixels), alpha_mode));
            },
            WebGLCommand::ReadPixelsPP(rect, format, pixel_type, offset) => unsafe {
                gl.read_pixels(
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                    format,
                    pixel_type,
                    glow::PixelPackData::BufferOffset(offset as u32),
                );
            },
            WebGLCommand::RenderbufferStorage(target, format, width, height) => unsafe {
                gl.renderbuffer_storage(target, format, width, height)
            },
            WebGLCommand::RenderbufferStorageMultisample(
                target,
                samples,
                format,
                width,
                height,
            ) => unsafe {
                gl.renderbuffer_storage_multisample(target, samples, format, width, height)
            },
            WebGLCommand::SampleCoverage(value, invert) => unsafe {
                gl.sample_coverage(value, invert)
            },
            WebGLCommand::Scissor(x, y, width, height) => {
                // FIXME(nox): Kinda unfortunate that some u32 values could
                // end up as negative numbers here, but I don't even think
                // that can happen in the real world.
                unsafe { gl.scissor(x, y, width as i32, height as i32) };
            },
            WebGLCommand::StencilFunc(func, ref_, mask) => {
                state.stencil_value_mask = (mask, mask);
                unsafe { gl.stencil_func(func, ref_, mask) }
            },
            WebGLCommand::StencilFuncSeparate(face, func, ref_, mask) => {
                if face != gl::BACK {
                    state.stencil_value_mask.0 = mask;
                }
                if face != gl::FRONT {
                    state.stencil_value_mask.1 = mask;
                }
                unsafe { gl.stencil_func_separate(face, func, ref_, mask) }
            },
            WebGLCommand::StencilMask(mask) => {
                state.stencil_write_mask = (mask, mask);
                state.restore_stencil_invariant(gl);
            },
            WebGLCommand::StencilMaskSeparate(face, mask) => {
                match face {
                    gl::FRONT => state.stencil_write_mask.0 = mask,
                    gl::BACK => state.stencil_write_mask.1 = mask,
                    gl::FRONT_AND_BACK => state.stencil_write_mask = (mask, mask),
                    _ => debug_assert!(
                        false,
                        "stencilMaskSeparate face {face:#x} should have been rejected by the DOM"
                    ),
                }
                state.restore_stencil_invariant(gl);
            },
            WebGLCommand::StencilOp(fail, zfail, zpass) => unsafe {
                gl.stencil_op(fail, zfail, zpass)
            },
            WebGLCommand::StencilOpSeparate(face, fail, zfail, zpass) => unsafe {
                gl.stencil_op_separate(face, fail, zfail, zpass)
            },
            WebGLCommand::GetRenderbufferParameter(target, pname, ref chan) => {
                Self::get_renderbuffer_parameter(gl, target, pname, chan)
            },
            WebGLCommand::CreateTransformFeedback(ref sender) => {
                let value = unsafe { gl.create_transform_feedback() }.ok();
                sender
                    .send_or_ignore(value.map(|ntf| ntf.0.get()).unwrap_or_default())
            },
            WebGLCommand::DeleteTransformFeedback(id) => {
                if let Some(tf) = NonZeroU32::new(id) {
                    unsafe { gl.delete_transform_feedback(NativeTransformFeedback(tf)) };
                }
            },
            WebGLCommand::IsTransformFeedback(id, ref sender) => {
                let value = NonZeroU32::new(id)
                    .map(|id| unsafe { gl.is_transform_feedback(NativeTransformFeedback(id)) })
                    .unwrap_or_default();
                sender.send_or_ignore(value)
            },
            WebGLCommand::BindTransformFeedback(target, id) => {
                unsafe {
                    gl.bind_transform_feedback(
                        target,
                        NonZeroU32::new(id).map(NativeTransformFeedback),
                    )
                };
            },
            WebGLCommand::BeginTransformFeedback(mode) => {
                unsafe { gl.begin_transform_feedback(mode) };
            },
            WebGLCommand::EndTransformFeedback() => {
                unsafe { gl.end_transform_feedback() };
            },
            WebGLCommand::PauseTransformFeedback() => {
                unsafe { gl.pause_transform_feedback() };
            },
            WebGLCommand::ResumeTransformFeedback() => {
                unsafe { gl.resume_transform_feedback() };
            },
            WebGLCommand::GetTransformFeedbackVarying(program, index, ref sender) => {
                let ActiveTransformFeedback { size, tftype, name } =
                    unsafe { gl.get_transform_feedback_varying(program.glow(), index) }.unwrap();
                // We need to split, because the name starts with '_u' prefix.
                let name = from_name_in_compiled_shader(&name);
                sender.send_or_ignore((size, tftype, name));
            },
            WebGLCommand::TransformFeedbackVaryings(program, ref varyings, buffer_mode) => {
                let varyings: Vec<String> = varyings
                    .iter()
                    .map(|varying| to_name_in_compiled_shader(varying))
                    .collect();
                let varyings_refs: Vec<&str> = varyings.iter().map(String::as_ref).collect();
                unsafe {
                    gl.transform_feedback_varyings(
                        program.glow(),
                        varyings_refs.as_slice(),
                        buffer_mode,
                    )
                };
            },
            WebGLCommand::GetFramebufferAttachmentParameter(
                target,
                attachment,
                pname,
                ref chan,
            ) => Self::get_framebuffer_attachment_parameter(gl, target, attachment, pname, chan),
            WebGLCommand::GetShaderPrecisionFormat(shader_type, precision_type, ref chan) => {
                Self::shader_precision_format(gl, shader_type, precision_type, chan)
            },
            WebGLCommand::GetExtensions(ref chan) => Self::get_extensions(gl, chan),
            WebGLCommand::GetFragDataLocation(program_id, ref name, ref sender) => {
                let location = unsafe {
                    gl.get_frag_data_location(program_id.glow(), &to_name_in_compiled_shader(name))
                };
                sender.send_or_ignore(location);
            },
            WebGLCommand::GetUniformLocation(program_id, ref name, ref chan) => {
                Self::uniform_location(gl, program_id, name, chan)
            },
            WebGLCommand::GetShaderInfoLog(shader_id, ref chan) => {
                Self::shader_info_log(gl, shader_id, chan)
            },
            WebGLCommand::GetProgramInfoLog(program_id, ref chan) => {
                Self::program_info_log(gl, program_id, chan)
            },
            WebGLCommand::CompileShader(shader_id, ref source) => {
                Self::compile_shader(gl, shader_id, source)
            },
            WebGLCommand::CreateBuffer(ref chan) => Self::create_buffer(gl, chan),
            WebGLCommand::CreateFramebuffer(ref chan) => Self::create_framebuffer(gl, chan),
            WebGLCommand::CreateRenderbuffer(ref chan) => Self::create_renderbuffer(gl, chan),
            WebGLCommand::CreateTexture(ref chan) => Self::create_texture(gl, chan),
            WebGLCommand::CreateProgram(ref chan) => Self::create_program(gl, chan),
            WebGLCommand::CreateShader(shader_type, ref chan) => {
                Self::create_shader(gl, shader_type, chan)
            },
            WebGLCommand::DeleteBuffer(id) => unsafe { gl.delete_buffer(id.glow()) },
            WebGLCommand::DeleteFramebuffer(id) => unsafe { gl.delete_framebuffer(id.glow()) },
            WebGLCommand::DeleteRenderbuffer(id) => unsafe { gl.delete_renderbuffer(id.glow()) },
            WebGLCommand::DeleteTexture(id) => unsafe { gl.delete_texture(id.glow()) },
            WebGLCommand::DeleteProgram(id) => unsafe { gl.delete_program(id.glow()) },
            WebGLCommand::DeleteShader(id) => unsafe { gl.delete_shader(id.glow()) },
            WebGLCommand::BindBuffer(target, id) => unsafe {
                gl.bind_buffer(target, id.map(WebGLBufferId::glow))
            },
            WebGLCommand::BindFramebuffer(target, request) => {
                Self::bind_framebuffer(gl, target, request, ctx, device, state)
            },
            WebGLCommand::BindRenderbuffer(target, id) => unsafe {
                gl.bind_renderbuffer(target, id.map(WebGLRenderbufferId::glow))
            },
            WebGLCommand::BindTexture(target, id) => unsafe {
                gl.bind_texture(target, id.map(WebGLTextureId::glow))
            },
            WebGLCommand::BlitFrameBuffer(
                src_x0,
                src_y0,
                src_x1,
                src_y1,
                dst_x0,
                dst_y0,
                dst_x1,
                dst_y1,
                mask,
                filter,
            ) => unsafe {
                gl.blit_framebuffer(
                    src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask, filter,
                );
            },
            WebGLCommand::Uniform1f(uniform_id, v) => unsafe {
                gl.uniform_1_f32(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform1fv(uniform_id, ref v) => unsafe {
                gl.uniform_1_f32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform1i(uniform_id, v) => unsafe {
                gl.uniform_1_i32(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform1iv(uniform_id, ref v) => unsafe {
                gl.uniform_1_i32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform1ui(uniform_id, v) => unsafe {
                gl.uniform_1_u32(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform1uiv(uniform_id, ref v) => unsafe {
                gl.uniform_1_u32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform2f(uniform_id, x, y) => unsafe {
                gl.uniform_2_f32(native_uniform_location(uniform_id).as_ref(), x, y)
            },
            WebGLCommand::Uniform2fv(uniform_id, ref v) => unsafe {
                gl.uniform_2_f32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform2i(uniform_id, x, y) => unsafe {
                gl.uniform_2_i32(native_uniform_location(uniform_id).as_ref(), x, y)
            },
            WebGLCommand::Uniform2iv(uniform_id, ref v) => unsafe {
                gl.uniform_2_i32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform2ui(uniform_id, x, y) => unsafe {
                gl.uniform_2_u32(native_uniform_location(uniform_id).as_ref(), x, y)
            },
            WebGLCommand::Uniform2uiv(uniform_id, ref v) => unsafe {
                gl.uniform_2_u32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform3f(uniform_id, x, y, z) => unsafe {
                gl.uniform_3_f32(native_uniform_location(uniform_id).as_ref(), x, y, z)
            },
            WebGLCommand::Uniform3fv(uniform_id, ref v) => unsafe {
                gl.uniform_3_f32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform3i(uniform_id, x, y, z) => unsafe {
                gl.uniform_3_i32(native_uniform_location(uniform_id).as_ref(), x, y, z)
            },
            WebGLCommand::Uniform3iv(uniform_id, ref v) => unsafe {
                gl.uniform_3_i32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform3ui(uniform_id, x, y, z) => unsafe {
                gl.uniform_3_u32(native_uniform_location(uniform_id).as_ref(), x, y, z)
            },
            WebGLCommand::Uniform3uiv(uniform_id, ref v) => unsafe {
                gl.uniform_3_u32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform4f(uniform_id, x, y, z, w) => unsafe {
                gl.uniform_4_f32(native_uniform_location(uniform_id).as_ref(), x, y, z, w)
            },
            WebGLCommand::Uniform4fv(uniform_id, ref v) => unsafe {
                gl.uniform_4_f32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform4i(uniform_id, x, y, z, w) => unsafe {
                gl.uniform_4_i32(native_uniform_location(uniform_id).as_ref(), x, y, z, w)
            },
            WebGLCommand::Uniform4iv(uniform_id, ref v) => unsafe {
                gl.uniform_4_i32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::Uniform4ui(uniform_id, x, y, z, w) => unsafe {
                gl.uniform_4_u32(native_uniform_location(uniform_id).as_ref(), x, y, z, w)
            },
            WebGLCommand::Uniform4uiv(uniform_id, ref v) => unsafe {
                gl.uniform_4_u32_slice(native_uniform_location(uniform_id).as_ref(), v)
            },
            WebGLCommand::UniformMatrix2fv(uniform_id, ref v) => unsafe {
                gl.uniform_matrix_2_f32_slice(
                    native_uniform_location(uniform_id).as_ref(),
                    false,
                    v,
                )
            },
            WebGLCommand::UniformMatrix3fv(uniform_id, ref v) => unsafe {
                gl.uniform_matrix_3_f32_slice(
                    native_uniform_location(uniform_id).as_ref(),
                    false,
                    v,
                )
            },
            WebGLCommand::UniformMatrix4fv(uniform_id, ref v) => unsafe {
                gl.uniform_matrix_4_f32_slice(
                    native_uniform_location(uniform_id).as_ref(),
                    false,
                    v,
                )
            },
            WebGLCommand::UniformMatrix3x2fv(uniform_id, ref v) => unsafe {
                gl.uniform_matrix_3x2_f32_slice(
                    native_uniform_location(uniform_id).as_ref(),
                    false,
                    v,
                )
            },
            WebGLCommand::UniformMatrix4x2fv(uniform_id, ref v) => unsafe {
                gl.uniform_matrix_4x2_f32_slice(
                    native_uniform_location(uniform_id).as_ref(),
                    false,
                    v,
                )
            },
            WebGLCommand::UniformMatrix2x3fv(uniform_id, ref v) => unsafe {
                gl.uniform_matrix_2x3_f32_slice(
                    native_uniform_location(uniform_id).as_ref(),
                    false,
                    v,
                )
            },
            WebGLCommand::UniformMatrix4x3fv(uniform_id, ref v) => unsafe {
                gl.uniform_matrix_4x3_f32_slice(
                    native_uniform_location(uniform_id).as_ref(),
                    false,
                    v,
                )
            },
            WebGLCommand::UniformMatrix2x4fv(uniform_id, ref v) => unsafe {
                gl.uniform_matrix_2x4_f32_slice(
                    native_uniform_location(uniform_id).as_ref(),
                    false,
                    v,
                )
            },
            WebGLCommand::UniformMatrix3x4fv(uniform_id, ref v) => unsafe {
                gl.uniform_matrix_3x4_f32_slice(
                    native_uniform_location(uniform_id).as_ref(),
                    false,
                    v,
                )
            },
            WebGLCommand::ValidateProgram(program_id) => unsafe {
                gl.validate_program(program_id.glow())
            },
            WebGLCommand::VertexAttrib(attrib_id, x, y, z, w) => unsafe {
                gl.vertex_attrib_4_f32(attrib_id, x, y, z, w)
            },
            WebGLCommand::VertexAttribI(attrib_id, x, y, z, w) => unsafe {
                gl.vertex_attrib_4_i32(attrib_id, x, y, z, w)
            },
            WebGLCommand::VertexAttribU(attrib_id, x, y, z, w) => unsafe {
                gl.vertex_attrib_4_u32(attrib_id, x, y, z, w)
            },
            WebGLCommand::VertexAttribPointer2f(attrib_id, size, normalized, stride, offset) => unsafe {
                gl.vertex_attrib_pointer_f32(
                    attrib_id,
                    size,
                    gl::FLOAT,
                    normalized,
                    stride,
                    offset as _,
                )
            },
            WebGLCommand::VertexAttribPointer(
                attrib_id,
                size,
                data_type,
                normalized,
                stride,
                offset,
            ) => unsafe {
                gl.vertex_attrib_pointer_f32(
                    attrib_id,
                    size,
                    data_type,
                    normalized,
                    stride,
                    offset as _,
                )
            },
            WebGLCommand::VertexAttribIPointer(attrib_id, size, data_type, stride, offset) => unsafe {
                gl.vertex_attrib_pointer_i32(attrib_id, size, data_type, stride, offset as _)
            },
            WebGLCommand::SetViewport(x, y, width, height) => unsafe {
                gl.viewport(x, y, width, height)
            },
            WebGLCommand::TexImage3D {
                target,
                level,
                internal_format,
                size,
                depth,
                format,
                data_type,
                effective_data_type,
                unpacking_alignment,
                alpha_treatment,
                y_axis_treatment,
                pixel_format,
                ref data,
            } => {
                let pixels = prepare_pixels(
                    internal_format,
                    data_type,
                    size,
                    unpacking_alignment,
                    alpha_treatment,
                    y_axis_treatment,
                    pixel_format,
                    Cow::Borrowed(data),
                );

                unsafe {
                    gl.pixel_store_i32(gl::UNPACK_ALIGNMENT, unpacking_alignment as i32);
                    gl.tex_image_3d(
                        target,
                        level as i32,
                        internal_format.as_gl_constant() as i32,
                        size.width as i32,
                        size.height as i32,
                        depth as i32,
                        0,
                        format.as_gl_constant(),
                        effective_data_type,
                        PixelUnpackData::Slice(Some(&pixels)),
                    );
                }
            },
            WebGLCommand::TexImage2D {
                target,
                level,
                internal_format,
                size,
                format,
                data_type,
                effective_data_type,
                unpacking_alignment,
                alpha_treatment,
                y_axis_treatment,
                pixel_format,
                ref data,
            } => {
                let pixels = prepare_pixels(
                    internal_format,
                    data_type,
                    size,
                    unpacking_alignment,
                    alpha_treatment,
                    y_axis_treatment,
                    pixel_format,
                    Cow::Borrowed(data),
                );

                unsafe {
                    gl.pixel_store_i32(gl::UNPACK_ALIGNMENT, unpacking_alignment as i32);
                    gl.tex_image_2d(
                        target,
                        level as i32,
                        internal_format.as_gl_constant() as i32,
                        size.width as i32,
                        size.height as i32,
                        0,
                        format.as_gl_constant(),
                        effective_data_type,
                        PixelUnpackData::Slice(Some(&pixels)),
                    );
                }
            },
            WebGLCommand::TexImage2DPBO {
                target,
                level,
                internal_format,
                size,
                format,
                effective_data_type,
                unpacking_alignment,
                offset,
            } => unsafe {
                gl.pixel_store_i32(gl::UNPACK_ALIGNMENT, unpacking_alignment as i32);

                gl.tex_image_2d(
                    target,
                    level as i32,
                    internal_format.as_gl_constant() as i32,
                    size.width as i32,
                    size.height as i32,
                    0,
                    format.as_gl_constant(),
                    effective_data_type,
                    PixelUnpackData::BufferOffset(offset as u32),
                );
            },
            WebGLCommand::TexSubImage2D {
                target,
                level,
                xoffset,
                yoffset,
                size,
                format,
                data_type,
                effective_data_type,
                unpacking_alignment,
                alpha_treatment,
                y_axis_treatment,
                pixel_format,
                ref data,
            } => {
                let pixels = prepare_pixels(
                    format,
                    data_type,
                    size,
                    unpacking_alignment,
                    alpha_treatment,
                    y_axis_treatment,
                    pixel_format,
                    Cow::Borrowed(data),
                );

                unsafe {
                    gl.pixel_store_i32(gl::UNPACK_ALIGNMENT, unpacking_alignment as i32);
                    gl.tex_sub_image_2d(
                        target,
                        level as i32,
                        xoffset,
                        yoffset,
                        size.width as i32,
                        size.height as i32,
                        format.as_gl_constant(),
                        effective_data_type,
                        glow::PixelUnpackData::Slice(Some(&pixels)),
                    );
                }
            },
            WebGLCommand::CompressedTexImage2D {
                target,
                level,
                internal_format,
                size,
                ref data,
            } => unsafe {
                gl.compressed_tex_image_2d(
                    target,
                    level as i32,
                    internal_format as i32,
                    size.width as i32,
                    size.height as i32,
                    0,
                    data.len() as i32,
                    data,
                )
            },
            WebGLCommand::CompressedTexSubImage2D {
                target,
                level,
                xoffset,
                yoffset,
                size,
                format,
                ref data,
            } => {
                unsafe {
                    gl.compressed_tex_sub_image_2d(
                        target,
                        level,
                        xoffset,
                        yoffset,
                        size.width as i32,
                        size.height as i32,
                        format,
                        glow::CompressedPixelUnpackData::Slice(data),
                    )
                };
            },
            WebGLCommand::TexStorage2D(target, levels, internal_format, width, height) => unsafe {
                gl.tex_storage_2d(
                    target,
                    levels as i32,
                    internal_format.as_gl_constant(),
                    width as i32,
                    height as i32,
                )
            },
            WebGLCommand::TexStorage3D(target, levels, internal_format, width, height, depth) => unsafe {
                gl.tex_storage_3d(
                    target,
                    levels as i32,
                    internal_format.as_gl_constant(),
                    width as i32,
                    height as i32,
                    depth as i32,
                )
            },
            WebGLCommand::DrawingBufferWidth(ref sender) => {
                let size = device
                    .context_surface_info(ctx)
                    .unwrap()
                    .expect("Where's the front buffer?")
                    .size;
                sender.send_or_ignore(size.width)
            },
            WebGLCommand::DrawingBufferHeight(ref sender) => {
                let size = device
                    .context_surface_info(ctx)
                    .unwrap()
                    .expect("Where's the front buffer?")
                    .size;
                sender.send_or_ignore(size.height)
            },
            WebGLCommand::Finish(ref sender) => Self::finish(gl, sender),
            WebGLCommand::Flush => unsafe { gl.flush() },
            WebGLCommand::GenerateMipmap(target) => unsafe { gl.generate_mipmap(target) },
            WebGLCommand::CreateVertexArray(ref chan) => {
                let id = Self::create_vertex_array(gl);
                let _ = chan.send(id);
            },
            WebGLCommand::DeleteVertexArray(id) => {
                unsafe { gl.delete_vertex_array(id.glow()) };
            },
            WebGLCommand::BindVertexArray(id) => {
                let id = id.map(WebGLVertexArrayId::glow).or(state.default_vao);
                unsafe { gl.bind_vertex_array(id) }
            },
            WebGLCommand::GetParameterBool(param, ref sender) => {
                let value = match param {
                    webgl::ParameterBool::DepthWritemask => state.depth_write_mask,
                    _ => unsafe { gl.get_parameter_bool(param as u32) },
                };
                sender.send_or_ignore(value)
            },
            WebGLCommand::FenceSync(ref sender) => {
                let value = unsafe { gl.fence_sync(gl::SYNC_GPU_COMMANDS_COMPLETE, 0).unwrap() };
                sender.send_or_ignore(WebGLSyncId::from_glow(value));
            },
            WebGLCommand::IsSync(sync_id, ref sender) => {
                let value = unsafe { gl.is_sync(sync_id.glow()) };
                sender.send_or_ignore(value);
            },
            WebGLCommand::ClientWaitSync(sync_id, flags, timeout, ref sender) => {
                let value = unsafe { gl.client_wait_sync(sync_id.glow(), flags, timeout as _) };
                sender.send_or_ignore(value);
            },
            WebGLCommand::WaitSync(sync_id, flags, timeout) => {
                unsafe { gl.wait_sync(sync_id.glow(), flags, timeout as u64) };
            },
            WebGLCommand::GetSyncParameter(sync_id, param, ref sender) => {
                let value = unsafe { gl.get_sync_parameter_i32(sync_id.glow(), param) };
                sender.send_or_ignore(value as u32);
            },
            WebGLCommand::DeleteSync(sync_id) => {
                unsafe { gl.delete_sync(sync_id.glow()) };
            },
            WebGLCommand::GetParameterBool4(param, ref sender) => {
                let value = match param {
                    webgl::ParameterBool4::ColorWritemask => state.color_write_mask,
                };
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetParameterInt(param, ref sender) => {
                // The core profiles used for WebGL 2 dropped the `*_BITS` queries.
                let webgl2 = state.webgl_version == WebGLVersion::WebGL2;
                let value = match param {
                    webgl::ParameterInt::AlphaBits if state.fake_no_alpha() => 0,
                    webgl::ParameterInt::DepthBits if state.fake_no_depth() => 0,
                    webgl::ParameterInt::StencilBits if state.fake_no_stencil() => 0,
                    webgl::ParameterInt::RedBits if webgl2 => Self::read_draw_framebuffer_bits(
                        gl,
                        gl::COLOR_ATTACHMENT0,
                        gl::FRAMEBUFFER_ATTACHMENT_RED_SIZE,
                    ),
                    webgl::ParameterInt::GreenBits if webgl2 => Self::read_draw_framebuffer_bits(
                        gl,
                        gl::COLOR_ATTACHMENT0,
                        gl::FRAMEBUFFER_ATTACHMENT_GREEN_SIZE,
                    ),
                    webgl::ParameterInt::BlueBits if webgl2 => Self::read_draw_framebuffer_bits(
                        gl,
                        gl::COLOR_ATTACHMENT0,
                        gl::FRAMEBUFFER_ATTACHMENT_BLUE_SIZE,
                    ),
                    webgl::ParameterInt::AlphaBits if webgl2 => Self::read_draw_framebuffer_bits(
                        gl,
                        gl::COLOR_ATTACHMENT0,
                        gl::FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE,
                    ),
                    webgl::ParameterInt::DepthBits if webgl2 => Self::read_draw_framebuffer_bits(
                        gl,
                        gl::DEPTH_ATTACHMENT,
                        gl::FRAMEBUFFER_ATTACHMENT_DEPTH_SIZE,
                    ),
                    webgl::ParameterInt::StencilBits if webgl2 => Self::read_draw_framebuffer_bits(
                        gl,
                        gl::STENCIL_ATTACHMENT,
                        gl::FRAMEBUFFER_ATTACHMENT_STENCIL_SIZE,
                    ),
                    _ => unsafe { gl.get_parameter_i32(param as u32) },
                };
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetParameterUInt(param, ref sender) => {
                let value = match param {
                    webgl::ParameterUInt::StencilWritemask => state.stencil_write_mask.0,
                    webgl::ParameterUInt::StencilBackWritemask => state.stencil_write_mask.1,
                    webgl::ParameterUInt::StencilValueMask => state.stencil_value_mask.0,
                    webgl::ParameterUInt::StencilBackValueMask => state.stencil_value_mask.1,
                };
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetParameterInt2(param, ref sender) => {
                let mut value = [0; 2];
                unsafe {
                    gl.get_parameter_i32_slice(param as u32, &mut value);
                }
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetParameterInt4(param, ref sender) => {
                let mut value = [0; 4];
                unsafe {
                    gl.get_parameter_i32_slice(param as u32, &mut value);
                }
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetParameterFloat(param, ref sender) => {
                let mut value = [0.];
                unsafe {
                    gl.get_parameter_f32_slice(param as u32, &mut value);
                }
                sender.send_or_ignore(value[0])
            },
            WebGLCommand::GetParameterFloat2(param, ref sender) => {
                let mut value = [0.; 2];
                unsafe {
                    gl.get_parameter_f32_slice(param as u32, &mut value);
                }
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetParameterFloat4(param, ref sender) => {
                let mut value = [0.; 4];
                unsafe {
                    gl.get_parameter_f32_slice(param as u32, &mut value);
                }
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetProgramValidateStatus(program, ref sender) => sender
                .send_or_ignore(unsafe { gl.get_program_validate_status(program.glow()) }),
            WebGLCommand::GetProgramActiveUniforms(program, ref sender) => sender
                .send_or_ignore(unsafe { gl.get_program_parameter_i32(program.glow(), gl::ACTIVE_UNIFORMS) }),
            WebGLCommand::GetCurrentVertexAttrib(index, ref sender) => {
                let mut value = [0.; 4];
                unsafe {
                    gl.get_vertex_attrib_parameter_f32_slice(
                        index,
                        gl::CURRENT_VERTEX_ATTRIB,
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetTexParameterFloat(target, param, ref sender) => {
                sender
                    .send_or_ignore(unsafe { gl.get_tex_parameter_f32(target, param as u32) });
            },
            WebGLCommand::GetTexParameterInt(target, param, ref sender) => {
                sender
                    .send_or_ignore(unsafe { gl.get_tex_parameter_i32(target, param as u32) });
            },
            WebGLCommand::GetTexParameterBool(target, param, ref sender) => {
                sender
                    .send_or_ignore(unsafe { gl.get_tex_parameter_i32(target, param as u32) } != 0);
            },
            WebGLCommand::GetInternalFormatIntVec(target, internal_format, param, ref sender) => {
                match param {
                    InternalFormatIntVec::Samples => {
                        let mut count = [0; 1];
                        unsafe {
                            gl.get_internal_format_i32_slice(
                                target,
                                internal_format,
                                gl::NUM_SAMPLE_COUNTS,
                                &mut count,
                            )
                        };
                        assert!(count[0] >= 0);

                        let mut values = vec![0; count[0] as usize];
                        unsafe {
                            gl.get_internal_format_i32_slice(
                                target,
                                internal_format,
                                param as u32,
                                &mut values,
                            )
                        };
                        sender.send_or_ignore(values)
                    },
                }
            },
            WebGLCommand::TexParameteri(target, param, value) => unsafe {
                gl.tex_parameter_i32(target, param, value)
            },
            WebGLCommand::TexParameterf(target, param, value) => unsafe {
                gl.tex_parameter_f32(target, param, value)
            },
            WebGLCommand::LinkProgram(program_id, ref sender) => {
                sender.send_or_ignore(Self::link_program(gl, program_id))
            },
            WebGLCommand::UseProgram(program_id) => unsafe {
                gl.use_program(program_id.map(|p| p.glow()))
            },
            WebGLCommand::DrawArrays { mode, first, count } => unsafe {
                gl.draw_arrays(mode, first, count)
            },
            WebGLCommand::DrawArraysInstanced {
                mode,
                first,
                count,
                primcount,
            } => unsafe { gl.draw_arrays_instanced(mode, first, count, primcount) },
            WebGLCommand::DrawElements {
                mode,
                count,
                type_,
                offset,
            } => unsafe { gl.draw_elements(mode, count, type_, offset as _) },
            WebGLCommand::DrawElementsInstanced {
                mode,
                count,
                type_,
                offset,
                primcount,
            } => unsafe {
                gl.draw_elements_instanced(mode, count, type_, offset as i32, primcount)
            },
            WebGLCommand::VertexAttribDivisor { index, divisor } => unsafe {
                gl.vertex_attrib_divisor(index, divisor)
            },
            WebGLCommand::GetUniformBool(program_id, loc, ref sender) => {
                let mut value = [0];
                unsafe {
                    gl.get_uniform_i32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value[0] != 0);
            },
            WebGLCommand::GetUniformBool2(program_id, loc, ref sender) => {
                let mut value = [0; 2];
                unsafe {
                    gl.get_uniform_i32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                let value = [value[0] != 0, value[1] != 0];
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformBool3(program_id, loc, ref sender) => {
                let mut value = [0; 3];
                unsafe {
                    gl.get_uniform_i32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                let value = [value[0] != 0, value[1] != 0, value[2] != 0];
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformBool4(program_id, loc, ref sender) => {
                let mut value = [0; 4];
                unsafe {
                    gl.get_uniform_i32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                let value = [value[0] != 0, value[1] != 0, value[2] != 0, value[3] != 0];
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformInt(program_id, loc, ref sender) => {
                let mut value = [0];
                unsafe {
                    gl.get_uniform_i32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value[0]);
            },
            WebGLCommand::GetUniformInt2(program_id, loc, ref sender) => {
                let mut value = [0; 2];
                unsafe {
                    gl.get_uniform_i32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformInt3(program_id, loc, ref sender) => {
                let mut value = [0; 3];
                unsafe {
                    gl.get_uniform_i32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformInt4(program_id, loc, ref sender) => {
                let mut value = [0; 4];
                unsafe {
                    gl.get_uniform_i32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformUint(program_id, loc, ref sender) => {
                let mut value = [0];
                unsafe {
                    gl.get_uniform_u32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value[0]);
            },
            WebGLCommand::GetUniformUint2(program_id, loc, ref sender) => {
                let mut value = [0; 2];
                unsafe {
                    gl.get_uniform_u32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformUint3(program_id, loc, ref sender) => {
                let mut value = [0; 3];
                unsafe {
                    gl.get_uniform_u32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformUint4(program_id, loc, ref sender) => {
                let mut value = [0; 4];
                unsafe {
                    gl.get_uniform_u32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformFloat(program_id, loc, ref sender) => {
                let mut value = [0.];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value[0]);
            },
            WebGLCommand::GetUniformFloat2(program_id, loc, ref sender) => {
                let mut value = [0.; 2];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformFloat3(program_id, loc, ref sender) => {
                let mut value = [0.; 3];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformFloat4(program_id, loc, ref sender) => {
                let mut value = [0.; 4];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformFloat9(program_id, loc, ref sender) => {
                let mut value = [0.; 9];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformFloat16(program_id, loc, ref sender) => {
                let mut value = [0.; 16];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetUniformFloat2x3(program_id, loc, ref sender) => {
                let mut value = [0.; 2 * 3];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetUniformFloat2x4(program_id, loc, ref sender) => {
                let mut value = [0.; 2 * 4];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetUniformFloat3x2(program_id, loc, ref sender) => {
                let mut value = [0.; 3 * 2];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetUniformFloat3x4(program_id, loc, ref sender) => {
                let mut value = [0.; 3 * 4];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetUniformFloat4x2(program_id, loc, ref sender) => {
                let mut value = [0.; 4 * 2];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetUniformFloat4x3(program_id, loc, ref sender) => {
                let mut value = [0.; 4 * 3];
                unsafe {
                    gl.get_uniform_f32(
                        program_id.glow(),
                        &NativeUniformLocation(loc as u32),
                        &mut value,
                    );
                }
                sender.send_or_ignore(value)
            },
            WebGLCommand::GetUniformBlockIndex(program_id, ref name, ref sender) => {
                let name = to_name_in_compiled_shader(name);
                let index = unsafe { gl.get_uniform_block_index(program_id.glow(), &name) };
                // TODO(#34300): use Option<u32>
                sender.send_or_ignore(index.unwrap_or(gl::INVALID_INDEX));
            },
            WebGLCommand::GetUniformIndices(program_id, ref names, ref sender) => {
                let names = names
                    .iter()
                    .map(|name| to_name_in_compiled_shader(name))
                    .collect::<Vec<_>>();
                let name_strs = names.iter().map(|name| name.as_str()).collect::<Vec<_>>();
                let indices = unsafe {
                    gl.get_uniform_indices(program_id.glow(), &name_strs)
                        .iter()
                        .map(|index| index.unwrap_or(gl::INVALID_INDEX))
                        .collect()
                };
                sender.send_or_ignore(indices);
            },
            WebGLCommand::GetActiveUniforms(program_id, ref indices, pname, ref sender) => {
                let results =
                    unsafe { gl.get_active_uniforms_parameter(program_id.glow(), indices, pname) };
                sender.send_or_ignore(results);
            },
            WebGLCommand::GetActiveUniformBlockName(program_id, block_idx, ref sender) => {
                let name =
                    unsafe { gl.get_active_uniform_block_name(program_id.glow(), block_idx) };
                sender.send_or_ignore(name);
            },
            WebGLCommand::GetActiveUniformBlockParameter(
                program_id,
                block_idx,
                pname,
                ref sender,
            ) => {
                let size = match pname {
                    gl::UNIFORM_BLOCK_ACTIVE_UNIFORM_INDICES => unsafe {
                        gl.get_active_uniform_block_parameter_i32(
                            program_id.glow(),
                            block_idx,
                            gl::UNIFORM_BLOCK_ACTIVE_UNIFORMS,
                        ) as usize
                    },
                    _ => 1,
                };
                let mut result = vec![0; size];
                unsafe {
                    gl.get_active_uniform_block_parameter_i32_slice(
                        program_id.glow(),
                        block_idx,
                        pname,
                        &mut result,
                    )
                };
                sender.send_or_ignore(result);
            },
            WebGLCommand::UniformBlockBinding(program_id, block_idx, block_binding) => unsafe {
                gl.uniform_block_binding(program_id.glow(), block_idx, block_binding)
            },
            WebGLCommand::InitializeFramebuffer {
                color,
                depth,
                stencil,
            } => Self::initialize_framebuffer(gl, state, color, depth, stencil),
            WebGLCommand::BeginQuery(target, query_id) => {
                unsafe { gl.begin_query(target, query_id.glow()) };
            },
            WebGLCommand::EndQuery(target) => {
                unsafe { gl.end_query(target) };
            },
            WebGLCommand::DeleteQuery(query_id) => {
                unsafe { gl.delete_query(query_id.glow()) };
            },
            WebGLCommand::GenerateQuery(ref sender) => {
                // TODO(#34300): use Option<WebGLQueryId>
                let id = unsafe { gl.create_query().unwrap() };
                sender.send_or_ignore(WebGLQueryId::from_glow(id))
            },
            WebGLCommand::GetQueryState(ref sender, query_id, pname) => {
                let value = unsafe { gl.get_query_parameter_u32(query_id.glow(), pname) };
                sender.send_or_ignore(value)
            },
            WebGLCommand::GenerateSampler(ref sender) => {
                let id = unsafe { gl.create_sampler().unwrap() };
                sender.send_or_ignore(WebGLSamplerId::from_glow(id))
            },
            WebGLCommand::DeleteSampler(sampler_id) => {
                unsafe { gl.delete_sampler(sampler_id.glow()) };
            },
            WebGLCommand::BindSampler(unit, sampler_id) => {
                unsafe { gl.bind_sampler(unit, Some(sampler_id.glow())) };
            },
            WebGLCommand::SetSamplerParameterInt(sampler_id, pname, value) => {
                unsafe { gl.sampler_parameter_i32(sampler_id.glow(), pname, value) };
            },
            WebGLCommand::SetSamplerParameterFloat(sampler_id, pname, value) => {
                unsafe { gl.sampler_parameter_f32(sampler_id.glow(), pname, value) };
            },
            WebGLCommand::GetSamplerParameterInt(sampler_id, pname, ref sender) => {
                let value = unsafe { gl.get_sampler_parameter_i32(sampler_id.glow(), pname) };
                sender.send_or_ignore(value);
            },
            WebGLCommand::GetSamplerParameterFloat(sampler_id, pname, ref sender) => {
                let value = unsafe { gl.get_sampler_parameter_f32(sampler_id.glow(), pname) };
                sender.send_or_ignore(value);
            },
            WebGLCommand::BindBufferBase(target, index, id) => {
                // https://searchfox.org/mozilla-central/rev/13b081a62d3f3e3e3120f95564529257b0bf451c/dom/canvas/WebGLContextBuffers.cpp#208-210
                // BindBufferBase/Range will fail (on some drivers) if the buffer name has
                // never been bound. (GenBuffers makes a name, but BindBuffer initializes
                // that name as a real buffer object)
                let id = id.map(WebGLBufferId::glow);
                unsafe {
                    gl.bind_buffer(target, id);
                    gl.bind_buffer(target, None);
                    gl.bind_buffer_base(target, index, id);
                }
            },
            WebGLCommand::BindBufferRange(target, index, id, offset, size) => {
                // https://searchfox.org/mozilla-central/rev/13b081a62d3f3e3e3120f95564529257b0bf451c/dom/canvas/WebGLContextBuffers.cpp#208-210
                // BindBufferBase/Range will fail (on some drivers) if the buffer name has
                // never been bound. (GenBuffers makes a name, but BindBuffer initializes
                // that name as a real buffer object)
                let id = id.map(WebGLBufferId::glow);
                unsafe {
                    gl.bind_buffer(target, id);
                    gl.bind_buffer(target, None);
                    gl.bind_buffer_range(target, index, id, offset as i32, size as i32);
                }
            },
            WebGLCommand::ClearBufferfv(buffer, draw_buffer, ref value) => unsafe {
                gl.clear_buffer_f32_slice(buffer, draw_buffer as u32, value)
            },
            WebGLCommand::ClearBufferiv(buffer, draw_buffer, ref value) => unsafe {
                gl.clear_buffer_i32_slice(buffer, draw_buffer as u32, value)
            },
            WebGLCommand::ClearBufferuiv(buffer, draw_buffer, ref value) => unsafe {
                gl.clear_buffer_u32_slice(buffer, draw_buffer as u32, value)
            },
            WebGLCommand::ClearBufferfi(buffer, draw_buffer, depth, stencil) => unsafe {
                gl.clear_buffer_depth_stencil(buffer, draw_buffer as u32, depth, stencil)
            },
            WebGLCommand::InvalidateFramebuffer(target, ref attachments) => unsafe {
                gl.invalidate_framebuffer(target, attachments)
            },
            WebGLCommand::InvalidateSubFramebuffer(target, ref attachments, x, y, w, h) => unsafe {
                gl.invalidate_sub_framebuffer(target, attachments, x, y, w, h)
            },
            WebGLCommand::FramebufferTextureLayer(target, attachment, tex_id, level, layer) => {
                let tex_id = tex_id.map(WebGLTextureId::glow);
                let attach = |attachment| unsafe {
                    gl.framebuffer_texture_layer(target, attachment, tex_id, level, layer)
                };

                if attachment == gl::DEPTH_STENCIL_ATTACHMENT {
                    attach(gl::DEPTH_ATTACHMENT);
                    attach(gl::STENCIL_ATTACHMENT);
                } else {
                    attach(attachment)
                }
            },
            WebGLCommand::ReadBuffer(buffer) => unsafe {
                gl.read_buffer(Self::default_framebuffer_color_buffer(buffer, ctx, device))
            },
            WebGLCommand::DrawBuffers(ref buffers) => {
                let buffers: Vec<_> = buffers
                    .iter()
                    .map(|&buffer| Self::default_framebuffer_color_buffer(buffer, ctx, device))
                    .collect();
                unsafe { gl.draw_buffers(&buffers) }
            },
        }

        // If debug asertions are enabled, then check the error state.
        #[cfg(debug_assertions)]
        {
            let error = unsafe { gl.get_error() };
            if error != gl::NO_ERROR {
                error!("Last GL operation failed: {:?}", command);
                if error == gl::INVALID_FRAMEBUFFER_OPERATION {
                    let framebuffer_bindings =
                        unsafe { gl.get_parameter_framebuffer(gl::DRAW_FRAMEBUFFER_BINDING) };
                    debug!(
                        "(thread {:?}) Current draw framebuffer binding: {:?}",
                        ::std::thread::current().id(),
                        framebuffer_bindings
                    );
                }
                #[cfg(feature = "webgl_backtrace")]
                {
                    error!("Backtrace from failed WebGL API:\n{}", _backtrace.backtrace);
                    if let Some(backtrace) = _backtrace.js_backtrace {
                        error!("JS backtrace from failed WebGL API:\n{}", backtrace);
                    }
                }
                // TODO(servo#30568) revert to panic!() once underlying bug is fixed
                log::warn!(
                    "debug assertion failed! Unexpected WebGL error: 0x{:x} ({}) [{:?}]",
                    error,
                    error,
                    command
                );
            }
        }
        true
    }

    fn initialize_framebuffer(gl: &Gl, state: &GLState, color: bool, depth: bool, stencil: bool) {
        let bits = [
            (color, gl::COLOR_BUFFER_BIT),
            (depth, gl::DEPTH_BUFFER_BIT),
            (stencil, gl::STENCIL_BUFFER_BIT),
        ]
        .iter()
        .fold(0, |bits, &(enabled, bit)| {
            bits | if enabled { bit } else { 0 }
        });

        unsafe {
            gl.disable(gl::SCISSOR_TEST);
            gl.color_mask(true, true, true, true);
            gl.clear_color(0., 0., 0., 0.);
            gl.depth_mask(true);
            gl.clear_depth(1.);
            gl.stencil_mask_separate(gl::FRONT, 0xFFFFFFFF);
            gl.stencil_mask_separate(gl::BACK, 0xFFFFFFFF);
            gl.clear_stencil(0);
            gl.clear(bits);
        }

        state.restore_invariant(gl);
    }

    fn link_program(gl: &Gl, program: WebGLProgramId) -> ProgramLinkInfo {
        unsafe { gl.link_program(program.glow()) };
        let linked = unsafe { gl.get_program_link_status(program.glow()) };
        if !linked {
            return ProgramLinkInfo {
                linked: false,
                active_attribs: vec![].into(),
                active_uniforms: vec![].into(),
                active_uniform_blocks: vec![].into(),
                transform_feedback_length: Default::default(),
                transform_feedback_mode: Default::default(),
            };
        }
        let num_active_attribs =
            unsafe { gl.get_program_parameter_i32(program.glow(), gl::ACTIVE_ATTRIBUTES) };
        let active_attribs = (0..num_active_attribs as u32)
            .map(|i| {
                let active_attribute =
                    unsafe { gl.get_active_attribute(program.glow(), i) }.unwrap();
                let name = &active_attribute.name;
                let location = if name.starts_with("gl_") {
                    None
                } else {
                    unsafe { gl.get_attrib_location(program.glow(), name) }
                };
                ActiveAttribInfo {
                    name: from_name_in_compiled_shader(name),
                    size: active_attribute.size,
                    type_: active_attribute.atype,
                    location,
                }
            })
            .collect::<Vec<_>>()
            .into();

        let num_active_uniforms =
            unsafe { gl.get_program_parameter_i32(program.glow(), gl::ACTIVE_UNIFORMS) };
        let active_uniforms = (0..num_active_uniforms as u32)
            .map(|i| {
                let active_uniform = unsafe { gl.get_active_uniform(program.glow(), i) }.unwrap();
                let is_array = active_uniform.name.ends_with("[0]");
                let active_uniform_name = active_uniform
                    .name
                    .strip_suffix("[0]")
                    .unwrap_or_else(|| &active_uniform.name);
                ActiveUniformInfo {
                    base_name: from_name_in_compiled_shader(active_uniform_name).into(),
                    size: if is_array {
                        Some(active_uniform.size)
                    } else {
                        None
                    },
                    type_: active_uniform.utype,
                    bind_index: None,
                }
            })
            .collect::<Vec<_>>()
            .into();

        let num_active_uniform_blocks =
            unsafe { gl.get_program_parameter_i32(program.glow(), gl::ACTIVE_UNIFORM_BLOCKS) };
        let active_uniform_blocks = (0..num_active_uniform_blocks as u32)
            .map(|i| {
                let name = unsafe { gl.get_active_uniform_block_name(program.glow(), i) };
                let size = unsafe {
                    gl.get_active_uniform_block_parameter_i32(
                        program.glow(),
                        i,
                        gl::UNIFORM_BLOCK_DATA_SIZE,
                    )
                };
                ActiveUniformBlockInfo { name, size }
            })
            .collect::<Vec<_>>()
            .into();

        let transform_feedback_length = unsafe {
            gl.get_program_parameter_i32(program.glow(), gl::TRANSFORM_FEEDBACK_VARYINGS)
        };
        let transform_feedback_mode = unsafe {
            gl.get_program_parameter_i32(program.glow(), gl::TRANSFORM_FEEDBACK_BUFFER_MODE)
        };

        ProgramLinkInfo {
            linked: true,
            active_attribs,
            active_uniforms,
            active_uniform_blocks,
            transform_feedback_length,
            transform_feedback_mode,
        }
    }

    fn finish(gl: &Gl, chan: &GenericSender<()>) {
        unsafe { gl.finish() };
        chan.send_or_ignore(());
    }

    fn shader_precision_format(
        gl: &Gl,
        shader_type: u32,
        precision_type: u32,
        chan: &GenericSender<(i32, i32, i32)>,
    ) {
        let ShaderPrecisionFormat {
            range_min,
            range_max,
            precision,
        } = unsafe {
            gl.get_shader_precision_format(shader_type, precision_type)
                .unwrap_or_else(|| {
                    ShaderPrecisionFormat::common_desktop_hardware(
                        precision_type,
                        gl.version().is_embedded,
                    )
                })
        };
        chan.send_or_ignore((range_min, range_max, precision));
    }

    /// This is an implementation of `getSupportedExtensions()` from
    /// <https://registry.khronos.org/webgl/specs/latest/1.0/#5.14>
    fn get_extensions(gl: &Gl, result_sender: &GenericSender<String>) {
        let _ = result_sender.send(gl.supported_extensions().iter().join(" "));
    }

    /// <https://www.khronos.org/registry/webgl/specs/latest/1.0/#5.14.6>
    fn get_framebuffer_attachment_parameter(
        gl: &Gl,
        target: u32,
        attachment: u32,
        pname: u32,
        chan: &GenericSender<i32>,
    ) {
        let parameter =
            unsafe { gl.get_framebuffer_attachment_parameter_i32(target, attachment, pname) };
        chan.send_or_ignore(parameter);
    }

    /// Reads one component size of a draw framebuffer attachment, or 0 when
    /// nothing is attached, as OpenGL ES 3.0.6 §4.4.5 defines the `*_BITS` values.
    fn read_draw_framebuffer_bits(gl: &Gl, attachment: u32, pname: u32) -> i32 {
        unsafe {
            let object_type = gl.get_framebuffer_attachment_parameter_i32(
                gl::DRAW_FRAMEBUFFER,
                attachment,
                gl::FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE,
            );
            if object_type as u32 == gl::NONE {
                return 0;
            }
            gl.get_framebuffer_attachment_parameter_i32(gl::DRAW_FRAMEBUFFER, attachment, pname)
        }
    }

    /// <https://www.khronos.org/registry/webgl/specs/latest/1.0/#5.14.7>
    fn get_renderbuffer_parameter(gl: &Gl, target: u32, pname: u32, chan: &GenericSender<i32>) {
        let parameter = unsafe { gl.get_renderbuffer_parameter_i32(target, pname) };
        chan.send_or_ignore(parameter);
    }

    fn uniform_location(
        gl: &Gl,
        program_id: WebGLProgramId,
        name: &str,
        chan: &GenericSender<Option<i32>>,
    ) {
        let location = unsafe {
            gl.get_uniform_location(program_id.glow(), &to_name_in_compiled_shader(name))
        };
        // (#34300): replace this with WebGLUniformId
        chan.send_or_ignore(location.map(|l| l.0 as i32));
    }

    fn shader_info_log(gl: &Gl, shader_id: WebGLShaderId, chan: &GenericSender<String>) {
        let log = unsafe { gl.get_shader_info_log(shader_id.glow()) };
        chan.send_or_ignore(log);
    }

    fn program_info_log(gl: &Gl, program_id: WebGLProgramId, chan: &GenericSender<String>) {
        let log = unsafe { gl.get_program_info_log(program_id.glow()) };
        chan.send_or_ignore(log);
    }

    fn create_buffer(gl: &Gl, chan: &GenericSender<Option<WebGLBufferId>>) {
        let buffer = unsafe { gl.create_buffer() }
            .ok()
            .map(WebGLBufferId::from_glow);
        chan.send_or_ignore(buffer);
    }

    fn create_framebuffer(gl: &Gl, chan: &GenericSender<Option<WebGLFramebufferId>>) {
        let framebuffer = unsafe { gl.create_framebuffer() }
            .ok()
            .map(WebGLFramebufferId::from_glow);
        chan.send_or_ignore(framebuffer);
    }

    fn create_renderbuffer(gl: &Gl, chan: &GenericSender<Option<WebGLRenderbufferId>>) {
        let renderbuffer = unsafe { gl.create_renderbuffer() }
            .ok()
            .map(WebGLRenderbufferId::from_glow);
        chan.send_or_ignore(renderbuffer);
    }

    fn create_texture(gl: &Gl, chan: &GenericSender<Option<WebGLTextureId>>) {
        let texture = unsafe { gl.create_texture() }
            .ok()
            .map(WebGLTextureId::from_glow);
        chan.send_or_ignore(texture);
    }

    fn create_program(gl: &Gl, chan: &GenericSender<Option<WebGLProgramId>>) {
        let program = unsafe { gl.create_program() }
            .ok()
            .map(WebGLProgramId::from_glow);
        chan.send_or_ignore(program);
    }

    fn create_shader(gl: &Gl, shader_type: u32, chan: &GenericSender<Option<WebGLShaderId>>) {
        let shader = unsafe { gl.create_shader(shader_type) }
            .ok()
            .map(WebGLShaderId::from_glow);
        chan.send_or_ignore(shader);
    }

    fn create_vertex_array(gl: &Gl) -> Option<WebGLVertexArrayId> {
        let vao = unsafe { gl.create_vertex_array() }
            .ok()
            .map(WebGLVertexArrayId::from_glow);
        if vao.is_none() {
            let code = unsafe { gl.get_error() };
            warn!("Failed to create vertex array with error code {:x}", code);
        }
        vao
    }

    #[inline]
    fn bind_framebuffer(
        gl: &Gl,
        target: u32,
        request: WebGLFramebufferBindingRequest,
        ctx: &Context,
        device: &Device,
        state: &mut GLState,
    ) {
        let id = match request {
            WebGLFramebufferBindingRequest::Explicit(id) => Some(id.glow()),
            WebGLFramebufferBindingRequest::Default => {
                device
                    .context_surface_info(ctx)
                    .unwrap()
                    .expect("No surface attached!")
                    .framebuffer_object
            },
        };

        debug!("WebGLImpl::bind_framebuffer: {:?}", id);
        unsafe { gl.bind_framebuffer(target, id) };

        if (target == gl::FRAMEBUFFER) || (target == gl::DRAW_FRAMEBUFFER) {
            state.drawing_to_default_framebuffer =
                request == WebGLFramebufferBindingRequest::Default;
            state.restore_invariant(gl);
        }
    }

    /// Maps a WebGL default framebuffer color buffer to its GL name: when the surface is backed
    /// by a framebuffer object, GL rejects `BACK` and uses `COLOR_ATTACHMENT0` for the color
    /// buffer.
    fn default_framebuffer_color_buffer(buffer: u32, ctx: &Context, device: &Device) -> u32 {
        if buffer != gl::BACK {
            return buffer;
        }
        let surface_info = device
            .context_surface_info(ctx)
            .expect("WebGL commands run with their context current")
            .expect("WebGL contexts are created with a surface attached");
        match surface_info.framebuffer_object {
            Some(_) => gl::COLOR_ATTACHMENT0,
            None => gl::BACK,
        }
    }

    #[inline]
    fn compile_shader(gl: &Gl, shader_id: WebGLShaderId, source: &str) {
        unsafe {
            gl.shader_source(shader_id.glow(), source);
            gl.compile_shader(shader_id.glow());
        }
    }
}

/// ANGLE adds a `_u` prefix to variable names:
///
/// <https://chromium.googlesource.com/angle/angle/+/855d964bd0d05f6b2cb303f625506cf53d37e94f>
///
/// To avoid hard-coding this we would need to use the `sh::GetAttributes` and `sh::GetUniforms`
/// API to look up the `x.name` and `x.mappedName` members.
const ANGLE_NAME_PREFIX: &str = "_u";

/// Adds `_u` prefix to variable names
fn to_name_in_compiled_shader(s: &str) -> String {
    map_dot_separated(s, |s, mapped| {
        mapped.push_str(ANGLE_NAME_PREFIX);
        mapped.push_str(s);
    })
}

/// Removes `_u` prefix from variable names
fn from_name_in_compiled_shader(s: &str) -> String {
    map_dot_separated(s, |s, mapped| {
        mapped.push_str(if let Some(stripped) = s.strip_prefix(ANGLE_NAME_PREFIX) {
            stripped
        } else {
            s
        })
    })
}

fn map_dot_separated<F: Fn(&str, &mut String)>(s: &str, f: F) -> String {
    let mut iter = s.split('.');
    let mut mapped = String::new();
    f(iter.next().unwrap(), &mut mapped);
    for s in iter {
        mapped.push('.');
        f(s, &mut mapped);
    }
    mapped
}

#[expect(clippy::too_many_arguments)]
fn prepare_pixels(
    internal_format: TexFormat,
    data_type: TexDataType,
    size: Size2D<u32>,
    unpacking_alignment: u32,
    alpha_treatment: Option<AlphaTreatment>,
    y_axis_treatment: YAxisTreatment,
    pixel_format: Option<PixelFormat>,
    mut pixels: Cow<[u8]>,
) -> Cow<[u8]> {
    match alpha_treatment {
        Some(AlphaTreatment::Premultiply) => {
            if let Some(pixel_format) = pixel_format {
                match pixel_format {
                    PixelFormat::BGRA8 | PixelFormat::RGBA8 => {},
                    _ => unimplemented!("unsupported pixel format ({:?})", pixel_format),
                }
                premultiply_inplace(TexFormat::RGBA, TexDataType::UnsignedByte, pixels.to_mut());
            } else {
                premultiply_inplace(internal_format, data_type, pixels.to_mut());
            }
        },
        Some(AlphaTreatment::Unmultiply) => {
            assert!(pixel_format.is_some());
            unmultiply_inplace::<false>(pixels.to_mut());
        },
        None => {},
    }

    if let Some(pixel_format) = pixel_format {
        pixels = image_to_tex_image_data(
            pixel_format,
            internal_format,
            data_type,
            pixels.into_owned(),
        )
        .into();
    }

    if y_axis_treatment == YAxisTreatment::Flipped {
        // FINISHME: Consider doing premultiply and flip in a single mutable Vec.
        pixels = flip_pixels_y(
            internal_format,
            data_type,
            size.width as usize,
            size.height as usize,
            unpacking_alignment as usize,
            pixels.into_owned(),
        )
        .into();
    }

    pixels
}

/// Translates an image in rgba8 (red in the first byte) format to
/// the format that was requested of TexImage.
fn image_to_tex_image_data(
    pixel_format: PixelFormat,
    format: TexFormat,
    data_type: TexDataType,
    mut pixels: Vec<u8>,
) -> Vec<u8> {
    // hint for vector allocation sizing.
    let pixel_count = pixels.len() / 4;

    match pixel_format {
        PixelFormat::BGRA8 => pixels::rgba8_byte_swap_colors_inplace(&mut pixels),
        PixelFormat::RGBA8 => {},
        _ => unimplemented!("unsupported pixel format ({:?})", pixel_format),
    }

    match (format, data_type) {
        (TexFormat::RGBA, TexDataType::UnsignedByte) |
        (TexFormat::RGBA8, TexDataType::UnsignedByte) => pixels,
        (TexFormat::RGB, TexDataType::UnsignedByte) |
        (TexFormat::RGB8, TexDataType::UnsignedByte) => {
            for i in 0..pixel_count {
                let rgb = {
                    let rgb = &pixels[i * 4..i * 4 + 3];
                    [rgb[0], rgb[1], rgb[2]]
                };
                pixels[i * 3..i * 3 + 3].copy_from_slice(&rgb);
            }
            pixels.truncate(pixel_count * 3);
            pixels
        },
        (TexFormat::Alpha, TexDataType::UnsignedByte) => {
            for i in 0..pixel_count {
                let p = pixels[i * 4 + 3];
                pixels[i] = p;
            }
            pixels.truncate(pixel_count);
            pixels
        },
        (TexFormat::Luminance, TexDataType::UnsignedByte) => {
            for i in 0..pixel_count {
                let p = pixels[i * 4];
                pixels[i] = p;
            }
            pixels.truncate(pixel_count);
            pixels
        },
        (TexFormat::LuminanceAlpha, TexDataType::UnsignedByte) => {
            for i in 0..pixel_count {
                let (lum, a) = {
                    let rgba = &pixels[i * 4..i * 4 + 4];
                    (rgba[0], rgba[3])
                };
                pixels[i * 2] = lum;
                pixels[i * 2 + 1] = a;
            }
            pixels.truncate(pixel_count * 2);
            pixels
        },
        (TexFormat::RGBA, TexDataType::UnsignedShort4444) => {
            for i in 0..pixel_count {
                let p = {
                    let rgba = &pixels[i * 4..i * 4 + 4];
                    ((rgba[0] as u16 & 0xf0) << 8) |
                        ((rgba[1] as u16 & 0xf0) << 4) |
                        (rgba[2] as u16 & 0xf0) |
                        ((rgba[3] as u16 & 0xf0) >> 4)
                };
                NativeEndian::write_u16(&mut pixels[i * 2..i * 2 + 2], p);
            }
            pixels.truncate(pixel_count * 2);
            pixels
        },
        (TexFormat::RGBA, TexDataType::UnsignedShort5551) => {
            for i in 0..pixel_count {
                let p = {
                    let rgba = &pixels[i * 4..i * 4 + 4];
                    ((rgba[0] as u16 & 0xf8) << 8) |
                        ((rgba[1] as u16 & 0xf8) << 3) |
                        ((rgba[2] as u16 & 0xf8) >> 2) |
                        ((rgba[3] as u16) >> 7)
                };
                NativeEndian::write_u16(&mut pixels[i * 2..i * 2 + 2], p);
            }
            pixels.truncate(pixel_count * 2);
            pixels
        },
        (TexFormat::RGB, TexDataType::UnsignedShort565) => {
            for i in 0..pixel_count {
                let p = {
                    let rgb = &pixels[i * 4..i * 4 + 3];
                    ((rgb[0] as u16 & 0xf8) << 8) |
                        ((rgb[1] as u16 & 0xfc) << 3) |
                        ((rgb[2] as u16 & 0xf8) >> 3)
                };
                NativeEndian::write_u16(&mut pixels[i * 2..i * 2 + 2], p);
            }
            pixels.truncate(pixel_count * 2);
            pixels
        },
        (TexFormat::RGBA, TexDataType::Float) | (TexFormat::RGBA32f, TexDataType::Float) => {
            let mut rgbaf32 = Vec::<u8>::with_capacity(pixel_count * 16);
            for rgba8 in pixels.chunks(4) {
                rgbaf32.write_f32::<NativeEndian>(rgba8[0] as f32).unwrap();
                rgbaf32.write_f32::<NativeEndian>(rgba8[1] as f32).unwrap();
                rgbaf32.write_f32::<NativeEndian>(rgba8[2] as f32).unwrap();
                rgbaf32.write_f32::<NativeEndian>(rgba8[3] as f32).unwrap();
            }
            rgbaf32
        },

        (TexFormat::RGB, TexDataType::Float) | (TexFormat::RGB32f, TexDataType::Float) => {
            let mut rgbf32 = Vec::<u8>::with_capacity(pixel_count * 12);
            for rgba8 in pixels.chunks(4) {
                rgbf32.write_f32::<NativeEndian>(rgba8[0] as f32).unwrap();
                rgbf32.write_f32::<NativeEndian>(rgba8[1] as f32).unwrap();
                rgbf32.write_f32::<NativeEndian>(rgba8[2] as f32).unwrap();
            }
            rgbf32
        },

        (TexFormat::Alpha, TexDataType::Float) | (TexFormat::Alpha32f, TexDataType::Float) => {
            for rgba8 in pixels.chunks_mut(4) {
                let p = rgba8[3] as f32;
                NativeEndian::write_f32(rgba8, p);
            }
            pixels
        },

        (TexFormat::Luminance, TexDataType::Float) |
        (TexFormat::Luminance32f, TexDataType::Float) => {
            for rgba8 in pixels.chunks_mut(4) {
                let p = rgba8[0] as f32;
                NativeEndian::write_f32(rgba8, p);
            }
            pixels
        },

        (TexFormat::LuminanceAlpha, TexDataType::Float) |
        (TexFormat::LuminanceAlpha32f, TexDataType::Float) => {
            let mut data = Vec::<u8>::with_capacity(pixel_count * 8);
            for rgba8 in pixels.chunks(4) {
                data.write_f32::<NativeEndian>(rgba8[0] as f32).unwrap();
                data.write_f32::<NativeEndian>(rgba8[3] as f32).unwrap();
            }
            data
        },

        (TexFormat::RGBA, TexDataType::HalfFloat) |
        (TexFormat::RGBA16f, TexDataType::HalfFloat) => {
            let mut rgbaf16 = Vec::<u8>::with_capacity(pixel_count * 8);
            for rgba8 in pixels.chunks(4) {
                rgbaf16
                    .write_u16::<NativeEndian>(f16::from_f32(rgba8[0] as f32).to_bits())
                    .unwrap();
                rgbaf16
                    .write_u16::<NativeEndian>(f16::from_f32(rgba8[1] as f32).to_bits())
                    .unwrap();
                rgbaf16
                    .write_u16::<NativeEndian>(f16::from_f32(rgba8[2] as f32).to_bits())
                    .unwrap();
                rgbaf16
                    .write_u16::<NativeEndian>(f16::from_f32(rgba8[3] as f32).to_bits())
                    .unwrap();
            }
            rgbaf16
        },

        (TexFormat::RGB, TexDataType::HalfFloat) | (TexFormat::RGB16f, TexDataType::HalfFloat) => {
            let mut rgbf16 = Vec::<u8>::with_capacity(pixel_count * 6);
            for rgba8 in pixels.chunks(4) {
                rgbf16
                    .write_u16::<NativeEndian>(f16::from_f32(rgba8[0] as f32).to_bits())
                    .unwrap();
                rgbf16
                    .write_u16::<NativeEndian>(f16::from_f32(rgba8[1] as f32).to_bits())
                    .unwrap();
                rgbf16
                    .write_u16::<NativeEndian>(f16::from_f32(rgba8[2] as f32).to_bits())
                    .unwrap();
            }
            rgbf16
        },
        (TexFormat::Alpha, TexDataType::HalfFloat) |
        (TexFormat::Alpha16f, TexDataType::HalfFloat) => {
            for i in 0..pixel_count {
                let p = f16::from_f32(pixels[i * 4 + 3] as f32).to_bits();
                NativeEndian::write_u16(&mut pixels[i * 2..i * 2 + 2], p);
            }
            pixels.truncate(pixel_count * 2);
            pixels
        },
        (TexFormat::Luminance, TexDataType::HalfFloat) |
        (TexFormat::Luminance16f, TexDataType::HalfFloat) => {
            for i in 0..pixel_count {
                let p = f16::from_f32(pixels[i * 4] as f32).to_bits();
                NativeEndian::write_u16(&mut pixels[i * 2..i * 2 + 2], p);
            }
            pixels.truncate(pixel_count * 2);
            pixels
        },
        (TexFormat::LuminanceAlpha, TexDataType::HalfFloat) |
        (TexFormat::LuminanceAlpha16f, TexDataType::HalfFloat) => {
            for rgba8 in pixels.chunks_mut(4) {
                let lum = f16::from_f32(rgba8[0] as f32).to_bits();
                let a = f16::from_f32(rgba8[3] as f32).to_bits();
                NativeEndian::write_u16(&mut rgba8[0..2], lum);
                NativeEndian::write_u16(&mut rgba8[2..4], a);
            }
            pixels
        },

        // Validation should have ensured that we only hit the
        // above cases, but we haven't turned the (format, type)
        // into an enum yet so there's a default case here.
        _ => unreachable!("Unsupported formats {:?} {:?}", format, data_type),
    }
}

fn premultiply_inplace(format: TexFormat, data_type: TexDataType, pixels: &mut [u8]) {
    match (format, data_type) {
        (TexFormat::RGBA, TexDataType::UnsignedByte) => {
            pixels::rgba8_premultiply_inplace(pixels);
        },
        (TexFormat::LuminanceAlpha, TexDataType::UnsignedByte) => {
            for la in pixels.chunks_mut(2) {
                la[0] = pixels::multiply_u8_color(la[0], la[1]);
            }
        },
        (TexFormat::RGBA, TexDataType::UnsignedShort5551) => {
            for rgba in pixels.chunks_mut(2) {
                if NativeEndian::read_u16(rgba) & 1 == 0 {
                    NativeEndian::write_u16(rgba, 0);
                }
            }
        },
        (TexFormat::RGBA, TexDataType::UnsignedShort4444) => {
            for rgba in pixels.chunks_mut(2) {
                let pix = NativeEndian::read_u16(rgba);
                let extend_to_8_bits = |val| (val | (val << 4)) as u8;
                let r = extend_to_8_bits((pix >> 12) & 0x0f);
                let g = extend_to_8_bits((pix >> 8) & 0x0f);
                let b = extend_to_8_bits((pix >> 4) & 0x0f);
                let a = extend_to_8_bits(pix & 0x0f);
                NativeEndian::write_u16(
                    rgba,
                    (((pixels::multiply_u8_color(r, a) & 0xf0) as u16) << 8) |
                        (((pixels::multiply_u8_color(g, a) & 0xf0) as u16) << 4) |
                        ((pixels::multiply_u8_color(b, a) & 0xf0) as u16) |
                        ((a & 0x0f) as u16),
                );
            }
        },
        // Other formats don't have alpha, so return their data untouched.
        _ => {},
    }
}

/// Flips the pixels in the Vec on the Y axis.
fn flip_pixels_y(
    internal_format: TexFormat,
    data_type: TexDataType,
    width: usize,
    height: usize,
    unpacking_alignment: usize,
    pixels: Vec<u8>,
) -> Vec<u8> {
    let cpp = (data_type.element_size() * internal_format.components() /
        data_type.components_per_element()) as usize;

    let stride = (width * cpp + unpacking_alignment - 1) & !(unpacking_alignment - 1);

    let mut flipped = Vec::<u8>::with_capacity(pixels.len());

    for y in 0..height {
        let flipped_y = height - 1 - y;
        let start = flipped_y * stride;

        flipped.extend_from_slice(&pixels[start..(start + width * cpp)]);
        flipped.extend(vec![0u8; stride - width * cpp]);
    }

    flipped
}

// Clamp a size to the current GL context's max viewport
fn clamp_viewport(gl: &Gl, size: Size2D<u32>) -> Size2D<u32> {
    let mut max_viewport = [i32::MAX, i32::MAX];
    let mut max_renderbuffer = [i32::MAX];

    unsafe {
        gl.get_parameter_i32_slice(gl::MAX_VIEWPORT_DIMS, &mut max_viewport);
        gl.get_parameter_i32_slice(gl::MAX_RENDERBUFFER_SIZE, &mut max_renderbuffer);
    }
    Size2D::new(
        size.width
            .min(max_viewport[0] as u32)
            .min(max_renderbuffer[0] as u32)
            .max(1),
        size.height
            .min(max_viewport[1] as u32)
            .min(max_renderbuffer[0] as u32)
            .max(1),
    )
}

trait ToSurfmanVersion {
    fn to_surfman_version(self, api_type: GlType) -> GLVersion;
}

impl ToSurfmanVersion for WebGLVersion {
    fn to_surfman_version(self, api_type: GlType) -> GLVersion {
        if api_type == GlType::Gles {
            return GLVersion::new(3, 0);
        }
        match self {
            // We make use of GL_PACK_PIXEL_BUFFER, which needs at least GL2.1
            // We make use of compatibility mode, which needs at most GL3.0
            WebGLVersion::WebGL1 => GLVersion::new(2, 1),
            // The WebGL2 conformance tests use std140 layout, which needs at GL3.1
            WebGLVersion::WebGL2 => GLVersion::new(3, 2),
        }
    }
}

trait SurfmanContextAttributeFlagsConvert {
    fn to_surfman_context_attribute_flags(
        &self,
        webgl_version: WebGLVersion,
        api_type: GlType,
    ) -> ContextAttributeFlags;
}

impl SurfmanContextAttributeFlagsConvert for GLContextAttributes {
    fn to_surfman_context_attribute_flags(
        &self,
        webgl_version: WebGLVersion,
        api_type: GlType,
    ) -> ContextAttributeFlags {
        let mut flags = ContextAttributeFlags::empty();
        flags.set(ContextAttributeFlags::ALPHA, self.alpha);
        flags.set(ContextAttributeFlags::DEPTH, self.depth);
        flags.set(ContextAttributeFlags::STENCIL, self.stencil);
        if (webgl_version == WebGLVersion::WebGL1) && (api_type == GlType::Gl) {
            flags.set(ContextAttributeFlags::COMPATIBILITY_PROFILE, true);
        }
        flags
    }
}

bitflags! {
    struct FramebufferRebindingFlags: u8 {
        const REBIND_READ_FRAMEBUFFER = 0x1;
        const REBIND_DRAW_FRAMEBUFFER = 0x2;
    }
}

struct FramebufferRebindingInfo {
    flags: FramebufferRebindingFlags,
    viewport: [GLint; 4],
}

impl FramebufferRebindingInfo {
    fn detect(device: &Device, context: &Context, gl: &Gl) -> Result<Self, surfman::Error> {
        let framebuffer = device.context_surface_info(context)?.ok_or(surfman::Error::Failed)?
            .framebuffer_object;
        unsafe {
            let read = gl.get_parameter_framebuffer(gl::READ_FRAMEBUFFER_BINDING);
            let draw = gl.get_parameter_framebuffer(gl::DRAW_FRAMEBUFFER_BINDING);
            let mut flags = FramebufferRebindingFlags::empty();
            flags.set(FramebufferRebindingFlags::REBIND_READ_FRAMEBUFFER, framebuffer == read);
            flags.set(FramebufferRebindingFlags::REBIND_DRAW_FRAMEBUFFER, framebuffer == draw);
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(gl::VIEWPORT, &mut viewport);
            Ok(Self { flags, viewport })
        }
    }

    fn apply(self, device: &Device, context: &Context, gl: &Gl) -> Result<(), surfman::Error> {
        if self.flags.is_empty() { return Ok(()); }
        let framebuffer = device.context_surface_info(context)?.ok_or(surfman::Error::Failed)?
            .framebuffer_object;
        unsafe {
            if self.flags.contains(FramebufferRebindingFlags::REBIND_READ_FRAMEBUFFER) {
                gl.bind_framebuffer(gl::READ_FRAMEBUFFER, framebuffer);
            }
            if self.flags.contains(FramebufferRebindingFlags::REBIND_DRAW_FRAMEBUFFER) {
                gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, framebuffer);
            }
            gl.viewport(self.viewport[0], self.viewport[1], self.viewport[2], self.viewport[3]);
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "webxr", target_os = "linux"))]
mod creation_retirement_tests {
    use super::*;

    fn creation() -> PendingCreation {
        let connection = surfman::mesa_surfaceless::connection::Connection::new().unwrap();
        let adapter = if std::env::var_os("DOME_CREATION_SOFTWARE").is_some() {
            connection.create_software_adapter().unwrap()
        } else { connection.create_low_power_adapter().unwrap() };
        let device = Rc::new(Device::Alternate(connection.create_device(&adapter).unwrap()));
        let descriptor = device.create_context_descriptor(&ContextAttributes {
            version: GLVersion::new(3, 3),
            flags: ContextAttributeFlags::ALPHA | ContextAttributeFlags::DEPTH | ContextAttributeFlags::STENCIL,
        }).unwrap();
        let ctx = device.create_context(&descriptor, None).unwrap();
        device.make_context_current(&ctx).unwrap();
        let gl = Rc::new(unsafe { Gl::try_from_loader_function(|name|device.get_proc_address(&ctx, name)) }.unwrap());
        unsafe { println!("DOME_CREATION_RENDERER={}", gl.get_parameter_string(gl::RENDERER)); }
        PendingCreation {
            device, ctx, returned_surfaces: vec![], chain_id: None,
            chains: SwapChains::new(), gl: Some(gl), retirement: RetirementPhase::default(),
        }
    }
    fn initial_surface(owner: &PendingCreation) -> Surface {
        owner.device.create_surface(&owner.ctx, SurfaceAccess::GPUOnly,
            SurfaceType::Generic { size: Size2D::new(8, 8) }).unwrap()
    }
    fn assert_native_destroyed(owner: &PendingCreation) {
        let (Device::Alternate(device), Context::Alternate(context)) = (&*owner.device, &owner.ctx) else {
            panic!("offscreen alternate required");
        };
        assert!(device.native_context(context).egl_context.is_null());
    }
    #[test]
    fn creation_rollback_owns_surface_returned_by_failed_initial_bind() {
        let mut owner = creation();
        let surface = initial_surface(&owner);
        owner.device.mark_context_lost(&mut owner.ctx).unwrap();
        assert!(owner.bind_initial_surface(surface).is_err());
        assert_eq!(owner.returned_surfaces.len(), 1);
        owner.retire().unwrap();
        assert!(owner.returned_surfaces.is_empty());
        assert_eq!(owner.retirement, RetirementPhase::Complete);
        assert_native_destroyed(&owner);
    }
    #[test]
    fn creation_rollback_releases_attachment_before_native_destruction() {
        let mut owner = creation();
        let surface = initial_surface(&owner);
        owner.bind_initial_surface(surface).unwrap();
        owner.retire().unwrap();
        assert_eq!(owner.retirement, RetirementPhase::Complete);
        assert!(owner.device.context_surface_info(&owner.ctx).unwrap().is_none());
        assert_native_destroyed(&owner);
    }
    #[test]
    fn creation_rollback_releases_registered_chain_and_retains_metadata_until_native_cleanup() {
        let mut owner = creation();
        let surface = initial_surface(&owner);
        owner.bind_initial_surface(surface).unwrap();
        let id = WebGLContextId(1);
        owner.chains.create_attached_swap_chain(id, &owner.device, &mut owner.ctx, SurfaceAccess::GPUOnly).unwrap();
        owner.chain_id = Some(id);
        let metadata = owner.gl.as_ref().unwrap().clone();
        assert_eq!(Rc::strong_count(&metadata), 2);
        owner.retire().unwrap();
        assert!(owner.chains.get(id).is_none());
        assert!(owner.chain_id.is_none());
        assert_native_destroyed(&owner);
        assert_eq!(Rc::strong_count(&metadata), 2);
        drop(owner);
        assert_eq!(Rc::strong_count(&metadata), 1);
    }
    #[test]
    fn creation_abort_preserves_peer_binding_and_framebuffer() {
        let mut owner = creation();
        let surface = initial_surface(&owner);
        owner.bind_initial_surface(surface).unwrap();
        let mut peer = creation();
        let surface = initial_surface(&peer);
        peer.bind_initial_surface(surface).unwrap();
        peer.device.make_context_current(&peer.ctx).unwrap();
        let gl = peer.gl.as_ref().unwrap();
        let before = unsafe { gl.get_parameter_framebuffer(gl::DRAW_FRAMEBUFFER_BINDING) };
        unsafe { gl.clear_color(0., 1., 0., 1.); gl.clear(gl::COLOR_BUFFER_BIT); }
        owner.abort().unwrap();
        let same = unsafe { gl.get_parameter_framebuffer(gl::DRAW_FRAMEBUFFER_BINDING) } == before;
        let mut pixel = [0; 4];
        if same {
            unsafe { gl.read_pixels(0, 0, 1, 1, gl::RGBA, gl::UNSIGNED_BYTE,
                gl::PixelPackData::Slice(Some(&mut pixel))); }
        }
        assert_native_destroyed(&owner);
        peer.retire().unwrap();
        assert!(same);
        assert_eq!(pixel, [0, 255, 0, 255]);
    }

}
