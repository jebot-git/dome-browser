/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use core::slice;
use std::rc::Rc;

use euclid::{
    Angle, Point2D, Rect, RigidTransform3D, Rotation3D, Size2D, Transform3D, UnknownUnit, Vector3D,
};
use glow::{self as gl, Context as Gl, HasContext};
use profile_traits::generic_callback::GenericCallback as ProfileGenericCallback;
use raw_window_handle::DisplayHandle;
use surfman::chains::{PreserveBuffer, SurfaceConsumer, SwapChain, SwapChains, SwapChainsAPI};
use surfman::{
    Adapter, Connection, Context as SurfmanContext, ContextAttributeFlags, ContextAttributes,
    Device as SurfmanDevice, GLApi, GLVersion, NativeWidget, SurfaceAccess, SurfaceType,
};
use webxr_api::util::ClipPlanes;
use webxr_api::{
    CUBE_BACK, CUBE_BOTTOM, CUBE_LEFT, CUBE_RIGHT, CUBE_TOP, ContextId, DeviceAPI, DiscoveryAPI,
    Display, Error, Event, EventBuffer, Floor, Frame, InputSource, LEFT_EYE, LayerGrandManager,
    LayerId, LayerInfo, LayerInit, LayerManager, Native, Quitter, RIGHT_EYE, Session,
    SessionBuilder, SessionInit, SessionMode, SomeEye, VIEWER, View, Viewer, ViewerPose, Viewport,
    Viewports, Views,
};

use crate::{SurfmanGL, SurfmanLayerManager};

// How far off the ground are the viewer's eyes?
const HEIGHT: f32 = 1.0;

// What is half the vertical field of view?
const FOV_UP: f32 = 45.0;

// Some guesstimated numbers, hopefully it doesn't matter if these are off by a bit.

// What the distance between the viewer's eyes?
const INTER_PUPILLARY_DISTANCE: f32 = 0.06;

// What is the size of a pixel?
const PIXELS_PER_METRE: f32 = 6000.0;

pub trait GlWindow {
    fn get_render_target(
        &self,
        device: &mut SurfmanDevice,
        context: &mut SurfmanContext,
    ) -> Result<GlWindowRenderTarget, String>;
    fn get_rotation(&self) -> Rotation3D<f32, UnknownUnit, UnknownUnit>;
    fn get_translation(&self) -> Vector3D<f32, UnknownUnit>;

    fn get_mode(&self) -> GlWindowMode {
        GlWindowMode::Blit
    }
    fn display_handle(&self) -> DisplayHandle<'_>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlWindowMode {
    Blit,
    StereoLeftRight,
    StereoRedCyan,
    Cubemap,
    Spherical,
}

pub enum GlWindowRenderTarget {
    NativeWidget(NativeWidget),
    SwapChain(SwapChain<SurfmanDevice>),
}

pub struct GlWindowDiscovery {
    connection: Connection,
    adapter: Adapter,
    context_attributes: ContextAttributes,
    window: Rc<dyn GlWindow>,
}

impl GlWindowDiscovery {
    pub fn new(window: Rc<dyn GlWindow>) -> GlWindowDiscovery {
        let connection = Connection::from_display_handle(window.display_handle()).unwrap();
        let adapter = connection.create_adapter().unwrap();
        let flags = ContextAttributeFlags::ALPHA
            | ContextAttributeFlags::DEPTH
            | ContextAttributeFlags::STENCIL;
        let version = match connection.gl_api() {
            GLApi::GLES => GLVersion { major: 3, minor: 0 },
            GLApi::GL => GLVersion { major: 3, minor: 2 },
        };
        let context_attributes = ContextAttributes { flags, version };
        GlWindowDiscovery {
            connection,
            adapter,
            context_attributes,
            window,
        }
    }
}

impl DiscoveryAPI<SurfmanGL> for GlWindowDiscovery {
    fn check_context_compatibility(
        &self,
        context: ContextId,
        manager: &LayerGrandManager<SurfmanGL>,
    ) -> Result<(), Error> {
        crate::compatibility::check(
            manager,
            context,
            self.connection.clone(),
            self.adapter.clone(),
        )
    }

    fn request_session(
        &mut self,
        mode: SessionMode,
        init: &SessionInit,
        xr: SessionBuilder<SurfmanGL>,
    ) -> Result<Session, Error> {
        if self.supports_session(mode) {
            let granted_features = init.validate(mode, &["local-floor".into()])?;
            let connection = self.connection.clone();
            let adapter = self.adapter.clone();
            let context_attributes = self.context_attributes;
            let window = self.window.clone();
            xr.run_on_main_thread(move |grand_manager| {
                GlWindowDevice::new(
                    connection,
                    adapter,
                    context_attributes,
                    window,
                    granted_features,
                    grand_manager,
                )
            })
        } else {
            Err(Error::NoMatchingDevice)
        }
    }

    fn supports_session(&self, mode: SessionMode) -> bool {
        mode == SessionMode::ImmersiveVR || mode == SessionMode::ImmersiveAR
    }
}

pub struct GlWindowDevice {
    device: SurfmanDevice,
    context: SurfmanContext,
    gl: Rc<Gl>,
    window: Rc<dyn GlWindow>,
    grand_manager: LayerGrandManager<SurfmanGL>,
    layer_manager: Option<LayerManager>,
    target_swap_chain: Option<SwapChain<SurfmanDevice>>,
    swap_chains: SwapChains<LayerId, SurfmanDevice>,
    read_fbo: Option<gl::NativeFramebuffer>,
    pending_consumer: Option<SurfaceConsumer<SurfmanDevice>>,
    pending_present: Option<surfman::Surface>,
    cleanup_phase: crate::native_cleanup::CleanupPhase,
    events: EventBuffer,
    clip_planes: ClipPlanes,
    granted_features: Vec<String>,
    shader: Option<GlWindowShader>,
}

