/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#![deny(unsafe_code)]

use std::any::Any;
use std::cell::{Cell, RefCell, RefMut};
use std::num::NonZeroU32;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use dpi::PhysicalSize;
use embedder_traits::RefreshDriver;
use euclid::default::{Rect, Size2D as UntypedSize2D};
use euclid::{Point2D, Size2D};
use gleam::gl::{self, Gl};
use glow::NativeFramebuffer;
use image::RgbaImage;
use log::{debug, trace, warn};
use raw_window_handle::{DisplayHandle, HasDisplayHandle, HasWindowHandle, WindowHandle};
pub use surfman::Error;
use surfman::chains::{PreserveBuffer, SwapChain};
use surfman::{
    Adapter, Connection, Context, ContextAttributeFlags, ContextAttributes, Device, GLApi,
    GLVersion, NativeWidget, Surface, SurfaceAccess, SurfaceInfo, SurfaceTexture, SurfaceType,
};
use webrender_api::units::{DeviceIntRect, DevicePixel};

#[path = "native_context_owner.rs"]
mod native_context_owner;
use native_context_owner::{NativeContextOwner, NativeContextState};

/// The `RenderingContext` trait defines a set of methods for managing
/// an OpenGL or GLES rendering context.
/// Implementors of this trait are responsible for handling the creation,
/// management, and destruction of the rendering context and its associated
/// resources.
pub trait RenderingContext {
    /// Prepare this [`RenderingContext`] to be rendered upon by Servo. For instance,
    /// by binding a framebuffer to the current OpenGL context.
    fn prepare_for_rendering(&self) {}
    /// Read the contents of this [`Renderingcontext`] into an in-memory image. If the
    /// image cannot be read (for instance, if no rendering has taken place yet), then
    /// `None` is returned.
    ///
    /// In a double-buffered [`RenderingContext`] this is expected to read from the back
    /// buffer. That means that once Servo renders to the context, this should return those
    /// results, even before [`RenderingContext::present`] is called.
    fn read_to_image(&self, source_rectangle: DeviceIntRect) -> Option<RgbaImage>;
    /// Get the current size of this [`RenderingContext`].
    fn size(&self) -> PhysicalSize<u32>;
    /// Get the current size of this [`RenderingContext`] as [`Size2D`].
    fn size2d(&self) -> Size2D<u32, DevicePixel> {
        let size = self.size();
        Size2D::new(size.width, size.height)
    }
    /// Resizes the rendering surface to the given size.
    fn resize(&self, size: PhysicalSize<u32>);
    /// Presents the rendered frame to the screen. In a double-buffered context, this would
    /// swap buffers.
    fn present(&self);
    /// Makes the context the current OpenGL context for this thread.
    /// After calling this function, it is valid to use OpenGL rendering
    /// commands.
    fn make_current(&self) -> Result<(), Error>;
    /// Native retirement associated with this rendering context. Embedders can
    /// keep this status after dropping their final context reference and service
    /// owner-thread cleanup until it is complete, before releasing native windows.
    fn cleanup_scope(&self) -> Option<servo_base::deferred_cleanup::CleanupScope> {
        None
    }
    /// Cached native loss state, without issuing ordinary GL queries.
    fn native_context_is_lost(&self) -> bool {
        false
    }
    /// Returns the `gleam` version of the OpenGL or GLES API.
    fn gleam_gl_api(&self) -> Rc<dyn gleam::gl::Gl>;
    /// Returns the OpenGL or GLES API.
    fn glow_gl_api(&self) -> Arc<glow::Context>;
    /// Creates a texture from a given surface and returns the surface texture,
    /// the OpenGL texture object, and its size. Failures return the original surface.
    fn create_texture(
        &self,
        surface: Surface,
    ) -> Result<(SurfaceTexture, u32, UntypedSize2D<i32>), (surfman::Error, Surface)> {
        Err((surfman::Error::Unimplemented, surface))
    }
    /// Export the surface. Failures return the original texture for retry.
    fn destroy_texture(
        &self,
        surface_texture: SurfaceTexture,
    ) -> Result<Surface, (surfman::Error, SurfaceTexture)> {
        Err((surfman::Error::Unimplemented, surface_texture))
    }
    /// The connection to the display server for WebGL. Default to `None`.
    fn connection(&self) -> Option<Connection> {
        None
    }
    /// Return the [`RefreshDriver`] for this [`RenderingContext`]. If `None` is returned,
    /// then the default timer-based [`RefreshDriver`] will be used.
    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        None
    }
}

