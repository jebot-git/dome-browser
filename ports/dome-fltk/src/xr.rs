// SPDX-License-Identifier: GPL-3.0-or-later
//! WebXR device backed by DomeD protocol v1. This never opens an OpenXR runtime.
use euclid::{Point2D, Rect, RigidTransform3D, Rotation3D, Size2D, Vector3D};
use glow::HasContext;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    io::{Read, Write},
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};
use surfman::chains::{SwapChainAPI, SwapChains, SwapChainsAPI};
use surfman::{
    Adapter, Connection, Context, ContextAttributeFlags, ContextAttributes, Device, GLApi,
    GLVersion, SurfaceAccess, SurfaceType,
};
use webxr::{SurfmanGL, SurfmanLayerManager};
use webxr_api::util::{ClipPlanes, fov_to_projection_matrix};
use webxr_api::{
    ContextId, DeviceAPI, DiscoveryAPI, Error, Event, EventBuffer, Floor, Frame, InputSource,
    LayerGrandManager, LayerId, LayerInfo, LayerInit, LayerManager, Native, Quitter, Session, SessionBuilder,
    SessionInit, SessionMode, View, ViewerPose, Viewports, Views, Visibility,
};
mod input;
mod reference_space;
mod inline;
mod time;

#[derive(Clone, Copy, Default)]
struct PageFocus {
    owner: Option<servo::WebViewId>,
    focused: bool,
    revision: u64,
}