impl DeviceAPI for GlWindowDevice {
    fn floor_transform(&self) -> Option<RigidTransform3D<f32, Native, Floor>> {
        let translation = Vector3D::new(0.0, HEIGHT, 0.0);
        Some(RigidTransform3D::from_translation(translation))
    }

    fn viewports(&self) -> Viewports {
        let size = self.viewport_size();
        let viewports = match self.window.get_mode() {
            #[expect(clippy::erasing_op, clippy::identity_op)]
            GlWindowMode::Cubemap | GlWindowMode::Spherical => vec![
                Rect::new(Point2D::new(size.width * 1, size.height * 1), size),
                Rect::new(Point2D::new(size.width * 0, size.height * 1), size),
                Rect::new(Point2D::new(size.width * 2, size.height * 1), size),
                Rect::new(Point2D::new(size.width * 2, size.height * 0), size),
                Rect::new(Point2D::new(size.width * 0, size.height * 0), size),
                Rect::new(Point2D::new(size.width * 1, size.height * 0), size),
            ],
            GlWindowMode::Blit | GlWindowMode::StereoLeftRight | GlWindowMode::StereoRedCyan => {
                vec![
                    Rect::new(Point2D::default(), size),
                    Rect::new(Point2D::new(size.width, 0), size),
                ]
            },
        };
        Viewports { viewports }
    }

    fn create_layer(&mut self, context_id: ContextId, init: LayerInit) -> Result<LayerInfo, Error> {
        let viewports = self.viewports();
        self.layer_manager()?
            .create_layer(context_id, init, viewports)
    }

    fn destroy_layer(&mut self, context_id: ContextId, layer_id: LayerId) {
        self.layer_manager()
            .unwrap()
            .destroy_layer(context_id, layer_id)
    }

    fn begin_animation_frame(&mut self, layers: &[(ContextId, LayerId)]) -> Option<Frame> {
        log::debug!("Begin animation frame for layers {:?}", layers);
        let translation = Vector3D::from_untyped(self.window.get_translation());
        let translation: RigidTransform3D<_, _, Native> =
            RigidTransform3D::from_translation(translation);
        let rotation = Rotation3D::from_untyped(&self.window.get_rotation());
        let rotation = RigidTransform3D::from_rotation(rotation);
        let transform = translation.then(&rotation);
        let sub_images = self.layer_manager().ok()?.begin_frame(layers).ok()?;
        Some(Frame {
            pose: Some(ViewerPose {
                transform,
                views: self.views(transform),
            }),
            inputs: vec![],
            events: vec![],
            sub_images,
            hit_test_results: vec![],
            predicted_display_time: 0.0,
            predicted_display_instant: None,
        })
    }

    fn end_animation_frame(&mut self, layers: &[(ContextId, LayerId)]) {
        if let Err(error) = self.render_frame(layers) {
            log::warn!("Deferring XR window frame: {error}");
        }
    }

    fn initial_inputs(&self) -> Vec<InputSource> {
        vec![]
    }

    fn set_event_dest(&mut self, dest: ProfileGenericCallback<Event>) {
        self.events.upgrade(dest)
    }

    fn quit(&mut self) {
        self.events.callback(Event::SessionEnd);
    }

    fn set_quitter(&mut self, _: Quitter) {
        // Glwindow currently doesn't have any way to end its own session
        // XXXManishearth add something for this that listens for the window
        // being closed
    }

    fn update_clip_planes(&mut self, near: f32, far: f32) {
        self.clip_planes.update(near, far)
    }

    fn poll_cleanup(&mut self) -> bool {
        let mut phase = self.cleanup_phase;
        let complete = crate::native_cleanup::advance(&mut phase, self);
        self.cleanup_phase = phase;
        complete
    }
    fn granted_features(&self) -> &[String] {
        &self.granted_features
    }
}

impl crate::native_cleanup::CleanupOwner for GlWindowDevice {
    type Error = String;
    fn release_resources(&mut self) -> Result<(), String> {
        self.layer_manager.take();
        self.return_consumer()?;
        if let Some(surface) = self.pending_present.as_mut() {
            self.device
                .destroy_surface(&mut self.context, surface)
                .map_err(|e| format!("{e:?}"))?;
            self.pending_present = None;
        }
        if let Some(chain) = self.target_swap_chain.as_ref() {
            chain
                .destroy(&self.device, &mut self.context)
                .map_err(|e| format!("{e:?}"))?;
            self.target_swap_chain = None;
        }
        let lost = self.device.context_is_lost(&self.context);
        if !lost {
            self.device
                .make_context_current(&self.context)
                .map_err(|e| format!("{e:?}"))?;
        }
        if let Some(shader) = self.shader.as_mut() {
            shader.dispose(lost);
        }
        self.shader = None;
        if let Some(read_fbo) = self.read_fbo.take() {
            if !lost {
                unsafe {
                    self.gl.delete_framebuffer(read_fbo);
                }
            }
        }
        Ok(())
    }
    fn destroy_native(&mut self) -> Result<(), String> {
        self.device
            .destroy_context(&mut self.context)
            .map_err(|e| format!("{e:?}"))
    }
}
impl Drop for GlWindowDevice {
    fn drop(&mut self) {
        self.poll_cleanup();
    }
}

// Failed binds must return ownership to the presentation slot, including a
// surface already presented but not yet rebound to its rendering context.
fn restore_present_surface(
    device: &SurfmanDevice,
    context: &mut SurfmanContext,
    pending: &mut Option<surfman::Surface>,
) -> Result<(), String> {
    let Some(surface) = pending.take() else {
        return Ok(());
    };
    match device.bind_surface_to_context(context, surface) {
        Ok(()) => Ok(()),
        Err((error, surface)) => {
            *pending = Some(surface);
            Err(format!("{error:?}"))
        },
    }
}