/// A rendering context that uses the Surfman library to create and manage
/// the OpenGL context and surface. This struct provides the default implementation
/// of the `RenderingContext` trait, handling the creation, management, and destruction
/// of the rendering context and its associated resources.
///
/// The `SurfmanRenderingContext` struct encapsulates the necessary data and methods
/// to interact with the Surfman library, including creating surfaces, binding surfaces,
/// resizing surfaces, presenting rendered frames, and managing the OpenGL context state.
struct SurfmanRenderingContext {
    gleam_gl: Rc<dyn Gl>,
    glow_gl: Arc<glow::Context>,
    native: NativeContextOwner,
    refresh_driver: Option<Rc<dyn RefreshDriver>>,
    reset_monitor: surfman::recovery::ResetStatusMonitor,
}

impl Deref for SurfmanRenderingContext {
    type Target = NativeContextState;
    fn deref(&self) -> &NativeContextState {
        self.native.state()
    }
}

impl SurfmanRenderingContext {
    fn new(
        connection: &Connection,
        adapter: &Adapter,
        refresh_driver: Option<Rc<dyn RefreshDriver>>,
    ) -> Result<Rc<Self>, Error> {
        Self::new_with_owner(connection, adapter, refresh_driver, None)
    }

    fn new_with_owner(
        connection: &Connection,
        adapter: &Adapter,
        refresh_driver: Option<Rc<dyn RefreshDriver>>,
        keep_alive: Option<Rc<dyn Any>>,
    ) -> Result<Rc<Self>, Error> {
        let device = connection.create_device(adapter)?;
        let flags = ContextAttributeFlags::ALPHA
            | ContextAttributeFlags::DEPTH
            | ContextAttributeFlags::STENCIL;
        let gl_api = connection.gl_api();
        let version = match gl_api {
            GLApi::GLES => GLVersion::new(3, 0),
            GLApi::GL => GLVersion::new(3, 2),
        };
        let context = surfman::recovery::create_context_with_optional_robustness(
            &device,
            ContextAttributes { flags, version },
        )
        .inspect_err(|error| {
            warn!("Unable to create {gl_api:?} {version:?} rendering context: {error:?}");
        })?;
        let native = NativeContextOwner::new(device, context);
        if let Some(owner) = keep_alive {
            native.keep_alive(owner);
        }
        Self::finish_initialization(native, gl_api, refresh_driver, |device, context| {
            #[expect(unsafe_code)]
            unsafe {
                glow::Context::try_from_loader_function(|name| {
                    device.get_proc_address(context, name)
                })
            }
            .map_err(Error::GLContextInitializationFailed)
        })
    }

    fn finish_initialization(
        native: NativeContextOwner,
        gl_api: GLApi,
        refresh_driver: Option<Rc<dyn RefreshDriver>>,
        load: impl FnOnce(&Device, &Context) -> Result<glow::Context, Error>,
    ) -> Result<Rc<Self>, Error> {
        let reset_monitor = surfman::recovery::ResetStatusMonitor::new();
        let (glow_gl, gleam_gl) = {
            let device = native.state().device.borrow();
            let mut context = native.state().context.borrow_mut();
            device.make_context_current(&context)?;
            reset_monitor.check(&*device, &mut context)?;
            // Validate the loader before publishing either GL interface.
            let glow_gl = load(&device, &context)?;
            #[expect(unsafe_code)]
            let gleam_gl = match gl_api {
                GLApi::GL => unsafe {
                    gl::GlFns::load_with(|name| device.get_proc_address(&context, name))
                },
                GLApi::GLES => unsafe {
                    gl::GlesFns::load_with(|name| device.get_proc_address(&context, name))
                },
            };
            (glow_gl, gleam_gl)
        };
        Ok(Rc::new(Self {
            native,
            gleam_gl,
            glow_gl: Arc::new(glow_gl),
            refresh_driver,
            reset_monitor,
        }))
    }

    fn create_surface(&self, surface_type: SurfaceType<NativeWidget>) -> Result<Surface, Error> {
        let device = &mut self.device.borrow_mut();
        let context = &self.context.borrow();
        device.create_surface(context, SurfaceAccess::GPUOnly, surface_type)
    }

    fn bind_surface(self: &Rc<Self>, surface: Surface) -> Result<(), Error> {
        let result = self
            .device
            .borrow()
            .bind_surface_to_context(&mut self.context.borrow_mut(), surface);
        match result {
            Ok(()) => Ok(()),
            Err((error, surface)) => {
                let _ = self.retire_surface(surface);
                Err(error)
            },
        }
    }

    fn release_surface(&self, surface: &mut Surface) -> Result<(), Error> {
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();
        if !device.context_is_lost(&context) {
            device.make_context_current(&context)?;
        }
        device.destroy_surface(&mut context, surface)
    }