/// Tracks trusted tab/chrome focus independently of the compositor's PID focus.
#[derive(Clone, Default)]
pub(crate) struct Focus(Rc<std::cell::Cell<PageFocus>>);
impl Focus {
    pub fn set(&self, owner: servo::WebViewId, focused: bool) {
        let last = self.0.get();
        if last.owner != Some(owner) || last.focused != focused {
            self.0.set(PageFocus {
                owner: Some(owner),
                focused,
                revision: last.revision.wrapping_add(1),
            });
        }
    }
    fn permits(&self, owner: servo::WebViewId) -> bool {
        let current = self.0.get();
        current.focused
            && current.owner == Some(owner)
            && unsafe { crate::ui::browser_ui_page_focused() != 0 }
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn endpoint() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DOME_WEBXR_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let runtime =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is not set")?);
    let mut port = std::env::var("DOME_CONTROL_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(49640);
    if let Some(display) = std::env::var_os("WAYLAND_DISPLAY") {
        let display = PathBuf::from(display);
        let display = if display.is_absolute() {
            display
        } else {
            runtime.join(display)
        };
        let record = PathBuf::from(format!("{}.dome-session.json", display.display()));
        if let Ok(metadata) = std::fs::symlink_metadata(&record) {
            if metadata.is_file()
                && metadata.uid() == unsafe { libc::getuid() }
                && metadata.mode() & 0o022 == 0
                && metadata.len() <= 4096
            {
                if let Ok(record) = serde_json::from_slice::<Value>(&std::fs::read(record)?) {
                    if record["version"] == 1 {
                        if let Some(value) = record["control_port"]
                            .as_u64()
                            .and_then(|p| u16::try_from(p).ok())
                            .filter(|p| *p > 0)
                        {
                            port = value;
                        }
                    }
                }
            }
        }
    }
    Ok(runtime.join(format!("dome-webxr-{port}.sock")))
}
fn rpc(path: &std::path::Path, request: Value, rgba: &[u8]) -> Result<Value> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut peer = unsafe { std::mem::zeroed::<libc::ucred>() };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    use std::os::fd::AsRawFd;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut peer as *mut _ as *mut c_void,
            &mut size,
        )
    } != 0
        || peer.uid != unsafe { libc::getuid() }
    {
        return Err("XR bridge is not owned by this user".into());
    }
    let bytes = serde_json::to_vec(&request)?;
    if bytes.len() > 65536 {
        return Err("XR request too large".into());
    }
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.write_all(rgba)?;
    let mut size = [0; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    if size == 0 || size > 65536 {
        return Err("Invalid XR response length".into());
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    if value["ok"] != true {
        return Err(value["error"]
            .as_str()
            .unwrap_or("XR bridge rejected request")
            .to_owned()
            .into());
    }
    Ok(value)
}
#[derive(Clone, Deserialize)]
struct Hello {
    version: u32,
    width: u32,
    height: u32,
    view_sizes: [[u32; 2]; 2],
    max_width: u32,
    max_height: u32,
    stage: bool,
    #[serde(default)]
    input_stream_version: u32,
}
impl Hello {
    fn parse(value: Value) -> Result<Self> {
        let result: Self = serde_json::from_value(value)?;
        if result.version != 1
            || result.width == 0
            || result.height == 0
            || result.width > result.max_width.min(4096)
            || result.height > result.max_height.min(4096)
            || result.view_sizes.iter().any(|s| s[0] == 0 || s[1] == 0)
            || result.view_sizes[0][0].checked_add(result.view_sizes[1][0]) != Some(result.width)
            || result.view_sizes.iter().any(|s| s[1] != result.height)
        {
            return Err("Invalid XR view dimensions".into());
        }
        Ok(result)
    }
    fn viewports(&self) -> Viewports {
        Viewports {
            viewports: vec![
                Rect::new(
                    Point2D::new(0, 0),
                    Size2D::new(self.view_sizes[0][0] as i32, self.height as i32),
                ),
                Rect::new(
                    Point2D::new(self.view_sizes[0][0] as i32, 0),
                    Size2D::new(self.view_sizes[1][0] as i32, self.height as i32),
                ),
            ],
        }
    }
}
pub fn probe() -> Result<Value> {
    let path = endpoint()?;
    let value = rpc(&path, json!({"op":"hello","version":1}), &[])?;
    Hello::parse(value.clone())?;
    Ok(value)
}
#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
struct Pose {
    position: [f32; 3],
    orientation: [f32; 4],
}
impl Pose {
    fn valid(&self) -> bool {
        self.position
            .iter()
            .chain(self.orientation.iter())
            .all(|x| x.is_finite())
            && (self.orientation.iter().map(|x| x * x).sum::<f32>() - 1.).abs() < 0.02
    }
    fn transform<A, B>(&self) -> RigidTransform3D<f32, A, B> {
        RigidTransform3D::new(
            Rotation3D::quaternion(
                self.orientation[0],
                self.orientation[1],
                self.orientation[2],
                self.orientation[3],
            )
            .normalize(),
            Vector3D::new(self.position[0], self.position[1], self.position[2]),
        )
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Fov {
    left: f32,
    right: f32,
    up: f32,
    down: f32,
}
impl Fov {
    fn valid(&self) -> bool {
        [self.left, self.right, self.up, self.down]
            .iter()
            .all(|x| x.is_finite() && x.abs() < 1.57)
            && self.left < self.right
            && self.down < self.up
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Eye {
    pose: Pose,
    fov: Fov,
}
#[derive(Clone, Deserialize)]
struct HostFrame {
    frame_id: u64,
    time: i64,
    #[serde(default)]
    monotonic_time: Option<i64>,
    should_render: bool,
    visible: bool,
    focused: bool,
    head_flags: u64,
    view_flags: u64,
    head: Pose,
    views: [Eye; 2],
    #[serde(default)]
    inputs: Vec<input::HostInput>,
    #[serde(default)]
    input_batch: Option<input::Batch>,
    #[serde(default)]
    reference_space: Option<reference_space::State>,
}
impl HostFrame {
    fn input_allowed(&self) -> bool {
        self.visible && self.focused && self.head_flags & 3 == 3 && self.view_flags & 3 == 3
    }
    fn web_frame(
        &self,
        anchor: RigidTransform3D<f32, Native, Native>,
        clip: ClipPlanes,
        clock: &time::Clock,
    ) -> Frame {
        let local = anchor.inverse();
        let pose = if self.visible && self.head_flags & 3 == 3 && self.view_flags & 3 == 3 {
            let view = |v: &Eye| View::<webxr_api::Viewer> {
                transform: v.pose.transform::<webxr_api::Viewer, Native>().then(&local),
                projection: fov_to_projection_matrix(
                    v.fov.left,
                    v.fov.right,
                    v.fov.up,
                    v.fov.down,
                    clip,
                ),
            };
            Some(ViewerPose {
                transform: self
                    .head
                    .transform::<webxr_api::Viewer, Native>()
                    .then(&local),
                views: Views::Stereo(
                    view(&self.views[0]).cast_unit(),
                    view(&self.views[1]).cast_unit(),
                ),
            })
        } else {
            None
        };
        Frame {
            pose,
            inputs: input::frames(self, anchor),
            events: vec![],
            sub_images: vec![],
            hit_test_results: vec![],
            predicted_display_time: self.time as f64 / 1_000_000.,
            predicted_display_instant: clock.instant(self.time, self.monotonic_time),
        }
    }
    fn valid(&self) -> bool {
        self.reference_space.is_none_or(|state| state.valid())
            && self.frame_id > 0
            && self.time > 0
            && self.monotonic_time.is_none_or(|value| value > 0)
            && (!self.focused || self.visible)
            && (self.head_flags & 3 != 3 || self.head.valid())
            && self
                .views
                .iter()
                .all(|v| (self.view_flags & 3 != 3 || v.pose.valid()) && v.fov.valid())
            && input::valid_inputs(&self.inputs)
    }
}
fn backend(error: impl std::fmt::Debug) -> Error {
    Error::BackendSpecific(format!("{error:?}"))
}
fn anchor(head: &Pose) -> RigidTransform3D<f32, Native, Native> {
    let q = head.orientation;
    let yaw = (2. * (q[3] * q[1] + q[0] * q[2])).atan2(1. - 2. * (q[1] * q[1] + q[2] * q[2]));
    RigidTransform3D::new(
        Rotation3D::around_y(euclid::Angle::radians(yaw)),
        Vector3D::new(head.position[0], head.position[1], head.position[2]),
    )
}
struct PendingLease {
    path: PathBuf,
    token: String,
    armed: bool,
}
impl Drop for PendingLease {
    fn drop(&mut self) {
        if self.armed {
            let _ = rpc(
                &self.path,
                json!({"op":"end_session","token":self.token}),
                &[],
            );
        }
    }
}
pub struct Registry {
    connection: Option<Connection>,
    focus: Focus,
}
impl Registry {
    pub fn new(connection: Option<Connection>, focus: Focus) -> Self {
        Self { connection, focus }
    }
}
impl servo::webxr::WebXrRegistry for Registry {
    fn register(&self, registry: &mut servo::webxr::MainThreadRegistry) {
        registry.register(inline::Discovery);
        if let Some(connection) = self.connection.clone() {
            registry.register(Discovery {
                connection,
                focus: self.focus.clone(),
            });
        }
    }
}
struct Discovery {
    connection: Connection,
    focus: Focus,
}
impl DiscoveryAPI<SurfmanGL> for Discovery {
    fn check_context_compatibility(&self, context: ContextId, manager: &LayerGrandManager<SurfmanGL>) -> std::result::Result<(), Error> {
        let adapter = self.connection.create_adapter().map_err(backend)?;
        webxr::compatibility::check(manager, context, self.connection.clone(), adapter)
    }

    fn supports_session(&self, mode: SessionMode) -> bool {
        mode == SessionMode::ImmersiveVR && probe().is_ok()
    }
    fn request_session(
        &mut self,
        mode: SessionMode,
        init: &SessionInit,
        builder: SessionBuilder<SurfmanGL>,
    ) -> std::result::Result<Session, Error> {
        if mode != SessionMode::ImmersiveVR {
            return Err(Error::NoMatchingDevice);
        }
        let owner = init
            .webview_id
            .filter(|owner| self.focus.permits(*owner))
            .ok_or_else(|| backend("WebXR requires the focused browser page"))?;
        let path = endpoint().map_err(backend)?;
        let hello =
            Hello::parse(rpc(&path, json!({"op":"hello","version":1}), &[]).map_err(backend)?)
                .map_err(backend)?;
        let features = init.validate(
            mode,
            &if hello.stage {
                vec!["local-floor".into()]
            } else {
                vec![]
            },
        )?;
        if init.origin == "null"
            || !crate::ui::ask(
                "Enter immersive WebXR",
                &format!(
                    "{}\n\nAllow this site to use headset tracking and display immersive content in DomeD's XR session?\n\nLeaving the page ends only this browser session.",
                    init.origin
                ),
            )
        {
            return Err(backend("WebXR permission denied"));
        }
        // The accepted native dialog temporarily owns keyboard focus. Its
        // teardown may restore page focus on the next event-loop iteration.
        // Keep the trusted requesting tab check here; frame input remains
        // independently gated by live native/page focus below.
        if self.focus.0.get().owner != Some(owner) {
            return Err(backend("The requesting WebXR tab is no longer active"));
        }
        let focus = self.focus.clone();
        let connection = self.connection.clone();
        let adapter = connection.create_adapter().map_err(backend)?;
        builder.run_on_main_thread(move |grand| {
            BridgeDevice::new(
                path, hello, connection, adapter, features, grand, focus, owner,
            )
        })
    }
}
struct BridgeDevice {
    path: PathBuf,
    hello: Hello,
    token: String,
    device: Device,
    context: Context,
    gl: Rc<glow::Context>,
    fbo: glow::NativeFramebuffer,
    chains: SwapChains<LayerId, Device>,
    grand: LayerGrandManager<SurfmanGL>,
    manager: Option<LayerManager>,
    layer_info: std::collections::HashMap<LayerId, LayerInfo>,
    submitted_viewports: std::collections::HashMap<LayerId, Viewports>,
    frame: Option<HostFrame>,
    anchor: Option<RigidTransform3D<f32, Native, Native>>,
    clip: ClipPlanes,
    events: EventBuffer,
    features: Vec<String>,
    quitter: Option<Quitter>,
    ended: bool,
    visible: Option<(bool, bool)>,
    inputs: Option<input::State>,
    focus: Focus,
    owner: servo::WebViewId,
    focus_revision: u64,
    clock: time::Clock,
    reference_spaces: reference_space::Tracker,
}
impl BridgeDevice {
    fn new(
        path: PathBuf,
        hello: Hello,
        connection: Connection,
        adapter: Adapter,
        features: Vec<String>,
        grand: LayerGrandManager<SurfmanGL>,
        focus: Focus,
        owner: servo::WebViewId,
    ) -> std::result::Result<Self, Error> {
        // A rejected or busy lease must not leave a live Surfman context behind.
        let begin = rpc(&path, json!({"op":"begin_session"}), &[]).map_err(backend)?;
        let token = begin["token"]
            .as_str()
            .filter(|t| !t.is_empty() && t.len() < 256)
            .ok_or_else(|| backend("Invalid XR lease"))?
            .to_owned();
        let mut lease = PendingLease {
            path: path.clone(),
            token: token.clone(),
            armed: true,
        };
        let baseline_frame = begin["frame_id"].as_u64().unwrap_or(0);
        let mut request = json!({"op":"wait_frame","token":token,"after":baseline_frame});
        if hello.input_stream_version == 1 {
            request["input_after"] = json!(0);
        }
        let initial: HostFrame =
            serde_json::from_value(rpc(&path, request, &[]).map_err(backend)?).map_err(backend)?;
        if !initial.valid() || initial.head_flags & 3 != 3 || initial.view_flags & 3 != 3 {
            return Err(backend("Headset tracking is not currently valid"));
        }
        let initial_anchor = anchor(&initial.head);
        let mut clock = time::Clock::default();
        clock.observe(initial.time, initial.monotonic_time);
        let inputs = if hello.input_stream_version == 1 {
            Some(input::State::new(&initial).map_err(backend)?)
        } else {
            None
        };
        let device = connection.create_device(&adapter).map_err(backend)?;
        let version = if device.gl_api() == GLApi::GLES {
            GLVersion::new(3, 0)
        } else {
            GLVersion::new(3, 2)
        };
        let desc = device
            .create_context_descriptor(&ContextAttributes {
                version,
                flags: ContextAttributeFlags::ALPHA
                    | ContextAttributeFlags::DEPTH
                    | ContextAttributeFlags::STENCIL,
            })
            .map_err(backend)?;
        let mut context = device.create_context(&desc, None).map_err(backend)?;
        let surface = device
            .create_surface(
                &context,
                SurfaceAccess::GPUOnly,
                SurfaceType::Generic {
                    size: Size2D::new(1, 1),
                },
            )
            .map_err(backend)?;
        device
            .bind_surface_to_context(&mut context, surface)
            .map_err(|(e, _)| backend(e))?;
        device.make_context_current(&context).map_err(backend)?;
        let gl = Rc::new(unsafe {
            glow::Context::from_loader_function(|symbol| device.get_proc_address(&context, symbol))
        });
        let fbo = unsafe { gl.create_framebuffer().map_err(backend)? };
        lease.armed = false;
        eprintln!("WEBXR session started through DomeD");
        Ok(Self {
            path,
            hello,
            token,
            device,
            context,
            gl,
            fbo,
            chains: SwapChains::new(),
            grand,
            manager: None,
            layer_info: std::collections::HashMap::new(),
            submitted_viewports: std::collections::HashMap::new(),
            reference_spaces: reference_space::Tracker::new(initial.reference_space),
            frame: Some(initial),
            anchor: Some(initial_anchor),
            clip: ClipPlanes::default(),
            events: EventBuffer::default(),
            features,
            quitter: None,
            ended: false,
            visible: None,
            inputs,
            focus_revision: focus.0.get().revision,
            focus,
            owner,
            clock,
        })
    }
    fn manager(&mut self) -> std::result::Result<&mut LayerManager, Error> {
        if self.manager.is_none() {
            let chains = self.chains.clone();
            let limit = Size2D::new(self.hello.max_width.min(4096) as i32, self.hello.max_height.min(4096) as i32);
            self.manager =
                Some(self.grand.create_layer_manager(move |_| {
                    Ok(SurfmanLayerManager::new(chains).with_framebuffer_limit(limit))
                })?);
        }
        Ok(self.manager.as_mut().unwrap())
    }
    fn release(&mut self) {
        if !self.ended {
            self.ended = true;
            let _ = rpc(
                &self.path,
                json!({"op":"end_session","token":self.token}),
                &[],
            );
            if let (Some(inputs), Some(frame), Some(anchor)) =
                (&mut self.inputs, &self.frame, self.anchor)
            {
                for event in inputs.cancel(&frame.web_frame(anchor, self.clip, &self.clock)) {
                    self.events.callback(event);
                }
            }
            self.events.callback(Event::SessionEnd);
            eprintln!("WEBXR browser lease ended; DomeD session preserved");
        }
    }
    fn fail(&mut self, error: impl std::fmt::Display) {
        eprintln!("WEBXR {error}");
        self.release();
        if let Some(q) = self.quitter.as_ref() {
            q.quit();
        }
    }
    fn submit(&mut self, layers: &[(ContextId, LayerId)]) -> Result<()> {
        self.manager()
            .map_err(|e| format!("{e:?}"))?
            .end_frame(layers)
            .map_err(|e| format!("{e:?}"))?;
        let Some(frame) = self.frame.clone() else {
            return Ok(());
        };
        if layers.is_empty()
            || !frame.should_render
            || !frame.visible
            || frame.head_flags & 3 != 3
            || frame.view_flags & 3 != 3
        {
            rpc(
                &self.path,
                json!({"op":"end_frame","token":self.token,"frame_id":frame.frame_id,"empty":true}),
                &[],
            )?;
            return Ok(());
        }
        let layer = layers[0].1;
        let chain = self.chains.get(layer).ok_or("XR layer has no swapchain")?;
        let surface = chain
            .take_surface()
            .ok_or("XR layer has no rendered surface")?;
        self.device
            .make_context_current(&self.context)
            .map_err(|e| format!("{e:?}"))?;
        let size = self.device.surface_info(&surface).size;
        let texture = self
            .device
            .create_surface_texture(&mut self.context, surface)
            .map_err(|(e, _)| format!("{e:?}"))?;
        let object = self.device.surface_texture_object(&texture);
        let target = self.device.surface_gl_texture_target();
        let mut pixels = vec![0u8; size.width as usize * size.height as usize * 4];
        let valid = unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            self.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                target,
                object,
                0,
            );
            let valid =
                self.gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;
            if valid {
                self.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                self.gl.read_pixels(
                    0,
                    0,
                    size.width,
                    size.height,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut pixels)),
                );
            }
            self.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                target,
                None,
                0,
            );
            valid
        };
        let surface = self
            .device
            .destroy_surface_texture(&mut self.context, texture)
            .map_err(|(e, _)| format!("{e:?}"))?;
        chain.recycle_surface(surface);
        if !valid {
            return Err("XR framebuffer is incomplete".into());
        }
        // OpenGL rows start at the bottom. DomeD's RGBA transport starts at the top.
        let stride = size.width as usize * 4;
        for y in 0..size.height as usize / 2 {
            let (a, b) = pixels.split_at_mut((size.height as usize - y - 1) * stride);
            a[y * stride..(y + 1) * stride].swap_with_slice(&mut b[..stride]);
        }
        let width = size.width as u32;
        let height = size.height as u32;
        let info = self.layer_info.get(&layer).ok_or("XR layer has no allocation information")?;
        let geometry = &info.geometry;
        if geometry.framebuffer_size.width != size.width || geometry.framebuffer_size.height != size.height {
            return Err("XR submitted surface differs from its allocation geometry".into());
        }
        let views: Vec<_> = frame.views.iter().zip(&self.submitted_viewports.get(&layer).unwrap_or(&geometry.viewports).viewports).map(|(view, rect)| json!({
            "rect":{"x":rect.origin.x,"y":size.height-rect.max_y(),"width":rect.size.width,"height":rect.size.height},
            "pose":view.pose,"fov":view.fov,
        })).collect();
        rpc(
            &self.path,
            json!({"op":"end_frame","token":self.token,"frame_id":frame.frame_id,"width":width,"height":height,"transport":"rgba","layer_flags":if info.attributes.alpha { 2 } else { 0 },"bytes":pixels.len(),"views":views}),
            &pixels,
        )?;
        Ok(())
    }
}
impl DeviceAPI for BridgeDevice {
    fn supports_dynamic_viewport_scaling(&self) -> bool { true }