impl GlWindowDevice {
    fn return_consumer(&mut self) -> Result<(), String> {
        if let Some(consumer) = self.pending_consumer.as_mut() {
            consumer
                .release(&self.device, &mut self.context)
                .map_err(|e| format!("{e:?}"))?;
        }
        self.pending_consumer = None;
        Ok(())
    }
    fn restore_present_surface(&mut self) -> Result<(), String> {
        restore_present_surface(&self.device, &mut self.context, &mut self.pending_present)
    }
    fn render_frame(&mut self, layers: &[(ContextId, LayerId)]) -> Result<(), String> {
        log::debug!("End animation frame for layers {:?}", layers);
        self.device
            .make_context_current(&self.context)
            .map_err(|e| format!("{e:?}"))?;
        self.return_consumer()?;
        self.restore_present_surface()?;
        debug_assert_eq!(unsafe { self.gl.get_error() }, gl::NO_ERROR);

        self.layer_manager()
            .map_err(|e| format!("{e:?}"))?
            .end_frame(layers)
            .map_err(|e| format!("{e:?}"))?;

        let window_size = self.window_size();
        let viewport_size = self.viewport_size();

        let framebuffer_object = self
            .device
            .context_surface_info(&self.context)
            .map_err(|e| format!("{e:?}"))?
            .and_then(|info| info.framebuffer_object);
        unsafe {
            self.gl
                .bind_framebuffer(gl::FRAMEBUFFER, framebuffer_object);
            debug_assert_eq!(
                (
                    self.gl.get_error(),
                    self.gl.check_framebuffer_status(gl::FRAMEBUFFER)
                ),
                (gl::NO_ERROR, gl::FRAMEBUFFER_COMPLETE)
            );

            self.gl.clear_color(0.0, 0.0, 0.0, 0.0);
            self.gl.clear(gl::COLOR_BUFFER_BIT);
            debug_assert_eq!(self.gl.get_error(), gl::NO_ERROR);
        }

        for &(_, layer_id) in layers {
            let swap_chain = match self.swap_chains.get(layer_id) {
                Some(swap_chain) => swap_chain,
                None => continue,
            };
            let Some(consumer) =
                SurfaceConsumer::acquire(swap_chain, &self.device, &mut self.context)
                    .map_err(|e| format!("{e:?}"))?
            else {
                continue;
            };
            let texture_size = consumer.size();
            let texture_id = self
                .device
                .surface_texture_object(consumer.texture().unwrap());
            self.pending_consumer = Some(consumer);
            let texture_target = self.device.surface_gl_texture_target();
            log::debug!("Presenting texture {:?}", texture_id);

            if let Some(ref shader) = self.shader {
                shader.draw_texture(
                    texture_id,
                    texture_target,
                    texture_size,
                    viewport_size,
                    window_size,
                );
            } else {
                self.blit_texture(texture_id, texture_target, texture_size, window_size);
            }
            debug_assert_eq!(unsafe { self.gl.get_error() }, gl::NO_ERROR);

            self.return_consumer()?;
        }

        match self.target_swap_chain.as_ref() {
            Some(target_swap_chain) => {
                // Rendering to a surfman swap chain
                target_swap_chain
                    .swap_buffers(&self.device, &mut self.context, PreserveBuffer::No)
                    .map_err(|e| format!("{e:?}"))?;
            },
            None => {
                // Rendering to a native widget
                self.pending_present = self
                    .device
                    .unbind_surface_from_context(&mut self.context)
                    .map_err(|e| format!("{e:?}"))?;
                let surface = self
                    .pending_present
                    .as_mut()
                    .ok_or("XR window has no surface")?;
                self.device
                    .present_surface(&self.context, surface)
                    .map_err(|e| format!("{e:?}"))?;
                self.restore_present_surface()?;
            },
        }

        debug_assert_eq!(unsafe { self.gl.get_error() }, gl::NO_ERROR);
        Ok(())
    }