    fn retire_surface(self: &Rc<Self>, surface: Surface) -> Result<(), Error> {
        self.retire_surface_with(surface, Self::release_surface)
    }

    fn retire_surface_with(
        self: &Rc<Self>,
        mut surface: Surface,
        mut release: impl FnMut(&Self, &mut Surface) -> Result<(), Error> + 'static,
    ) -> Result<(), Error> {
        self.native.cleanup_scope().run(|| {
            if servo_base::deferred_cleanup::retention_active() {
                let owner = self.clone();
                servo_base::deferred_cleanup::defer(move || release(&owner, &mut surface).is_ok());
                return Err(Error::Failed);
            }
            match release(self, &mut surface) {
                Ok(()) => Ok(()),
                Err(error) => {
                    let owner = self.clone();
                    servo_base::deferred_cleanup::defer(move || {
                        release(&owner, &mut surface).is_ok()
                    });
                    Err(error)
                },
            }
        })
    }

    fn create_attached_swap_chain(&self) -> Result<SwapChain<Device>, Error> {
        let chain = SwapChain::<Device>::create_attached(
            &self.device.borrow(),
            &mut self.context.borrow_mut(),
            SurfaceAccess::GPUOnly,
        )?;
        self.native.track_chain(&chain);
        Ok(chain)
    }

    fn resize_bound_surface(&self, size: PhysicalSize<u32>) -> Result<(), Error> {
        if size.width == 0
            || size.height == 0
            || size.width > i32::MAX as u32
            || size.height > i32::MAX as u32
        {
            log::error!("Unable to resize to size under 1x1 ({size:?} provided)");
            return Err(Error::Failed);
        }

        let size = Size2D::new(size.width as i32, size.height as i32);
        self.device
            .borrow()
            .resize_bound_surface(&mut self.context.borrow_mut(), size)
    }

    fn present_bound_surface(&self) -> Result<(), Error> {
        self.device
            .borrow()
            .present_bound_surface(&mut self.context.borrow_mut())
    }

    fn framebuffer(&self) -> Option<NativeFramebuffer> {
        let device = &self.device.borrow();
        let context = &self.context.borrow();
        device
            .context_surface_info(context)
            .unwrap_or(None)
            .and_then(|info| info.framebuffer_object)
    }

    fn prepare_for_rendering(&self) {
        if self.native_context_is_lost() || self.make_current().is_err() {
            return;
        }
        let framebuffer_id = self
            .framebuffer()
            .map_or(0, |framebuffer| framebuffer.0.into());
        self.gleam_gl
            .bind_framebuffer(gleam::gl::FRAMEBUFFER, framebuffer_id);
    }

    fn read_to_image(&self, source_rectangle: DeviceIntRect) -> Option<RgbaImage> {
        if self.native_context_is_lost()
            || self.make_current().is_err()
            || self.native_context_is_lost()
        {
            return None;
        }
        let framebuffer_id = self
            .framebuffer()
            .map_or(0, |framebuffer| framebuffer.0.into());
        Framebuffer::read_framebuffer_to_image(&self.gleam_gl, framebuffer_id, source_rectangle)
    }

    fn make_current(&self) -> Result<(), Error> {
        self.native.cleanup_scope().run(|| {
            if servo_base::deferred_cleanup::retention_active() {
                return Err(Error::Failed);
            }
            let device = &self.device.borrow();
            let mut context = self.context.borrow_mut();
            device.make_context_current(&context)?;
            self.reset_monitor.check(&**device, &mut context)
        })
    }

    fn create_texture(
        &self,
        surface: Surface,
    ) -> Result<(SurfaceTexture, u32, UntypedSize2D<i32>), (surfman::Error, Surface)> {
        let device = &self.device.borrow();
        let context = &mut self.context.borrow_mut();
        let SurfaceInfo {
            id: front_buffer_id,
            size,
            ..
        } = device.surface_info(&surface);
        debug!("... getting texture for surface {:?}", front_buffer_id);
        let surface_texture = device.create_surface_texture(context, surface)?;
        let gl_texture = device
            .surface_texture_object(&surface_texture)
            .map(|tex| tex.0.get())
            .unwrap_or(0);
        Ok((surface_texture, gl_texture, size))
    }

    fn native_context_is_lost(&self) -> bool {
        self.device.borrow().context_is_lost(&self.context.borrow())
    }

    fn destroy_texture(
        &self,
        surface_texture: SurfaceTexture,
    ) -> Result<Surface, (surfman::Error, SurfaceTexture)> {
        let device = &self.device.borrow();
        let context = &mut self.context.borrow_mut();
        device.destroy_surface_texture(context, surface_texture)
    }