    fn set_layer_viewports(&mut self, _context: ContextId, layer: LayerId, viewports: Viewports) {
        let Some(info) = self.layer_info.get(&layer) else { return; };
        if viewports.viewports.len() == info.geometry.viewports.viewports.len() &&
            viewports.viewports.iter().zip(&info.geometry.viewports.viewports).all(|(scaled, full)|
                scaled.size.width > 0 && scaled.size.height > 0 && full.contains_rect(scaled)) {
            self.submitted_viewports.insert(layer, viewports);
        }
    }

    fn create_layer(
        &mut self,
        context: ContextId,
        init: LayerInit,
    ) -> std::result::Result<LayerInfo, Error> {
        let viewports = self.hello.viewports();
        let info = self.manager()?.create_layer(context, init, viewports)?;
        self.layer_info.insert(info.id, info.clone());
        Ok(info)
    }
    fn destroy_layer(&mut self, context: ContextId, layer: LayerId) {
        self.layer_info.remove(&layer);
        self.submitted_viewports.remove(&layer);
        if let Ok(manager) = self.manager() {
            manager.destroy_layer(context, layer);
        }
    }
    fn viewports(&self) -> Viewports {
        self.hello.viewports()
    }
    fn floor_transform(&self) -> Option<RigidTransform3D<f32, Native, Floor>> {
        if !self.hello.stage {
            return None;
        }
        self.anchor.map(|anchor| {
            RigidTransform3D::from_translation(Vector3D::new(0., anchor.translation.y, 0.))
        })
    }
    fn begin_animation_frame(&mut self, layers: &[(ContextId, LayerId)]) -> Option<Frame> {
        if self.ended {
            return None;
        }
        let after = self.frame.as_ref().map_or(0, |f| f.frame_id);
        let mut request = json!({"op":"wait_frame","token":self.token,"after":after});
        if let Some(inputs) = &self.inputs {
            request["input_after"] = json!(inputs.cursor());
        }
        let value = match rpc(&self.path, request, &[]) {
            Ok(v) => v,
            Err(e) => {
                self.fail(e);
                return None;
            },
        };
        let mut frame: HostFrame = match serde_json::from_value(value) {
            Ok(f) => f,
            Err(e) => {
                self.fail(e);
                return None;
            },
        };
        if !frame.valid() || frame.frame_id <= after {
            self.fail("Invalid or repeated tracking frame");
            return None;
        }
        self.clock.observe(frame.time, frame.monotonic_time);
        let page_allowed = self.focus.permits(self.owner);
        frame.visible &= self.focus.0.get().owner == Some(self.owner);
        let focus_changed = self.focus.0.get().revision != self.focus_revision;
        self.focus_revision = self.focus.0.get().revision;
        frame.focused &= page_allowed;
        let visibility = (frame.visible, frame.focused);
        if self.visible != Some(visibility) {
            self.visible = Some(visibility);
            self.events
                .callback(Event::VisibilityChange(if !frame.visible {
                    Visibility::Hidden
                } else if frame.focused {
                    Visibility::Visible
                } else {
                    Visibility::VisibleBlurred
                }));
        }
        let anchor = self.anchor.unwrap_or_else(|| anchor(&frame.head));
        let reset_events = match self.reference_spaces.frame_events(frame.reference_space, anchor, self.hello.stage) {
            Ok(events) => events,
            Err(error) => { self.fail(error); return None; },
        };
        let origin_changed = !reset_events.is_empty();
        if let Some(inputs) = &mut self.inputs {
            match inputs.update(
                &frame,
                anchor,
                self.clip,
                &self.clock,
                !page_allowed || focus_changed || origin_changed,
            ) {
                Ok(events) => {
                    for event in events {
                        self.events.callback(event);
                    }
                },
                Err(error) => {
                    self.fail(error);
                    return None;
                },
            }
        }
        let mut web_frame = frame.web_frame(anchor, self.clip, &self.clock);
        web_frame.events.extend(reset_events);
        // An older host has no controller extension, even if a future transport
        // happens to include fields the legacy stereo client doesn't understand.
        if self.inputs.is_none() {
            web_frame.inputs.clear();
        }
        let sub_images = match self.manager().and_then(|m| m.begin_frame(layers)) {
            Ok(images) => images,
            Err(e) => {
                self.fail(format!("{e:?}"));
                return None;
            },
        };
        web_frame.sub_images = sub_images;
        self.frame = Some(frame);
        Some(web_frame)
    }