    fn new(
        connection: Connection,
        adapter: Adapter,
        context_attributes: ContextAttributes,
        window: Rc<dyn GlWindow>,
        granted_features: Vec<String>,
        grand_manager: LayerGrandManager<SurfmanGL>,
    ) -> Result<GlWindowDevice, Error> {
        let device = connection
            .create_device(&adapter)
            .map_err(|e| Error::BackendSpecific(format!("{e:?}")))?;
        let context_descriptor = device
            .create_context_descriptor(&context_attributes)
            .map_err(|e| Error::BackendSpecific(format!("{e:?}")))?;
        let context = device
            .create_context(&context_descriptor, None)
            .map_err(|e| Error::BackendSpecific(format!("{e:?}")))?;
        let mut pending = crate::pending_context::PendingContext::new(device, context);
        let (device, context) = pending.parts();
        device
            .make_context_current(context)
            .map_err(|e| Error::BackendSpecific(format!("{e:?}")))?;
        let gl = Rc::new(
            unsafe { Gl::try_from_loader_function(|name| device.get_proc_address(context, name)) }
                .map_err(|e| Error::BackendSpecific(format!("{e:?}")))?,
        );
        let (device, context) = pending.parts_mut();
        match window
            .get_render_target(device, context)
            .map_err(Error::BackendSpecific)?
        {
            GlWindowRenderTarget::NativeWidget(native_widget) => {
                let surface = device
                    .create_surface(
                        context,
                        SurfaceAccess::GPUOnly,
                        SurfaceType::Widget { native_widget },
                    )
                    .map_err(|e| Error::BackendSpecific(format!("{e:?}")))?;
                pending
                    .bind(surface)
                    .map_err(|e| Error::BackendSpecific(format!("{e:?}")))?;
            },
            GlWindowRenderTarget::SwapChain(chain) => pending.adopt_chain(chain),
        }
        let (device, context) = pending.parts();
        device
            .make_context_current(context)
            .map_err(|e| Error::BackendSpecific(format!("{e:?}")))?;
        let read_fbo = Some(unsafe { gl.create_framebuffer() }.map_err(Error::BackendSpecific)?);
        let framebuffer_object = device
            .context_surface_info(context)
            .map_err(|e| Error::BackendSpecific(format!("{e:?}")))?
            .ok_or_else(|| {
                Error::BackendSpecific("XR render target has no attached surface".into())
            })?
            .framebuffer_object;
        unsafe {
            gl.bind_framebuffer(gl::FRAMEBUFFER, framebuffer_object);
            if gl.check_framebuffer_status(gl::FRAMEBUFFER) != gl::FRAMEBUFFER_COMPLETE {
                return Err(Error::BackendSpecific("Incomplete XR render target".into()));
            }
            gl.enable(gl::BLEND);
            gl.blend_func_separate(
                gl::SRC_ALPHA,
                gl::ONE_MINUS_SRC_ALPHA,
                gl::ONE,
                gl::ONE_MINUS_SRC_ALPHA,
            );
        }
        let swap_chains = SwapChains::new();
        let layer_manager = None;

        let shader = GlWindowShader::new(gl.clone(), window.get_mode())?;
        let target_swap_chain = pending.take_chain();
        let (device, context) = pending.commit();

        Ok(GlWindowDevice {
            gl,
            window,
            device,
            context,
            read_fbo,
            pending_consumer: None,
            pending_present: None,
            cleanup_phase: Default::default(),
            swap_chains,
            target_swap_chain,
            grand_manager,
            layer_manager,
            events: Default::default(),
            clip_planes: Default::default(),
            granted_features,
            shader,
        })
    }

    fn blit_texture(
        &self,
        texture_id: Option<gl::NativeTexture>,
        texture_target: u32,
        texture_size: Size2D<i32, UnknownUnit>,
        window_size: Size2D<i32, Viewport>,
    ) {
        unsafe {
            self.gl
                .bind_framebuffer(gl::READ_FRAMEBUFFER, self.read_fbo);
            self.gl.framebuffer_texture_2d(
                gl::READ_FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                texture_target,
                texture_id,
                0,
            );
            self.gl.blit_framebuffer(
                0,
                0,
                texture_size.width,
                texture_size.height,
                0,
                0,
                window_size.width,
                window_size.height,
                gl::COLOR_BUFFER_BIT,
                gl::NEAREST,
            );
        }
    }

    fn layer_manager(&mut self) -> Result<&mut LayerManager, Error> {
        if let Some(ref mut manager) = self.layer_manager {
            return Ok(manager);
        }
        let swap_chains = self.swap_chains.clone();
        let layer_manager = self
            .grand_manager
            .create_layer_manager(move |_| Ok(SurfmanLayerManager::new(swap_chains)))?;
        self.layer_manager = Some(layer_manager);
        Ok(self.layer_manager.as_mut().unwrap())
    }

    fn window_size(&self) -> Size2D<i32, Viewport> {
        let window_size = self
            .device
            .context_surface_info(&self.context)
            .unwrap()
            .unwrap()
            .size
            .to_i32();
        Size2D::from_untyped(window_size)
    }

    fn viewport_size(&self) -> Size2D<i32, Viewport> {
        let window_size = self.window_size();
        match self.window.get_mode() {
            GlWindowMode::StereoRedCyan => {
                // This device has a slightly odd characteristic, which is that anaglyphic stereo
                // renders both eyes to the same surface. If we want the two eyes to be parallel,
                // and to agree at distance infinity, this means gettng the XR content to render some
                // wasted pixels, which are stripped off when we render to the target surface.
                // (The wasted pixels are on the right of the left eye and vice versa.)
                let wasted_pixels = (INTER_PUPILLARY_DISTANCE / PIXELS_PER_METRE) as i32;
                Size2D::new(window_size.width + wasted_pixels, window_size.height)
            },
            GlWindowMode::Cubemap => {
                // Cubemap viewports should be square
                let size = 1.max(window_size.width / 3).max(window_size.height / 2);
                Size2D::new(size, size)
            },
            GlWindowMode::Spherical => {
                // Cubemap viewports should be square
                let size = 1.max(window_size.width / 2).max(window_size.height);
                Size2D::new(size, size)
            },
            GlWindowMode::StereoLeftRight | GlWindowMode::Blit => {
                Size2D::new(window_size.width / 2, window_size.height)
            },
        }
    }

    fn views(&self, viewer: RigidTransform3D<f32, Viewer, Native>) -> Views {
        match self.window.get_mode() {
            GlWindowMode::Cubemap | GlWindowMode::Spherical => Views::Cubemap(
                self.view(viewer, VIEWER),
                self.view(viewer, CUBE_LEFT),
                self.view(viewer, CUBE_RIGHT),
                self.view(viewer, CUBE_TOP),
                self.view(viewer, CUBE_BOTTOM),
                self.view(viewer, CUBE_BACK),
            ),
            GlWindowMode::Blit | GlWindowMode::StereoLeftRight | GlWindowMode::StereoRedCyan => {
                Views::Stereo(self.view(viewer, LEFT_EYE), self.view(viewer, RIGHT_EYE))
            },
        }
    }