    fn connection(&self) -> Option<Connection> {
        Some(self.device.borrow().connection())
    }

    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        self.refresh_driver.clone()
    }
}

/// A software rendering context that uses a software OpenGL implementation to render
/// Servo. This will generally have bad performance, but can be used in situations where
/// it is more convenient to have consistent, but slower display output.
///
/// The results of the render can be accessed via [`RenderingContext::read_to_image`].
pub struct SoftwareRenderingContext {
    size: Cell<PhysicalSize<u32>>,
    surfman_rendering_info: Rc<SurfmanRenderingContext>,
    swap_chain: SwapChain<Device>,
}

impl SoftwareRenderingContext {
    pub fn new(size: PhysicalSize<u32>) -> Result<Self, Error> {
        if size.width == 0 || size.height == 0 {
            log::error!(
                "Unable to create SoftwareRenderingContext with size under 1x1 ({size:?} provided)"
            );
            return Err(Error::Failed);
        }

        Self::new_offscreen(size, true)
    }

    /// An offscreen surface on the preferred hardware adapter. Embedders can
    /// composite the resulting image into a toolkit-owned native window.
    pub fn new_accelerated(size: PhysicalSize<u32>) -> Result<Self, Error> {
        if size.width == 0 || size.height == 0 {
            return Err(Error::Failed);
        }
        Self::new_offscreen(size, false)
    }

    fn new_offscreen(size: PhysicalSize<u32>, software: bool) -> Result<Self, Error> {
        let connection = Connection::new()?;
        let adapter = if software {
            connection.create_software_adapter()?
        } else {
            connection.create_adapter()?
        };
        let surfman_rendering_info = SurfmanRenderingContext::new(&connection, &adapter, None)?;

        let surfman_size = Size2D::new(size.width as i32, size.height as i32);
        let surface =
            surfman_rendering_info.create_surface(SurfaceType::Generic { size: surfman_size })?;
        surfman_rendering_info.bind_surface(surface)?;
        surfman_rendering_info.make_current()?;

        let swap_chain = surfman_rendering_info.create_attached_swap_chain()?;
        Ok(SoftwareRenderingContext {
            size: Cell::new(size),
            surfman_rendering_info,
            swap_chain,
        })
    }
}

impl RenderingContext for SoftwareRenderingContext {
    fn prepare_for_rendering(&self) {
        self.surfman_rendering_info.prepare_for_rendering();
    }

    fn read_to_image(&self, source_rectangle: DeviceIntRect) -> Option<RgbaImage> {
        self.surfman_rendering_info.read_to_image(source_rectangle)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.size.get()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        let (Ok(width), Ok(height)) = (i32::try_from(size.width), i32::try_from(size.height))
        else {
            warn!("Ignoring out-of-range rendering surface size: {size:?}");
            return;
        };
        if width == 0 || height == 0 || self.size.get() == size {
            return;
        }
        let device = self.surfman_rendering_info.device.borrow();
        let mut context = self.surfman_rendering_info.context.borrow_mut();
        if let Err(error) =
            self.swap_chain
                .resize(&device, &mut context, Size2D::new(width, height))
        {
            warn!("Unable to complete rendering surface resize: {error:?}");
        }
        // A resize can commit its new buffer before old-buffer retirement fails.
        // The chain's committed dimensions remain authoritative in either case.
        let committed = self.swap_chain.size();
        self.size.set(PhysicalSize::new(
            committed.width as u32,
            committed.height as u32,
        ));
    }

    #[servo_tracing::instrument(skip_all, name = "SoftwareRenderingContext::present")]
    fn present(&self) {
        let device = &mut self.surfman_rendering_info.device.borrow_mut();
        let context = &mut self.surfman_rendering_info.context.borrow_mut();
        let _ = self
            .swap_chain
            .swap_buffers(device, context, PreserveBuffer::No);
    }

    fn make_current(&self) -> Result<(), Error> {
        self.surfman_rendering_info.make_current()
    }

    fn gleam_gl_api(&self) -> Rc<dyn gleam::gl::Gl> {
        self.surfman_rendering_info.gleam_gl.clone()
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.surfman_rendering_info.glow_gl.clone()
    }

    fn create_texture(
        &self,
        surface: Surface,
    ) -> Result<(SurfaceTexture, u32, UntypedSize2D<i32>), (surfman::Error, Surface)> {
        self.surfman_rendering_info.create_texture(surface)
    }

    fn cleanup_scope(&self) -> Option<servo_base::deferred_cleanup::CleanupScope> {
        Some(self.surfman_rendering_info.native.cleanup_scope())
    }