    fn end_animation_frame(&mut self, layers: &[(ContextId, LayerId)]) {
        if !self.ended {
            if let Err(error) = self.submit(layers) {
                self.fail(error);
            }
        }
    }
    fn initial_inputs(&self) -> Vec<InputSource> {
        self.inputs
            .as_ref()
            .map_or_else(Vec::new, input::State::sources)
    }
    fn set_event_dest(
        &mut self,
        dest: servo::profile_traits::generic_callback::GenericCallback<Event>,
    ) {
        self.events.upgrade(dest)
    }
    fn quit(&mut self) {
        self.release()
    }
    fn set_quitter(&mut self, quitter: Quitter) {
        self.quitter = Some(quitter)
    }
    fn update_clip_planes(&mut self, near: f32, far: f32) {
        self.clip.update(near, far)
    }
    fn granted_features(&self) -> &[String] {
        &self.features
    }
}
impl Drop for BridgeDevice {
    fn drop(&mut self) {
        self.release();
        let _ = self.device.make_context_current(&self.context);
        unsafe {
            self.gl.delete_framebuffer(self.fbo);
        }
        let _ = self.device.destroy_context(&mut self.context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_tracking() {
        assert!(
            !Pose {
                position: [f32::NAN, 0., 0.],
                orientation: [0., 0., 0., 1.]
            }
            .valid()
        );
        assert!(
            !Pose {
                position: [0.; 3],
                orientation: [0.; 4]
            }
            .valid()
        );
        assert!(
            !Fov {
                left: 1.,
                right: -1.,
                up: 0.7,
                down: -0.7
            }
            .valid()
        );
    }
    #[test]
    fn validates_stereo_dimensions() {
        let valid = json!({"version":1,"width":1280,"height":720,"view_sizes":[[640,720],[640,720]],"max_width":4096,"max_height":4096,"stage":true});
        assert!(Hello::parse(valid.clone()).is_ok());
        let mut invalid = valid;
        invalid["width"] = json!(9000);
        assert!(Hello::parse(invalid).is_err());
        let overflow = json!({"version":1,"width":1280,"height":720,"view_sizes":[[4294967295u32,720],[1281,720]],"max_width":4096,"max_height":4096,"stage":true});
        assert!(Hello::parse(overflow).is_err());
    }
}