    fn view<Eye>(
        &self,
        viewer: RigidTransform3D<f32, Viewer, Native>,
        eye: SomeEye<Eye>,
    ) -> View<Eye> {
        let projection = self.perspective();
        let translation = if eye == RIGHT_EYE {
            Vector3D::new(-INTER_PUPILLARY_DISTANCE / 2.0, 0.0, 0.0)
        } else if eye == LEFT_EYE {
            Vector3D::new(INTER_PUPILLARY_DISTANCE / 2.0, 0.0, 0.0)
        } else {
            Vector3D::zero()
        };
        let rotation = if eye == CUBE_TOP {
            Rotation3D::euler(
                Angle::degrees(270.0),
                Angle::degrees(0.0),
                Angle::degrees(90.0),
            )
        } else if eye == CUBE_BOTTOM {
            Rotation3D::euler(
                Angle::degrees(90.0),
                Angle::degrees(0.0),
                Angle::degrees(90.0),
            )
        } else if eye == CUBE_LEFT {
            Rotation3D::around_y(Angle::degrees(-90.0))
        } else if eye == CUBE_RIGHT {
            Rotation3D::around_y(Angle::degrees(90.0))
        } else if eye == CUBE_BACK {
            Rotation3D::euler(
                Angle::degrees(180.0),
                Angle::degrees(0.0),
                Angle::degrees(90.0),
            )
        } else {
            Rotation3D::identity()
        };
        let transform: RigidTransform3D<f32, Viewer, Eye> =
            RigidTransform3D::new(rotation, translation);
        View {
            transform: transform.inverse().then(&viewer),
            projection,
        }
    }

    fn perspective<Eye>(&self) -> Transform3D<f32, Eye, Display> {
        let near = self.clip_planes.near;
        let far = self.clip_planes.far;
        // https://github.com/toji/gl-matrix/blob/bd3307196563fbb331b40fc6ebecbbfcc2a4722c/src/mat4.js#L1271
        let fov_up = match self.window.get_mode() {
            GlWindowMode::Spherical | GlWindowMode::Cubemap => Angle::degrees(45.0),
            GlWindowMode::Blit | GlWindowMode::StereoLeftRight | GlWindowMode::StereoRedCyan => {
                Angle::degrees(FOV_UP)
            },
        };
        let f = 1.0 / fov_up.radians.tan();
        let nf = 1.0 / (near - far);
        let viewport_size = self.viewport_size();
        let aspect = viewport_size.width as f32 / viewport_size.height as f32;

        // Sigh, row-major vs column-major
        Transform3D::from_arrays([
            [f / aspect, 0.0, 0.0, 0.0],
            [0.0, f, 0.0, 0.0],
            [0.0, 0.0, (far + near) * nf, -1.0],
            [0.0, 0.0, 2.0 * far * near * nf, 0.0],
        ])
    }
}

struct GlWindowShader {
    disposed: bool,
    gl: Rc<Gl>,
    buffer: Option<gl::NativeBuffer>,
    vao: Option<gl::NativeVertexArray>,
    program: gl::NativeProgram,
    mode: GlWindowMode,
}

const VERTEX_ATTRIBUTE: u32 = 0;
const VERTICES: &[[f32; 2]; 4] = &[[-1.0, -1.0], [-1.0, 1.0], [1.0, -1.0], [1.0, 1.0]];

const PASSTHROUGH_VERTEX_SHADER: &str = "
  in vec2 coord;
  out vec2 vTexCoord;
  void main(void) {
    gl_Position = vec4(coord, 0.0, 1.0);
    vTexCoord = coord * 0.5 + 0.5;
  }
";

const PASSTHROUGH_FRAGMENT_SHADER: &str = "
  out vec4 color;
  uniform sampler2D image;
  in vec2 vTexCoord;
  void main() {
    color = texture(image, vTexCoord);
  }
";

const ANAGLYPH_VERTEX_SHADER: &str = "
  in vec2 coord;
  uniform float wasted; // What fraction of the image is wasted?
  out vec2 left_coord;
  out vec2 right_coord;
  void main(void) {
    gl_Position = vec4(coord, 0.0, 1.0);
    vec2 coordn = coord * 0.5 + 0.5;
    left_coord = vec2(mix(wasted/2.0, 0.5, coordn.x), coordn.y);
    right_coord = vec2(mix(0.5, 1.0-wasted/2.0, coordn.x), coordn.y);
  }
";

const ANAGLYPH_RED_CYAN_FRAGMENT_SHADER: &str = "
  out vec4 color;
  uniform sampler2D image;
  in vec2 left_coord;
  in vec2 right_coord;
  void main() {
    vec4 left_color = texture(image, left_coord);
    vec4 right_color = texture(image, right_coord);
    float red = left_color.x;
    float green = right_color.y;
    float blue = right_color.z;
    color = vec4(red, green, blue, 1.0);
  }
";

const SPHERICAL_VERTEX_SHADER: &str = "
  in vec2 coord;
  out vec2 lon_lat;
  const float PI = 3.141592654;
  void main(void) {
    lon_lat = coord * vec2(PI, 0.5*PI);
    gl_Position = vec4(coord, 0.0, 1.0);
  }
";