    fn native_context_is_lost(&self) -> bool {
        self.surfman_rendering_info.native_context_is_lost()
    }

    fn destroy_texture(
        &self,
        surface_texture: SurfaceTexture,
    ) -> Result<Surface, (surfman::Error, SurfaceTexture)> {
        self.surfman_rendering_info.destroy_texture(surface_texture)
    }

    fn connection(&self) -> Option<Connection> {
        self.surfman_rendering_info.connection()
    }
}

/// A [`RenderingContext`] that uses the `surfman` library to render to a
/// `raw-window-handle` identified window. `surfman` will attempt to create an
/// OpenGL context and surface for this window. This is a simple implementation
/// of the [`RenderingContext`] trait, but by default it paints to the entire
/// window surface.
///
/// If you would like to paint to only a portion of the window, consider using
/// [`OffscreenRenderingContext`] by calling [`WindowRenderingContext::offscreen_context`].
pub struct WindowRenderingContext {
    /// The inner size of the window in physical pixels which excludes OS decorations.
    size: Cell<PhysicalSize<u32>>,
    surfman_context: Rc<SurfmanRenderingContext>,
    presentation_epoch: Arc<AtomicU64>,
}

impl WindowRenderingContext {
    /// Construct using borrowed handles. The embedder must retain their owners
    /// through all deferred cleanup; prefer `new_owned` for an owned window.
    pub fn new(
        display_handle: DisplayHandle,
        window_handle: WindowHandle,
        size: PhysicalSize<u32>,
    ) -> Result<Self, Error> {
        Self::new_with_optional_refresh_driver(display_handle, window_handle, size, None, None)
    }

    pub fn new_with_refresh_driver(
        display_handle: DisplayHandle,
        window_handle: WindowHandle,
        size: PhysicalSize<u32>,
        refresh_driver: Rc<dyn RefreshDriver>,
    ) -> Result<Self, Error> {
        Self::new_with_optional_refresh_driver(
            display_handle,
            window_handle,
            size,
            Some(refresh_driver),
            None,
        )
    }

    /// Keep the native window (and its display connection) alive until its
    /// native context has retired, including initialization failure and retry.
    pub fn new_owned<W: HasDisplayHandle + HasWindowHandle + 'static>(
        window: Rc<W>,
        size: PhysicalSize<u32>,
    ) -> Result<Self, Error> {
        let owner: Rc<dyn Any> = window.clone();
        let display_handle = window
            .display_handle()
            .map_err(|_| Error::ConnectionRequired)?;
        let handle = window
            .window_handle()
            .map_err(|_| Error::InvalidNativeWidget)?;
        Self::new_with_optional_refresh_driver(display_handle, handle, size, None, Some(owner))
    }

    pub fn new_owned_with_refresh_driver<W: HasDisplayHandle + HasWindowHandle + 'static>(
        window: Rc<W>,
        size: PhysicalSize<u32>,
        refresh_driver: Rc<dyn RefreshDriver>,
    ) -> Result<Self, Error> {
        let owner: Rc<dyn Any> = window.clone();
        let display_handle = window
            .display_handle()
            .map_err(|_| Error::ConnectionRequired)?;
        let handle = window
            .window_handle()
            .map_err(|_| Error::InvalidNativeWidget)?;
        Self::new_with_optional_refresh_driver(
            display_handle,
            handle,
            size,
            Some(refresh_driver),
            Some(owner),
        )
    }

    fn new_with_optional_refresh_driver(
        display_handle: DisplayHandle,
        window_handle: WindowHandle,
        size: PhysicalSize<u32>,
        refresh_driver: Option<Rc<dyn RefreshDriver>>,
        keep_alive: Option<Rc<dyn Any>>,
    ) -> Result<Self, Error> {
        if size.width == 0
            || size.height == 0
            || size.width > i32::MAX as u32
            || size.height > i32::MAX as u32
        {
            log::error!("Invalid WindowRenderingContext dimensions ({size:?} provided)");
            return Err(Error::Failed);
        }

        let connection = Connection::from_display_handle(display_handle)?;
        let adapter = connection.create_adapter()?;
        let surfman_context = SurfmanRenderingContext::new_with_owner(
            &connection,
            &adapter,
            refresh_driver,
            keep_alive,
        )?;

        let native_widget = connection.create_native_widget_from_window_handle(
            window_handle,
            Size2D::new(size.width as i32, size.height as i32),
        )?;

        let surface = surfman_context.create_surface(SurfaceType::Widget { native_widget })?;
        surfman_context.bind_surface(surface)?;
        surfman_context.make_current()?;

        Ok(Self {
            size: Cell::new(size),
            surfman_context,
            presentation_epoch: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Allocate a child target. Errors leave the parent target intact.
    pub fn offscreen_context(
        self: &Rc<Self>,
        size: PhysicalSize<u32>,
    ) -> Result<OffscreenRenderingContext, Error> {
        OffscreenRenderingContext::new(self.clone(), size)
    }

    /// Stop rendering to the window that was used to create this `WindowRenderingContext`
    /// or last set with [`Self::set_window`].
    ///
    /// TODO: This should be removed once `WebView`s can replace their `RenderingContext`s.
    pub fn take_window(&self) -> Result<(), Error> {
        self.presentation_epoch.fetch_add(1, Ordering::AcqRel);
        let surface = self
            .surfman_context
            .device
            .borrow()
            .unbind_surface_from_context(&mut self.surfman_context.context.borrow_mut())?;
        if let Some(surface) = surface {
            self.surfman_context.retire_surface(surface)?;
        }
        Ok(())
    }

    /// Replace the window that this [`WindowRenderingContext`] renders to and give it a new
    /// size.
    ///
    /// TODO: This should be removed once `WebView`s can replace their `RenderingContext`s.
    pub fn set_window(
        &self,
        window_handle: WindowHandle,
        size: PhysicalSize<u32>,
    ) -> Result<(), Error> {
        self.presentation_epoch.fetch_add(1, Ordering::AcqRel);
        let connection = self.surfman_context.device.borrow().connection();
        let native_widget = connection.create_native_widget_from_window_handle(
            window_handle,
            Size2D::new(size.width as i32, size.height as i32),
        )?;
        let surface = self
            .surfman_context
            .create_surface(SurfaceType::Widget { native_widget })?;
        self.surfman_context.bind_surface(surface)?;
        self.surfman_context.make_current()?;
        Ok(())
    }

    pub fn surfman_details(&self) -> (RefMut<'_, Device>, RefMut<'_, Context>) {
        (
            self.surfman_context.device.borrow_mut(),
            self.surfman_context.context.borrow_mut(),
        )
    }
}

impl RenderingContext for WindowRenderingContext {
    fn prepare_for_rendering(&self) {
        self.surfman_context.prepare_for_rendering();
    }

    fn read_to_image(&self, source_rectangle: DeviceIntRect) -> Option<RgbaImage> {
        self.surfman_context.read_to_image(source_rectangle)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.size.get()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        self.presentation_epoch.fetch_add(1, Ordering::AcqRel);
        match self.surfman_context.resize_bound_surface(size) {
            Ok(..) => self.size.set(size),
            Err(error) => warn!("Error resizing surface: {error:?}"),
        }
    }

    #[servo_tracing::instrument(skip_all, name = "WindowRenderingContext::present")]
    fn present(&self) {
        if let Err(error) = self.surfman_context.present_bound_surface() {
            warn!("Error presenting surface: {error:?}");
        }
    }

    fn make_current(&self) -> Result<(), Error> {
        self.surfman_context.make_current()
    }

    fn gleam_gl_api(&self) -> Rc<dyn gleam::gl::Gl> {
        self.surfman_context.gleam_gl.clone()
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.surfman_context.glow_gl.clone()
    }

    fn create_texture(
        &self,
        surface: Surface,
    ) -> Result<(SurfaceTexture, u32, UntypedSize2D<i32>), (surfman::Error, Surface)> {
        self.surfman_context.create_texture(surface)
    }

    fn cleanup_scope(&self) -> Option<servo_base::deferred_cleanup::CleanupScope> {
        Some(self.surfman_context.native.cleanup_scope())
    }

    fn native_context_is_lost(&self) -> bool {
        self.surfman_context.native_context_is_lost()
    }

    fn destroy_texture(
        &self,
        surface_texture: SurfaceTexture,
    ) -> Result<Surface, (surfman::Error, SurfaceTexture)> {
        self.surfman_context.destroy_texture(surface_texture)
    }

    fn connection(&self) -> Option<Connection> {
        self.surfman_context.connection()
    }

    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        self.surfman_context.refresh_driver()
    }
}

#[path = "offscreen_framebuffer.rs"]
mod offscreen_framebuffer;
use offscreen_framebuffer::Framebuffer;

pub struct OffscreenRenderingContext {
    parent_context: Rc<WindowRenderingContext>,
    size: Cell<PhysicalSize<u32>>,
    framebuffer: RefCell<Framebuffer>,
}

type RenderToParentCallback = Box<dyn Fn(&glow::Context, Rect<i32>) + Send + Sync>;

impl OffscreenRenderingContext {
    fn new(
        parent_context: Rc<WindowRenderingContext>,
        size: PhysicalSize<u32>,
    ) -> Result<Self, Error> {
        let framebuffer = RefCell::new(Framebuffer::new(parent_context.clone(), size)?);
        Ok(Self {
            parent_context,
            size: Cell::new(size),
            framebuffer,
        })
    }

    pub fn parent_context(&self) -> &WindowRenderingContext {
        &self.parent_context
    }

    pub fn render_to_parent_callback(&self) -> Option<RenderToParentCallback> {
        let framebuffer = self.framebuffer.borrow();
        let source = NonZeroU32::new(framebuffer.framebuffer_id).map(NativeFramebuffer)?;
        let target = self.parent_context.surfman_context.framebuffer();
        let lease = framebuffer.lease.clone();
        let epoch = self.parent_context.presentation_epoch.clone();
        let expected_epoch = epoch.load(Ordering::Acquire);
        let size = self.size.get();
        Some(Box::new(move |_gl, target_rect| {
            let active = lease
                .active
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !*active || epoch.load(Ordering::Acquire) != expected_epoch {
                return;
            }
            let Some(owner) = lease.context() else {
                return;
            };
            Self::blit_framebuffer(
                &owner.glow_gl_api(),
                Rect::new(
                    Point2D::origin(),
                    Size2D::new(size.width as i32, size.height as i32),
                ),
                source,
                target_rect,
                target,
            );
        }))
    }

    #[expect(unsafe_code)]
    fn blit_framebuffer(
        gl: &glow::Context,
        source_rect: Rect<i32>,
        source_framebuffer_id: NativeFramebuffer,
        target_rect: Rect<i32>,
        target_framebuffer_id: Option<NativeFramebuffer>,
    ) {
        use glow::HasContext as _;
        if source_rect.width() <= 0
            || source_rect.height() <= 0
            || target_rect.width() <= 0
            || target_rect.height() <= 0
            || target_rect
                .origin
                .x
                .checked_add(target_rect.width())
                .is_none()
            || target_rect
                .origin
                .y
                .checked_add(target_rect.height())
                .is_none()
        {
            return;
        }
        unsafe {
            let draw = gl.get_parameter_i32(gl::DRAW_FRAMEBUFFER_BINDING) as u32;
            let read = gl.get_parameter_i32(gl::READ_FRAMEBUFFER_BINDING) as u32;
            let scissor_enabled = gl.is_enabled(gl::SCISSOR_TEST);
            let mut scissor = [0; 4];
            gl.get_parameter_i32_slice(gl::SCISSOR_BOX, &mut scissor);
            let mut clear = [0.; 4];
            gl.get_parameter_f32_slice(gl::COLOR_CLEAR_VALUE, &mut clear);
            let mut mask = [0; 4];
            gl.get_parameter_i32_slice(gl::COLOR_WRITEMASK, &mut mask);
            gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, target_framebuffer_id);
            gl.color_mask(true, true, true, true);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.scissor(
                target_rect.origin.x,
                target_rect.origin.y,
                target_rect.width(),
                target_rect.height(),
            );
            gl.enable(gl::SCISSOR_TEST);
            gl.clear(gl::COLOR_BUFFER_BIT);
            gl.disable(gl::SCISSOR_TEST);

            gl.bind_framebuffer(gl::READ_FRAMEBUFFER, Some(source_framebuffer_id));
            gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, target_framebuffer_id);

            gl.blit_framebuffer(
                source_rect.origin.x,
                source_rect.origin.y,
                source_rect.origin.x + source_rect.width(),
                source_rect.origin.y + source_rect.height(),
                target_rect.origin.x,
                target_rect.origin.y,
                target_rect.origin.x + target_rect.width(),
                target_rect.origin.y + target_rect.height(),
                gl::COLOR_BUFFER_BIT,
                gl::NEAREST,
            );
            gl.bind_framebuffer(
                gl::READ_FRAMEBUFFER,
                NonZeroU32::new(read).map(NativeFramebuffer),
            );
            gl.bind_framebuffer(
                gl::DRAW_FRAMEBUFFER,
                NonZeroU32::new(draw).map(NativeFramebuffer),
            );
            gl.scissor(scissor[0], scissor[1], scissor[2], scissor[3]);
            if scissor_enabled {
                gl.enable(gl::SCISSOR_TEST);
            } else {
                gl.disable(gl::SCISSOR_TEST);
            }
            gl.clear_color(clear[0], clear[1], clear[2], clear[3]);
            gl.color_mask(mask[0] != 0, mask[1] != 0, mask[2] != 0, mask[3] != 0);
        }
    }
}