const SPHERICAL_FRAGMENT_SHADER: &str = "
  out vec4 color;
  uniform sampler2D image;
  in vec2 lon_lat;
  void main() {
    vec3 direction = vec3(
      sin(lon_lat.x)*cos(lon_lat.y),
      sin(lon_lat.y),
      cos(lon_lat.x)*cos(lon_lat.y)
    );
    vec2 vTexCoord;
    if ((direction.y > abs(direction.x)) && (direction.y > abs(direction.z))) {
      // Looking up
      vTexCoord.x = direction.z / (direction.y*6.0) + 5.0/6.0;
      vTexCoord.y = direction.x / (direction.y*4.0) + 1.0/4.0;
    } else if ((direction.y < -abs(direction.x)) && (direction.y < -abs(direction.z))) {
      // Looking down
      vTexCoord.x = direction.z / (direction.y*6.0) + 1.0/6.0;
      vTexCoord.y = -direction.x / (direction.y*4.0) + 1.0/4.0;
    } else if (direction.z < -abs(direction.x)) {
      // Looking back
      vTexCoord.x = -direction.y / (direction.z*6.0) + 3.0/6.0;
      vTexCoord.y = -direction.x / (direction.z*4.0) + 1.0/4.0;
    } else if (direction.x < -abs(direction.z)) {
      // Looking left
      vTexCoord.x = -direction.z / (direction.x*6.0) + 1.0/6.0;
      vTexCoord.y = -direction.y / (direction.x*4.0) + 3.0/4.0;
    } else if (direction.x > abs(direction.z)) {
      // Looking right
      vTexCoord.x = -direction.z / (direction.x*6.0) + 5.0/6.0;
      vTexCoord.y = direction.y / (direction.x*4.0) + 3.0/4.0;
    } else {
      // Looking ahead
      vTexCoord.x = direction.x / (direction.z*6.0) + 3.0/6.0;
      vTexCoord.y = direction.y / (direction.z*4.0) + 3.0/4.0;
    }
    color = texture(image, vTexCoord);
  }
";

fn shader_sources(mode: GlWindowMode, embedded: bool) -> Option<(String, String)> {
    let (vertex, fragment) = match mode {
        GlWindowMode::Blit => return None,
        GlWindowMode::StereoLeftRight | GlWindowMode::Cubemap => {
            (PASSTHROUGH_VERTEX_SHADER, PASSTHROUGH_FRAGMENT_SHADER)
        },
        GlWindowMode::StereoRedCyan => (ANAGLYPH_VERTEX_SHADER, ANAGLYPH_RED_CYAN_FRAGMENT_SHADER),
        GlWindowMode::Spherical => (SPHERICAL_VERTEX_SHADER, SPHERICAL_FRAGMENT_SHADER),
    };
    let header = if embedded {
        "#version 300 es\nprecision highp float;\nprecision highp int;\n"
    } else {
        "#version 150 core\n"
    };
    let fragment = if embedded {
        fragment.replace("out vec4 color;", "layout(location=0) out vec4 color;")
    } else {
        fragment.into()
    };
    Some((
        format!("{header}{}", vertex.trim_start()),
        format!("{header}{}", fragment.trim_start()),
    ))
}

impl GlWindowShader {
    fn new(gl: Rc<Gl>, mode: GlWindowMode) -> Result<Option<GlWindowShader>, Error> {
        let Some((vertex_source, fragment_source)) = shader_sources(mode, gl.version().is_embedded)
        else {
            return Ok(None);
        };

        unsafe {
            // The four corners of the window in a VAO, set to attribute 0
            let buffer = Some(gl.create_buffer().map_err(Error::BackendSpecific)?);
            let vao = Some(gl.create_vertex_array().map_err(Error::BackendSpecific)?);
            gl.bind_buffer(gl::ARRAY_BUFFER, buffer);

            let data =
                slice::from_raw_parts(VERTICES as *const _ as _, std::mem::size_of_val(VERTICES));
            gl.buffer_data_u8_slice(gl::ARRAY_BUFFER, data, gl::STATIC_DRAW);

            gl.bind_vertex_array(vao);
            gl.vertex_attrib_pointer_f32(
                VERTEX_ATTRIBUTE,
                VERTICES[0].len() as i32,
                gl::FLOAT,
                false,
                0,
                0,
            );
            gl.enable_vertex_attrib_array(VERTEX_ATTRIBUTE);

            // The shader program
            let program = gl.create_program().map_err(Error::BackendSpecific)?;
            let vertex_shader = gl
                .create_shader(gl::VERTEX_SHADER)
                .map_err(Error::BackendSpecific)?;
            let fragment_shader = gl
                .create_shader(gl::FRAGMENT_SHADER)
                .map_err(Error::BackendSpecific)?;
            gl.shader_source(vertex_shader, &vertex_source);
            gl.compile_shader(vertex_shader);
            if !gl.get_shader_compile_status(vertex_shader) {
                return Err(Error::BackendSpecific(format!(
                    "XR vertex shader: {}",
                    gl.get_shader_info_log(vertex_shader)
                )));
            }
            gl.shader_source(fragment_shader, &fragment_source);
            gl.compile_shader(fragment_shader);
            if !gl.get_shader_compile_status(fragment_shader) {
                return Err(Error::BackendSpecific(format!(
                    "XR fragment shader: {}",
                    gl.get_shader_info_log(fragment_shader)
                )));
            }
            gl.attach_shader(program, vertex_shader);
            gl.attach_shader(program, fragment_shader);
            gl.bind_attrib_location(program, VERTEX_ATTRIBUTE, "coord");
            if !gl.version().is_embedded {
                gl.bind_frag_data_location(program, 0, "color");
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(Error::BackendSpecific(format!(
                    "XR shader link: {}",
                    gl.get_program_info_log(program)
                )));
            }

            // Clean up
            gl.delete_shader(vertex_shader);
            gl.delete_shader(fragment_shader);

            // And we're done
            Ok(Some(GlWindowShader {
                disposed: false,
                gl,
                buffer,
                vao,
                program,
                mode,
            }))
        }
    }