impl RenderingContext for OffscreenRenderingContext {
    fn size(&self) -> PhysicalSize<u32> {
        self.size.get()
    }

    fn resize(&self, new_size: PhysicalSize<u32>) {
        let old_size = self.size.get();
        if old_size == new_size {
            return;
        }
        let new_framebuffer = match Framebuffer::new(self.parent_context.clone(), new_size) {
            Ok(framebuffer) => framebuffer,
            Err(error) => {
                warn!("Unable to resize offscreen target: {error:?}");
                return;
            },
        };
        let old_framebuffer = self.framebuffer.borrow();
        let copy = new_size.min(old_size);
        let rect = Rect::new(
            Point2D::origin(),
            Size2D::new(copy.width as i32, copy.height as i32),
        );
        Self::blit_framebuffer(
            &self.glow_gl_api(),
            rect,
            NativeFramebuffer(
                NonZeroU32::new(old_framebuffer.framebuffer_id).expect("validated framebuffer"),
            ),
            rect,
            NonZeroU32::new(new_framebuffer.framebuffer_id).map(NativeFramebuffer),
        );
        drop(old_framebuffer);
        *self.framebuffer.borrow_mut() = new_framebuffer;
        self.size.set(new_size);
    }

    fn prepare_for_rendering(&self) {
        if let Err(error) = self.framebuffer.borrow().bind() {
            warn!("Unable to bind offscreen target: {error:?}");
        }
    }

    fn present(&self) {}

    fn make_current(&self) -> Result<(), surfman::Error> {
        self.parent_context.make_current()
    }

    fn gleam_gl_api(&self) -> Rc<dyn gleam::gl::Gl> {
        self.parent_context.gleam_gl_api()
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.parent_context.glow_gl_api()
    }

    fn create_texture(
        &self,
        surface: Surface,
    ) -> Result<(SurfaceTexture, u32, UntypedSize2D<i32>), (surfman::Error, Surface)> {
        self.parent_context.create_texture(surface)
    }

    fn cleanup_scope(&self) -> Option<servo_base::deferred_cleanup::CleanupScope> {
        self.parent_context.cleanup_scope()
    }

    fn native_context_is_lost(&self) -> bool {
        self.parent_context.native_context_is_lost()
    }

    fn destroy_texture(
        &self,
        surface_texture: SurfaceTexture,
    ) -> Result<Surface, (surfman::Error, SurfaceTexture)> {
        self.parent_context.destroy_texture(surface_texture)
    }

    fn connection(&self) -> Option<Connection> {
        self.parent_context.connection()
    }

    fn read_to_image(&self, source_rectangle: DeviceIntRect) -> Option<RgbaImage> {
        self.framebuffer.borrow().read_to_image(source_rectangle)
    }

    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        self.parent_context().refresh_driver()
    }
}

#[cfg(test)]
mod test {
    use dpi::PhysicalSize;
    use euclid::{Box2D, Point2D, Size2D};
    use gleam::gl;
    use image::Rgba;
    use surfman::Error;

    use super::Framebuffer;
    use crate::rendering_context::SoftwareRenderingContext;

    #[test]
    fn test_read_pixels() -> Result<(), Error> {
        use std::rc::Rc;
        const SIZE: u32 = 16;
        let context = Rc::new(SoftwareRenderingContext::new(PhysicalSize::new(
            SIZE, SIZE,
        ))?);
        let framebuffer = Framebuffer::new(context, PhysicalSize::new(SIZE, SIZE))?;
        framebuffer.bind()?;
        framebuffer
            .gl
            .clear_color(12.0 / 255.0, 34.0 / 255.0, 56.0 / 255.0, 78.0 / 255.0);
        framebuffer.gl.clear(gl::COLOR_BUFFER_BIT);
        let rect = Box2D::from_origin_and_size(Point2D::zero(), Size2D::new(SIZE, SIZE));
        let img = framebuffer
            .read_to_image(rect.to_i32())
            .expect("Should read back image");
        assert_eq!((img.width(), img.height()), (SIZE, SIZE));
        assert!(img.pixels().all(|pixel| *pixel == Rgba([12, 34, 56, 78])));
        Ok(())
    }

    #[test]
    fn test_minimum_size_error() {
        let result = SoftwareRenderingContext::new(PhysicalSize {
            width: 0,
            height: 1,
        });
        match result {
            Err(surfman::Error::Failed) => (),
            _ => panic!("Expected {:?}", surfman::Error::Failed),
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "native_context_owner_tests.rs"]
mod native_context_owner_tests;