    fn draw_texture(
        &self,
        texture_id: Option<gl::NativeTexture>,
        texture_target: u32,
        texture_size: Size2D<i32, UnknownUnit>,
        viewport_size: Size2D<i32, Viewport>,
        window_size: Size2D<i32, Viewport>,
    ) {
        unsafe {
            self.gl.use_program(Some(self.program));

            self.gl.enable_vertex_attrib_array(VERTEX_ATTRIBUTE);
            self.gl.vertex_attrib_pointer_f32(
                VERTEX_ATTRIBUTE,
                VERTICES[0].len() as i32,
                gl::FLOAT,
                false,
                0,
                0,
            );

            debug_assert_eq!(self.gl.get_error(), gl::NO_ERROR);

            self.gl.active_texture(gl::TEXTURE0);
            self.gl.bind_texture(texture_target, texture_id);

            match self.mode {
                GlWindowMode::StereoRedCyan => {
                    let wasted = 1.0
                        - (texture_size.width as f32 / viewport_size.width as f32).clamp(0.0, 1.0);
                    let wasted_location = self.gl.get_uniform_location(self.program, "wasted");
                    self.gl.uniform_1_f32(wasted_location.as_ref(), wasted);
                },
                GlWindowMode::Blit
                | GlWindowMode::Cubemap
                | GlWindowMode::Spherical
                | GlWindowMode::StereoLeftRight => {},
            }

            self.gl
                .viewport(0, 0, window_size.width, window_size.height);
            self.gl
                .draw_arrays(gl::TRIANGLE_STRIP, 0, VERTICES.len() as i32);
            self.gl.disable_vertex_attrib_array(VERTEX_ATTRIBUTE);
            debug_assert_eq!(self.gl.get_error(), gl::NO_ERROR);
        }
    }
}

impl GlWindowShader {
    fn dispose(&mut self, lost: bool) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        if lost {
            return;
        }
        unsafe {
            if let Some(buffer) = self.buffer {
                self.gl.delete_buffer(buffer);
            }
            if let Some(vao) = self.vao {
                self.gl.delete_vertex_array(vao);
            }
            self.gl.delete_program(self.program);
        }
    }
}

impl Drop for GlWindowShader {
    fn drop(&mut self) {
        self.dispose(false);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod presentation_tests {
    use super::*;
    #[test]
    fn failed_presentation_rebind_retains_surface_for_retry() {
        let connection = surfman::mesa_surfaceless::connection::Connection::new().unwrap();
        let adapter = if std::env::var_os("DOME_XR_RETIRE_SOFTWARE").is_some() {
            connection.create_software_adapter().unwrap()
        } else {
            connection.create_low_power_adapter().unwrap()
        };
        let device = SurfmanDevice::Alternate(connection.create_device(&adapter).unwrap());
        let descriptor = device
            .create_context_descriptor(&ContextAttributes {
                version: GLVersion::new(3, 2),
                flags: ContextAttributeFlags::ALPHA,
            })
            .unwrap();
        let mut owner = device.create_context(&descriptor, None).unwrap();
        let mut other = device.create_context(&descriptor, None).unwrap();
        let surface = device
            .create_surface(
                &owner,
                SurfaceAccess::GPUOnly,
                SurfaceType::Generic {
                    size: Size2D::new(8, 8),
                },
            )
            .unwrap();
        let id = device.surface_info(&surface).id;
        let mut pending = Some(surface);
        let failed = restore_present_surface(&device, &mut other, &mut pending).is_err();
        let retained = pending.as_ref().map(|s| device.surface_info(s).id) == Some(id);
        restore_present_surface(&device, &mut owner, &mut pending).unwrap();
        let released = pending.is_none();
        restore_present_surface(&device, &mut owner, &mut pending).unwrap();
        let attached = device.context_surface_info(&owner).unwrap().unwrap().id == id;
        device.destroy_context(&mut owner).unwrap();
        device.destroy_context(&mut other).unwrap();
        assert!(failed && retained && released && attached);
    }
    struct PrivateWindow {
        reject: bool,
        detached: bool,
        mode: GlWindowMode,
    }
    impl GlWindow for PrivateWindow {
        fn get_render_target(
            &self,
            device: &mut SurfmanDevice,
            context: &mut SurfmanContext,
        ) -> Result<GlWindowRenderTarget, String> {
            if self.reject {
                return Err("XR fixture target unavailable".into());
            }
            let surface = device
                .create_surface(
                    context,
                    SurfaceAccess::GPUOnly,
                    SurfaceType::Generic {
                        size: Size2D::new(8, 8),
                    },
                )
                .unwrap();
            device.bind_surface_to_context(context, surface).unwrap();
            let chain =
                SwapChain::create_attached(device, context, SurfaceAccess::GPUOnly).unwrap();
            if self.detached {
                chain
                    .take_surface_texture(device, context)
                    .map(|texture| {
                        let mut slot = Some(texture);
                        chain
                            .recycle_surface_texture(device, context, &mut slot)
                            .unwrap();
                    })
                    .unwrap();
                // A valid, independently owned detached target is incompatible
                // with presentation, but must still be retired without panic.
                chain.destroy(device, context).unwrap();
                return Ok(GlWindowRenderTarget::SwapChain(
                    SwapChain::create_detached(
                        device,
                        context,
                        SurfaceAccess::GPUOnly,
                        Size2D::new(8, 8),
                    )
                    .unwrap(),
                ));
            }
            Ok(GlWindowRenderTarget::SwapChain(chain))
        }
        fn get_rotation(&self) -> Rotation3D<f32, UnknownUnit, UnknownUnit> {
            Rotation3D::identity()
        }
        fn get_translation(&self) -> Vector3D<f32, UnknownUnit> {
            Vector3D::zero()
        }
        fn get_mode(&self) -> GlWindowMode {
            self.mode
        }
        fn display_handle(&self) -> DisplayHandle<'_> {
            unreachable!("offscreen fixture opens no desktop")
        }
    }
    #[derive(Clone)]
    struct UnusedGrand;
    impl webxr_api::LayerGrandManagerAPI<SurfmanGL> for UnusedGrand {
        fn create_layer_manager(
            &self,
            _: webxr_api::LayerManagerFactory<SurfmanGL>,
        ) -> Result<LayerManager, Error> {
            unreachable!()
        }
        fn clone_layer_grand_manager(&self) -> LayerGrandManager<SurfmanGL> {
            LayerGrandManager::new(self.clone())
        }
    }
    fn private_window(detached: bool, mode: GlWindowMode) -> Result<GlWindowDevice, Error> {
        private_window_with_rejection(detached, mode, false)
    }
    fn private_window_with_rejection(
        detached: bool,
        mode: GlWindowMode,
        reject: bool,
    ) -> Result<GlWindowDevice, Error> {
        let connection = surfman::mesa_surfaceless::connection::Connection::new().unwrap();
        let adapter = if std::env::var_os("DOME_XR_RETIRE_SOFTWARE").is_some() {
            connection.create_software_adapter().unwrap()
        } else {
            connection.create_low_power_adapter().unwrap()
        };
        GlWindowDevice::new(
            Connection::Alternate(connection),
            Adapter::Alternate(adapter),
            ContextAttributes {
                version: GLVersion::new(3, 2),
                flags: ContextAttributeFlags::ALPHA,
            },
            Rc::new(PrivateWindow {
                reject,
                detached,
                mode,
            }),
            vec![],
            LayerGrandManager::new(UnusedGrand),
        )
    }
    #[test]
    fn private_blit_initialization_and_cleanup_succeed() {
        let mut window = private_window(false, GlWindowMode::Blit).unwrap();
        assert!(window.read_fbo.is_some() && window.target_swap_chain.is_some());
        assert!(window.poll_cleanup());
        assert!(window.poll_cleanup());
        assert_eq!(
            window.cleanup_phase,
            crate::native_cleanup::CleanupPhase::Complete
        );
        assert!(window.read_fbo.is_none() && window.target_swap_chain.is_none());
    }
    #[test]
    fn private_shader_is_disposed_before_native_context() {
        let mut window = private_window(false, GlWindowMode::StereoLeftRight).unwrap();
        assert!(window.shader.is_some());
        assert!(window.poll_cleanup());
        assert!(window.shader.is_none());
        assert!(window.poll_cleanup());
    }
    #[test]
    fn rejected_detached_target_rolls_back_instead_of_dropping_live_owner() {
        assert!(private_window(true, GlWindowMode::Blit).is_err());
        assert!(!webxr_api::deferred_cleanup::pending());
    }
    #[test]
    fn host_target_error_releases_unpublished_context() {
        let result = private_window_with_rejection(false, GlWindowMode::Blit, true);
        assert!(
            matches!(result,Err(Error::BackendSpecific(message)) if message.contains("target unavailable"))
        );
        assert!(!webxr_api::deferred_cleanup::pending());
    }
    #[test]
    fn all_shader_modes_draw_native_pixels() {
        for mode in [
            GlWindowMode::StereoLeftRight,
            GlWindowMode::StereoRedCyan,
            GlWindowMode::Cubemap,
            GlWindowMode::Spherical,
        ] {
            let mut window = private_window(false, mode).unwrap();
            let mut pixel = [0; 4];
            let error;
            unsafe {
                let gl = &window.gl;
                let target = window
                    .device
                    .context_surface_info(&window.context)
                    .unwrap()
                    .unwrap()
                    .framebuffer_object;
                gl.bind_framebuffer(gl::FRAMEBUFFER, target);
                let texture = gl.create_texture().unwrap();
                gl.bind_texture(gl::TEXTURE_2D, Some(texture));
                gl.tex_parameter_i32(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
                gl.tex_parameter_i32(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
                gl.tex_image_2d(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA as i32,
                    2,
                    2,
                    0,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    gl::PixelUnpackData::Slice(Some(&[
                        0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255,
                    ])),
                );
                window.shader.as_ref().unwrap().draw_texture(
                    Some(texture),
                    gl::TEXTURE_2D,
                    Size2D::new(2, 2),
                    Size2D::new(8, 8),
                    Size2D::new(8, 8),
                );
                gl.read_pixels(
                    4,
                    4,
                    1,
                    1,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    gl::PixelPackData::Slice(Some(&mut pixel)),
                );
                error = gl.get_error();
                gl.delete_texture(texture);
            }
            assert!(window.poll_cleanup());
            assert_eq!(pixel, [0, 255, 0, 255], "mode {mode:?}");
            assert_eq!(error, gl::NO_ERROR, "mode {mode:?}");
        }
    }
    #[test]
    #[ignore = "Requires independent glslangValidator; run explicitly with --ignored"]
    fn shader_sources_pass_standard_validator() {
        let out = std::env::var_os("DOME_XR_GLSL_TEST_OUTPUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("dome-glsl-{}", std::process::id()))
            });
        std::fs::create_dir_all(&out).unwrap();
        let validator =
            std::env::var_os("DOME_XR_GLSL_VALIDATOR").unwrap_or_else(|| "glslangValidator".into());
        for embedded in [false, true] {
            for (name, mode) in [
                ("passthrough", GlWindowMode::StereoLeftRight),
                ("anaglyph", GlWindowMode::StereoRedCyan),
                ("spherical", GlWindowMode::Spherical),
            ] {
                let (vertex, fragment) = shader_sources(mode, embedded).unwrap();
                let name = format!("{}-{name}", if embedded { "gles300" } else { "gl150" });
                let vp = out.join(format!("{name}.vert"));
                let fp = out.join(format!("{name}.frag"));
                std::fs::write(&vp, vertex).unwrap();
                std::fs::write(&fp, fragment).unwrap();
                let result = std::process::Command::new(&validator)
                    .arg("-l")
                    .arg(vp)
                    .arg(fp)
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "{name}: {}{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                println!("GLSL_VALIDATED={name}");
            }
        }
    }
}
