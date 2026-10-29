/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use euclid::{Point2D, RigidTransform3D};
use profile_traits::generic_callback::GenericCallback as ProfileGenericCallback;
use servo_base::generic_channel::GenericReceiver;
use surfman::chains::SwapChains;
use webxr_api::util::{self, ClipPlanes, HitTestList};
use webxr_api::{
    ApiSpace, BaseSpace, ContextId, DeviceAPI, DiscoveryAPI, EnvironmentBlendMode, Error, Event,
    EventBuffer, Floor, Frame, FrameUpdateEvent, HitTestId, HitTestResult, HitTestSource, Input,
    InputFrame, InputId, InputSource, LayerGrandManager, LayerId, LayerInfo, LayerInit,
    LayerManager, MockButton, MockButtonType, MockDeviceInit, MockDeviceMsg, MockDiscoveryAPI, MockInputMsg,
    MockViewInit, MockViewsInit, MockWorld, Native, Quitter, Ray, SelectEvent, SelectKind, Session,
    SessionBuilder, SessionInit, SessionMode, Space, SubImages, View, ViewDescription, Viewer,
    ViewerPose, Viewports, Views,
};

use crate::{SurfmanGL, SurfmanLayerManager};

pub struct HeadlessMockDiscovery {
    enabled: Arc<AtomicBool>,
}

impl HeadlessMockDiscovery {
    pub fn new(enabled: Arc<AtomicBool>) -> Self {
        Self { enabled }
    }
}

struct HeadlessDiscovery {
    data: Arc<Mutex<HeadlessDeviceData>>,
    supports_vr: bool,
    supports_inline: bool,
    supports_ar: bool,
}

struct InputInfo {
    source: InputSource,
    active: bool,
    pointer: Option<RigidTransform3D<f32, Input, Native>>,
    grip: Option<RigidTransform3D<f32, Input, Native>>,
    pointer_emulated: bool,
    grip_emulated: bool,
    clicking: bool,
    buttons: Vec<MockButton>,
}

impl InputInfo {
    fn action_pressed(&self, kind: SelectKind) -> bool {
        match kind {
            SelectKind::Select => self.clicking,
            SelectKind::Squeeze => self.buttons.iter()
                .find(|button| button.button_type == MockButtonType::Grip)
                .is_some_and(|button| button.pressed),
        }
    }

    fn set_action_pressed(&mut self, kind: SelectKind, pressed: bool) -> bool {
        match kind {
            SelectKind::Select => self.clicking = pressed,
            SelectKind::Squeeze => {
                let Some(button) = self.buttons.iter_mut()
                    .find(|button| button.button_type == MockButtonType::Grip) else { return false; };
                button.pressed = pressed;
                button.touched = pressed;
                button.pressed_value = if pressed { 1. } else { 0. };
            },
        }
        true
    }
}

struct HeadlessDevice {
    initial_inputs: Vec<InputSource>,
    data: Arc<Mutex<HeadlessDeviceData>>,
    id: u32,
    hit_tests: HitTestList,
    granted_features: Vec<String>,
    grand_manager: LayerGrandManager<SurfmanGL>,
    layer_manager: Option<LayerManager>,
}

struct PerSessionData {
    id: u32,
    mode: SessionMode,
    clip_planes: ClipPlanes,
    quitter: Option<Quitter>,
    events: EventBuffer,
    needs_vp_update: bool,
    frame_events: Vec<FrameUpdateEvent>,
    secondary_views_enabled: bool,
    visibility_masks: Vec<(u64, webxr_api::ViewEye, Option<webxr_api::VisibilityMask>)>,
    needs_mask_update: bool,
}

#[derive(Clone)]
struct HeadlessView {
    id: u64,
    init: MockViewInit<ApiSpace>,
}

struct HeadlessDeviceData {
    floor_transform: Option<RigidTransform3D<f32, Native, Floor>>,
    viewer_origin: Option<RigidTransform3D<f32, Viewer, Native>>,
    viewer_emulated: bool,
    supported_features: Vec<String>,
    views: Vec<HeadlessView>,
    secondary_views: Vec<HeadlessView>,
    next_view_id: u64,
    inputs: Vec<InputInfo>,
    sessions: Vec<PerSessionData>,
    disconnected: bool,
    immersive: bool,
    device_change_callback: Option<ProfileGenericCallback<()>>,
    world: Option<MockWorld>,
    next_id: u32,
    bounds_geometry: Vec<Point2D<f32, Floor>>,
}

impl MockDiscoveryAPI<SurfmanGL> for HeadlessMockDiscovery {
    fn simulate_device_connection(
        &mut self,
        init: MockDeviceInit,
        receiver: GenericReceiver<MockDeviceMsg>,
    ) -> Result<Box<dyn DiscoveryAPI<SurfmanGL>>, Error> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Err(Error::NoMatchingDevice);
        }

        let viewer_origin = init.viewer_origin;
        let floor_transform = init.floor_origin.map(|f| f.inverse());
        let mut next_view_id = 0;
        let mut identify = |init| {
            let id = next_view_id;
            next_view_id += 1;
            HeadlessView { id, init }
        };
        let views = init.views.into_iter().map(&mut identify).collect();
        let secondary_views = init
            .secondary_views
            .into_iter()
            .map(&mut identify)
            .collect();
        let data = HeadlessDeviceData {
            floor_transform,
            viewer_origin,
            viewer_emulated: false,            supported_features: init.supported_features,
            views,
            secondary_views,
            next_view_id,
            inputs: vec![],
            sessions: vec![],
            disconnected: false,
            immersive: init.supports_vr || init.supports_ar,
            device_change_callback: None,
            world: init.world,
            next_id: 0,
            bounds_geometry: init.bounds_geometry,
        };
        let data = Arc::new(Mutex::new(data));
        let data_ = data.clone();

        thread::spawn(move || {
            run_loop(receiver, data_);
        });
        Ok(Box::new(HeadlessDiscovery {
            data,
            supports_vr: init.supports_vr,
            supports_inline: init.supports_inline,
            supports_ar: init.supports_ar,
        }))
    }
}

fn run_loop(receiver: GenericReceiver<MockDeviceMsg>, data: Arc<Mutex<HeadlessDeviceData>>) {
    while let Ok(msg) = receiver.recv() {
        if !data.lock().expect("Mutex poisoned").handle_msg(msg) {
            break;
        }
    }
}

impl DiscoveryAPI<SurfmanGL> for HeadlessDiscovery {
    fn set_device_change_callback(&mut self, callback: Option<ProfileGenericCallback<()>>) {
        self.data.lock().unwrap().device_change_callback = callback;
    }

    fn check_context_compatibility(
        &self,
        context: ContextId,
        manager: &LayerGrandManager<SurfmanGL>,
    ) -> Result<(), Error> {
        crate::compatibility::check_mock(manager, context)
    }

    fn request_session(
        &mut self,
        mode: SessionMode,
        init: &SessionInit,
        xr: SessionBuilder<SurfmanGL>,
    ) -> Result<Session, Error> {
        if !self.supports_session(mode) {
            return Err(Error::NoMatchingDevice);
        }
        let data = self.data.clone();
        let mut d = data.lock().unwrap();
        let granted_features = init.validate(mode, &d.supported_features)?;
        let id = d.next_id;
        d.next_id += 1;
        let per_session = PerSessionData {
            id,
            mode,
            clip_planes: Default::default(),
            quitter: Default::default(),
            events: Default::default(),
            needs_vp_update: false,
            frame_events: vec![],
            visibility_masks: vec![],
            needs_mask_update: true,
            secondary_views_enabled: mode != SessionMode::Inline
                && granted_features
                    .iter()
                    .any(|feature| feature == "secondary-views"),
        };
        // Snapshot existing sources at the same lock boundary as registering
        // the session. Later additions/changes are buffered as events exactly
        // once, including while the session thread is being started.
        let initial_inputs = d.initial_inputs();
        d.sessions.push(per_session);

        let layer_manager = None;
        drop(d);
        xr.spawn(move |grand_manager| {
            Ok(HeadlessDevice {
                initial_inputs,
                data,
                id,
                granted_features,
                hit_tests: HitTestList::default(),
                grand_manager,
                layer_manager,
            })
        })
    }

    fn supports_session(&self, mode: SessionMode) -> bool {
        if self.data.lock().unwrap().disconnected {
            return false;
        }
        match mode {
            SessionMode::Inline => self.supports_inline,
            SessionMode::ImmersiveVR => self.supports_vr,
            SessionMode::ImmersiveAR => self.supports_ar,
        }
    }
}

fn view<Eye>(
    init: MockViewInit<Eye>,
    viewer: RigidTransform3D<f32, Viewer, Native>,
    clip_planes: ClipPlanes,
) -> View<Eye> {
    let projection = if let Some((l, r, t, b)) = init.fov {
        util::fov_to_projection_matrix(l, r, t, b, clip_planes)
    } else {
        init.projection
    };

    View {
        transform: init.transform.inverse().then(&viewer),
        projection,
    }
}

impl HeadlessDevice {
    fn with_per_session<R>(&self, f: impl FnOnce(&mut PerSessionData) -> R) -> R {
        f(self
            .data
            .lock()
            .unwrap()
            .sessions
            .iter_mut()
            .find(|s| s.id == self.id)
            .unwrap())
    }

    fn layer_manager(&mut self) -> Result<&mut LayerManager, Error> {
        if let Some(ref mut manager) = self.layer_manager {
            return Ok(manager);
        }
        let swap_chains = SwapChains::new();
        let layer_manager = self
            .grand_manager
            .create_layer_manager(move |_| Ok(SurfmanLayerManager::new(swap_chains)))?;
        self.layer_manager = Some(layer_manager);
        Ok(self.layer_manager.as_mut().unwrap())
    }
}

impl DeviceAPI for HeadlessDevice {
    fn supports_dynamic_viewport_scaling(&self) -> bool {
        true
    }

    fn floor_transform(&self) -> Option<RigidTransform3D<f32, Native, Floor>> {
        self.data.lock().unwrap().floor_transform
    }

    fn viewports(&self) -> Viewports {
        let d = self.data.lock().unwrap();
        let per_session = d.sessions.iter().find(|s| s.id == self.id).unwrap();
        d.viewports(per_session)
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
        let sub_images = self.layer_manager().ok()?.begin_frame(layers).ok()?;
        let mut data = self.data.lock().unwrap();
        let mut frame = data.get_frame(
            data.sessions.iter().find(|s| s.id == self.id).unwrap(),
            sub_images,
        );
        let per_session = data.sessions.iter_mut().find(|s| s.id == self.id).unwrap();
        if per_session.needs_vp_update {
            per_session.needs_vp_update = false;
            let per_session = data.sessions.iter().find(|s| s.id == self.id).unwrap();
            let vp = data.viewports(per_session);
            frame.events.push(FrameUpdateEvent::UpdateViewports(vp));
        }
        let per_session = data.sessions.iter_mut().find(|s| s.id == self.id).unwrap();
        frame.events.append(&mut per_session.frame_events);
        frame.events.extend(data.update_visibility_masks(self.id));
        frame.events.extend(self.hit_tests.commit_tests());

        if let Some(ref world) = data.world {
            for source in self.hit_tests.tests() {
                let ray = data.native_ray(source.ray, source.space);
                let ray = if let Some(ray) = ray { ray } else { break };
                let hits = world
                    .regions
                    .iter()
                    .filter(|region| source.types.is_type(region.ty))
                    .flat_map(|region| &region.faces)
                    .filter_map(|triangle| triangle.intersect(ray))
                    .map(|space| HitTestResult {
                        space,
                        id: source.id,
                    });
                frame.hit_test_results.extend(hits);
            }
        }

        Some(frame)
    }

    fn end_animation_frame(&mut self, layers: &[(ContextId, LayerId)]) {
        let _ = self.layer_manager().unwrap().end_frame(layers);
        thread::sleep(std::time::Duration::from_millis(20));
    }

    fn initial_inputs(&self) -> Vec<InputSource> {
        self.initial_inputs.clone()
    }

    fn set_event_dest(&mut self, dest: ProfileGenericCallback<Event>) {
        self.with_per_session(|s| s.events.upgrade(dest))
    }

    fn quit(&mut self) {
        let session = {
            let mut data = self.data.lock().unwrap_or_else(|error| error.into_inner());
            data.sessions.iter().position(|session| session.id == self.id)
                .map(|index| data.sessions.swap_remove(index))
        };
        // Release the event destination and quitter when this session ends,
        // rather than retaining its DOM roots until the whole mock disconnects.
        if let Some(mut session) = session {
            session.events.callback(Event::SessionEnd);
        }
    }

    fn set_quitter(&mut self, quitter: Quitter) {
        self.with_per_session(|s| s.quitter = Some(quitter))
    }

    fn update_clip_planes(&mut self, near: f32, far: f32) {
        self.with_per_session(|s| s.clip_planes.update(near, far));
    }

    fn granted_features(&self) -> &[String] {
        &self.granted_features
    }

    fn request_hit_test(&mut self, source: HitTestSource) {
        self.hit_tests.request_hit_test(source)
    }

    fn cancel_hit_test(&mut self, id: HitTestId) {
        self.hit_tests.cancel_hit_test(id)
    }

    fn reference_space_bounds(&self) -> Option<Vec<Point2D<f32, Floor>>> {
        let bounds = self.data.lock().unwrap().bounds_geometry.clone();
        Some(bounds)
    }

    fn environment_blend_mode(&self) -> EnvironmentBlendMode {
        let data = self.data.lock().unwrap();
        let session = data
            .sessions
            .iter()
            .find(|s| s.id == self.id)
            .expect("Failed to find session for this device");

        if session.mode == SessionMode::ImmersiveAR {
            EnvironmentBlendMode::AlphaBlend
        } else {
            EnvironmentBlendMode::Opaque
        }
    }
}

impl Drop for HeadlessDevice {
    fn drop(&mut self) {
        self.quit();
    }
}

macro_rules! with_all_sessions {
    ($self:ident, |$s:ident| $e:expr) => {
        for $s in &mut $self.sessions {
            $e;
        }
    };
}

impl HeadlessDeviceData {
    fn initial_inputs(&self) -> Vec<InputSource> {
        self.inputs.iter().filter(|input| input.active).map(|input| input.source.clone()).collect()
    }

    fn get_frame(&self, s: &PerSessionData, sub_images: Vec<SubImages>) -> Frame {
        let pose = self.viewer_origin.map(|transform| {
            let views = if s.mode == SessionMode::Inline {
                Views::Inline
            } else {
                Views::List(
                    self.active_views(s)
                        .map(|entry| ViewDescription {
                            id: entry.id,
                            eye: entry.init.eye,
                            is_first_person_observer: entry.init.is_first_person_observer,
                            view: view(entry.init.clone(), transform, s.clip_planes),
                        })
                        .collect(),
                )
            };
            ViewerPose { transform, views, emulated_position: self.viewer_emulated }
        });
        let inputs = self
            .inputs
            .iter()
            .filter(|i| i.active)
            .map(|i| {
                let (button_values, button_pressed, button_touched, axis_values) =
                    webxr_api::mock_gamepad_state(&i.buttons, i.clicking);
                InputFrame {
                    id: i.source.id,
                    target_ray_origin: i.pointer,
                    grip_origin: i.grip,
                    target_ray_emulated_position: i.pointer_emulated,
                    grip_emulated_position: i.grip_emulated,
                    pressed: i.source.primary_action && i.clicking,
                    squeezed: i.source.primary_action && button_pressed.get(1).copied().unwrap_or(false),
                    hand: None,
                    button_values,
                    button_pressed,
                    button_touched,
                    axis_values,
                    input_changed: true,
                }
            })
            .collect();
        let untracked_views = pose.is_none().then(|| {
            if s.mode == SessionMode::Inline {
                webxr_api::ViewerViews::Inline
            } else {
                webxr_api::ViewerViews::List(self.active_views(s).map(|entry| {
                    let projection = if let Some((l, r, t, b)) = entry.init.fov {
                        util::fov_to_projection_matrix(l, r, t, b, s.clip_planes)
                    } else { entry.init.projection };
                    webxr_api::ViewerView {
                        id: entry.id, eye: entry.init.eye,
                        transform: entry.init.transform.inverse(),
                        projection,
                        is_first_person_observer: entry.init.is_first_person_observer,
                    }
                }).collect())
            }
        });
        Frame {
            untracked_views,
            floor_transform: self.floor_transform,
            pose,
            inputs,
            events: vec![],
            sub_images,
            hit_test_results: vec![],
            predicted_display_time: 0.0,
            predicted_display_instant: None,
        }
    }

    fn active_views(&self, session: &PerSessionData) -> impl Iterator<Item = &HeadlessView> {
        let enabled = session.secondary_views_enabled;
        self.views
            .iter()
            .chain(self.secondary_views.iter().filter(move |_| enabled))
    }

    fn viewports(&self, session: &PerSessionData) -> Viewports {
        let mut viewports = Vec::new();
        if session.mode == SessionMode::Inline {
            return Viewports { viewports };
        }
        let mut x = 0_i32;
        for view in self.active_views(session) {
            let size = view.init.viewport.size;
            let Some(next) = x.checked_add(size.width) else {
                return Viewports {
                    viewports: Vec::new(),
                };
            };
            viewports.push(euclid::Rect::new(Point2D::new(x, 0), size));
            x = next;
        }
        Viewports { viewports }
    }

    fn update_visibility_masks(&mut self, id: u32) -> Vec<FrameUpdateEvent> {
        let session = self.sessions.iter().find(|session| session.id == id).unwrap();
        if !session.needs_mask_update { return vec![]; }
        let current: Vec<_> = if session.mode == SessionMode::Inline {
            vec![]
        } else {
            self.active_views(session).map(|view|
                (view.id, view.init.eye, view.init.visibility_mask.clone())).collect()
        };
        let session = self.sessions.iter_mut().find(|session| session.id == id).unwrap();
        session.needs_mask_update = false;
        let previous = &mut session.visibility_masks;
        let mut events = vec![];
        for (index, view) in current.iter().enumerate() {
            let old = previous.get(index);
            if old != Some(view) && (view.2.is_some() || old.is_some_and(|old| old.2.is_some())) {
                events.push(FrameUpdateEvent::VisibilityMaskChanged {
                    index: index as u32, eye: view.1, mask: view.2.clone().unwrap_or_default(),
                });
            }
        }
        *previous = current;
        events
    }

    fn replace_views(&mut self, primary: MockViewsInit, secondary: Option<MockViewsInit>) {
        let mut identify = |init| {
            let id = self.next_view_id;
            self.next_view_id += 1;
            HeadlessView { id, init }
        };
        self.views = primary.into_iter().map(&mut identify).collect();
        if let Some(secondary) = secondary {
            self.secondary_views = secondary.into_iter().map(identify).collect();
        }
        for session in &mut self.sessions {
            session.needs_vp_update = true;
            session.needs_mask_update = true;
        }
    }

    fn trigger_select(&mut self, id: InputId, kind: SelectKind, event: SelectEvent) {
        if !self.inputs.iter().any(|input| input.source.id == id && input.active && input.source.primary_action) {
            return;
        }
        for i in 0..self.sessions.len() {
            let frame = self.get_frame(&self.sessions[i], Vec::new());
            self.sessions[i]
                .events
                .callback(Event::Select(id, kind, event, frame));
        }
    }

    fn trigger_action(&mut self, id: InputId, kind: SelectKind, event: SelectEvent) {
        let Some(index) = self.inputs.iter().position(|input| input.source.id == id && input.source.primary_action) else { return; };
        let held = self.inputs[index].action_pressed(kind);
        match event {
            SelectEvent::Start if !held => {
                if self.inputs[index].set_action_pressed(kind, true) {
                    self.trigger_select(id, kind, SelectEvent::Start);
                }
            },
            SelectEvent::End if held => {
                self.inputs[index].set_action_pressed(kind, false);
                self.trigger_select(id, kind, SelectEvent::Select);
            },
            SelectEvent::Select => {
                if !held {
                    if !self.inputs[index].set_action_pressed(kind, true) { return; }
                    self.trigger_select(id, kind, SelectEvent::Start);
                }
                self.inputs[index].set_action_pressed(kind, false);
                self.trigger_select(id, kind, SelectEvent::Select);
                if held {
                    self.inputs[index].set_action_pressed(kind, true);
                    self.trigger_select(id, kind, SelectEvent::Start);
                }
            },
            _ => {},
        }
    }

    fn cancel_actions(&mut self, id: InputId) {
        let Some(input) = self.inputs.iter().find(|input| input.source.id == id && input.active && input.source.primary_action) else { return; };
        let held = [input.action_pressed(SelectKind::Select), input.action_pressed(SelectKind::Squeeze)];
        if !held.iter().any(|held| *held) { return; }
        for index in 0..self.sessions.len() {
            let mut frame = self.get_frame(&self.sessions[index], Vec::new());
            if let Some(input) = frame.inputs.iter_mut().find(|input| input.id == id) {
                input.pressed = false;
                input.squeezed = false;
                for button in 0..2 {
                    if let Some(value) = input.button_values.get_mut(button) { *value = 0.; }
                    if let Some(value) = input.button_pressed.get_mut(button) { *value = false; }
                    if let Some(value) = input.button_touched.get_mut(button) { *value = false; }
                }
            }
            for (kind, held) in [SelectKind::Select, SelectKind::Squeeze].into_iter().zip(held) {
                if held {
                    self.sessions[index].events.callback(Event::Select(id, kind, SelectEvent::End, frame.clone()));
                }
            }
        }
    }

    fn handle_msg(&mut self, msg: MockDeviceMsg) -> bool {
        match msg {
            MockDeviceMsg::SetWorld(w) => self.world = Some(w),
            MockDeviceMsg::ClearWorld => self.world = None,
            MockDeviceMsg::SetViewerOrigin(viewer_origin, emulated) => {
                self.viewer_origin = viewer_origin;
                self.viewer_emulated = emulated;
            },
            MockDeviceMsg::SetFloorOrigin(floor_origin) => {
                let bounded_delta = self
                    .floor_transform
                    .zip(floor_origin)
                    .map(|(old, new)| new.then(&old).cast_unit());
                let old_floor = webxr_api::util::local_floor_transform(self.floor_transform);
                self.floor_transform = floor_origin.map(|f| f.inverse());
                let new_floor = webxr_api::util::local_floor_transform(self.floor_transform);
                let delta = Some(new_floor.inverse().then(&old_floor).cast_unit());
                let floor = self.floor_transform;
                with_all_sessions!(self, |s| {
                    s.frame_events
                        .push(FrameUpdateEvent::UpdateFloorTransform(floor));
                    s.frame_events.push(FrameUpdateEvent::ReferenceSpaceChanged(
                        BaseSpace::Floor,
                        delta,
                    ));
                    s.frame_events.push(FrameUpdateEvent::ReferenceSpaceChanged(
                        BaseSpace::BoundedFloor,
                        bounded_delta,
                    ));
                });
            },
            MockDeviceMsg::SetViews(primary, secondary) => {
                self.replace_views(primary, secondary);
            },
            MockDeviceMsg::VisibilityChange(v) => {
                with_all_sessions!(self, |s| s.events.callback(Event::VisibilityChange(v)))
            },
            MockDeviceMsg::AddInputSource(init) => {
                self.inputs.push(InputInfo {
                    source: init.source.clone(),
                    pointer: init.pointer_origin,
                    grip: init.grip_origin,
                    pointer_emulated: false,
                    grip_emulated: false,
                    active: true,
                    clicking: false,
                    buttons: init.supported_buttons,
                });
                with_all_sessions!(self, |s| s
                    .events
                    .callback(Event::AddInput(init.source.clone())))
            },
            MockDeviceMsg::MessageInputSource(id, msg) => {
                if let MockInputMsg::SetPrimaryAction(primary) = &msg {
                    if self.inputs.iter().find(|input| input.source.id == id)
                        .is_none_or(|input| input.source.primary_action == *primary) { return true; }
                }

                // Source replacement/removal must end author interactions before
                // the old XRInputSource is removed. Keep raw controller state so
                // reconnecting does not manufacture a new button press.
                if matches!(&msg, MockInputMsg::Disconnect | MockInputMsg::SetHandedness(_) |
                    MockInputMsg::SetProfiles(_) | MockInputMsg::SetTargetRayMode(_) |
                    MockInputMsg::SetSupportedButtons(_) | MockInputMsg::SetPrimaryAction(_)) {
                    self.cancel_actions(id);
                }
                if let Some(ref mut input) = self.inputs.iter_mut().find(|i| i.source.id == id) {
                    match msg {
                        MockInputMsg::SetPrimaryAction(primary) => {
                            input.source.primary_action = primary;
                            if input.active { with_all_sessions!(self, |session| {
                                session.events.callback(Event::UpdateInput(id, input.source.clone()))
                            }); }
                        },
                        MockInputMsg::SetHandedness(h) => {
                            input.source.handedness = h;
                            if input.active { with_all_sessions!(self, |s| {
                                s.events
                                    .callback(Event::UpdateInput(id, input.source.clone()))
                            }); }
                        },
                        MockInputMsg::SetProfiles(p) => {
                            input.source.profiles = p;
                            if input.active { with_all_sessions!(self, |s| {
                                s.events
                                    .callback(Event::UpdateInput(id, input.source.clone()))
                            }); }
                        },
                        MockInputMsg::SetTargetRayMode(t) => {
                            input.source.target_ray_mode = t;
                            if input.active { with_all_sessions!(self, |s| {
                                s.events
                                    .callback(Event::UpdateInput(id, input.source.clone()))
                            }); }
                        },
                        MockInputMsg::SetPointerOrigin(p, emulated) => {
                            input.pointer = p;
                            input.pointer_emulated = emulated;
                        },
                        MockInputMsg::SetGripOrigin(p, emulated) => {
                            input.grip = p;
                            input.grip_emulated = emulated;
                        },
                        MockInputMsg::TriggerSelect(kind, event) => {
                            self.trigger_action(id, kind, event);
                        },
                        MockInputMsg::Disconnect => {
                            if input.active {
                                with_all_sessions!(self, |s| s
                                    .events
                                    .callback(Event::RemoveInput(input.source.id)));
                                input.active = false;
                            }
                        },
                        MockInputMsg::Reconnect => {
                            if !input.active {
                                with_all_sessions!(self, |s| s
                                    .events
                                    .callback(Event::AddInput(input.source.clone())));
                                input.active = true;
                            }
                        },
                        MockInputMsg::SetSupportedButtons(buttons) => {
                            input.buttons = buttons;
                            input.source.gamepad =
                                Some(webxr_api::mock_gamepad_layout(&input.buttons));
                            if input.active { with_all_sessions!(self, |s| s.events.callback(Event::UpdateInput(
                                input.source.id,
                                input.source.clone()
                            ))); }
                        },
                        MockInputMsg::UpdateButtonState(state) => {
                            let squeeze = state.button_type == MockButtonType::Grip;
                            let pressed = state.pressed;
                            let active = input.active;
                            let mut changed = false;
                            if let Some(button) = input
                                .buttons
                                .iter_mut()
                                .find(|b| b.button_type == state.button_type)
                            {
                                changed = button.pressed != pressed;
                                *button = state;
                            }
                            if squeeze && changed && active {
                                self.trigger_select(id, SelectKind::Squeeze,
                                    if pressed { SelectEvent::Start } else { SelectEvent::Select });
                            }
                        },
                    }
                }
            },
            MockDeviceMsg::Disconnect(s) => {
                self.disconnected = true;
                if self.immersive {
                    if let Some(callback) = &self.device_change_callback { let _ = callback.send(()); }
                }
                with_all_sessions!(self, |s| s.quitter.as_ref().map(|q| q.quit()));
                // notify the client that we're done disconnecting
                let _ = s.send(());
                return false;
            },
            MockDeviceMsg::SetBoundsGeometry(g) => {
                if self.bounds_geometry != g {
                    self.bounds_geometry = g;
                    with_all_sessions!(self, |s| s.frame_events.push(
                        FrameUpdateEvent::ReferenceSpaceChanged(
                            BaseSpace::BoundedFloor,
                            Some(RigidTransform3D::identity())
                        )
                    ));
                }
            },
            MockDeviceMsg::SimulateResetPose => {
                with_all_sessions!(self, |s| s.frame_events.push(
                    FrameUpdateEvent::ReferenceSpaceChanged(
                        BaseSpace::Local,
                        Some(RigidTransform3D::identity())
                    )
                ));
            },
        }
        true
    }

    fn native_ray(&self, ray: Ray<ApiSpace>, space: Space) -> Option<Ray<Native>> {
        let origin: RigidTransform3D<f32, ApiSpace, Native> = match space.base {
            BaseSpace::Local => RigidTransform3D::identity(),
            BaseSpace::Floor => webxr_api::util::local_floor_transform(self.floor_transform)
                .inverse().cast_unit(),
            BaseSpace::Viewer => self.viewer_origin?.cast_unit(),
            BaseSpace::BoundedFloor => self.floor_transform?.inverse().cast_unit(),
            BaseSpace::TargetRay(id) => self
                .inputs
                .iter()
                .find(|i| i.source.id == id)?
                .pointer?
                .cast_unit(),
            BaseSpace::Grip(id) => self
                .inputs
                .iter()
                .find(|i| i.source.id == id)?
                .grip?
                .cast_unit(),
            BaseSpace::Joint(..) => panic!("Cannot request mocking backend with hands"),
        };
        let space_origin = space.offset.then(&origin);

        let origin_rigid: RigidTransform3D<f32, ApiSpace, ApiSpace> = ray.origin.into();
        Some(Ray {
            origin: origin_rigid.then(&space_origin).translation,
            direction: space_origin.rotation.transform_vector3d(ray.direction),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use euclid::{Rect, Size2D, Transform3D, Vector3D};
    use webxr_api::{LayerGeometry, ViewEye};

    fn controller(device: &mut HeadlessDeviceData) -> InputId {
        let id = InputId(12);
        let buttons = vec![MockButton {
            button_type: MockButtonType::Grip, pressed: false, touched: false,
            pressed_value: 0., x_value: 0., y_value: 0.,
        }];
        device.sessions.push(session(SessionMode::ImmersiveVR, false));
        device.handle_msg(MockDeviceMsg::AddInputSource(webxr_api::MockInputInit {
            source: InputSource {
                primary_action: true,
                id, handedness: webxr_api::Handedness::None,
                target_ray_mode: webxr_api::TargetRayMode::TrackedPointer,
                supports_grip: true, hand_support: None, profiles: vec![],
                gamepad: Some(webxr_api::mock_gamepad_layout(&buttons)),
            },
            pointer_origin: Some(RigidTransform3D::identity()),
            grip_origin: Some(RigidTransform3D::identity()),
            supported_buttons: buttons,
        }));
        drain_events(device);
        id
    }

    fn drain_events(device: &mut HeadlessDeviceData) -> Vec<Event> {
        let EventBuffer::Buffered(events) = &mut device.sessions[0].events else { panic!("Buffered test events expected") };
        std::mem::take(events)
    }

    fn grip(pressed: bool) -> MockInputMsg {
        MockInputMsg::UpdateButtonState(MockButton {
            button_type: MockButtonType::Grip, pressed, touched: pressed,
            pressed_value: if pressed { 1. } else { 0. }, x_value: 0., y_value: 0.,
        })
    }

    #[test]
    fn unchanged_or_absent_source_roles_keep_the_device_message_loop_running() {
        let mut device = data();
        let id = controller(&mut device);
        assert!(device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::SetPrimaryAction(true))));
        assert!(device.handle_msg(MockDeviceMsg::MessageInputSource(InputId(999), MockInputMsg::SetPrimaryAction(false))));
        assert!(drain_events(&mut device).is_empty());
        assert!(device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::SetPrimaryAction(false))));
        assert!(matches!(drain_events(&mut device).as_slice(), [Event::UpdateInput(_, source)] if !source.primary_action));
    }

    #[test]
    fn initial_source_snapshot_keeps_classification_and_excludes_disconnected_sources() {
        let mut device = data();
        let id = controller(&mut device);
        let before = device.initial_inputs();
        assert_eq!(before.len(), 1);
        assert!(before[0].primary_action);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::SetPrimaryAction(false)));
        assert!(before[0].primary_action);
        let after = device.initial_inputs();
        assert!(!after[0].primary_action);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::Disconnect));
        assert!(device.initial_inputs().is_empty());
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::Reconnect));
        assert!(!device.initial_inputs()[0].primary_action);
    }

    #[test]
    fn source_role_change_cancels_actions_before_replacement_and_keeps_poses() {
        let mut device = data();
        let id = controller(&mut device);
        device.trigger_action(id, SelectKind::Select, SelectEvent::Start);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, grip(true)));
        drain_events(&mut device);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::SetPrimaryAction(false)));
        let events = drain_events(&mut device);
        assert!(matches!(events.as_slice(), [
            Event::Select(_, SelectKind::Select, SelectEvent::End, _),
            Event::Select(_, SelectKind::Squeeze, SelectEvent::End, _),
            Event::UpdateInput(_, source),
        ] if !source.primary_action));
        let frame = device.get_frame(&device.sessions[0], vec![]);
        assert!(frame.inputs[0].target_ray_origin.is_some() && frame.inputs[0].grip_origin.is_some());
        assert!(!frame.inputs[0].pressed && !frame.inputs[0].squeezed);
        device.trigger_action(id, SelectKind::Select, SelectEvent::Select);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, grip(false)));
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, grip(true)));
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::SetPrimaryAction(false)));
        assert!(drain_events(&mut device).is_empty());
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::SetPrimaryAction(true)));
        assert!(matches!(drain_events(&mut device).as_slice(), [Event::UpdateInput(_, source)] if source.primary_action));
    }

    #[test]
    fn disconnected_source_role_is_retained_without_replacement_events() {
        let mut device = data();
        let id = controller(&mut device);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::Disconnect));
        drain_events(&mut device);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::SetPrimaryAction(false)));
        device.trigger_action(id, SelectKind::Select, SelectEvent::Start);
        assert!(drain_events(&mut device).is_empty());
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::Reconnect));
        assert!(matches!(drain_events(&mut device).as_slice(), [Event::AddInput(source)] if !source.primary_action));
        assert!(!device.get_frame(&device.sessions[0], vec![]).inputs[0].pressed);
    }

    #[test]
    fn interleaved_primary_and_grip_events_have_independent_button_snapshots() {
        let mut device = data();
        let id = controller(&mut device);
        for message in [
            MockInputMsg::TriggerSelect(SelectKind::Select, SelectEvent::Start),
            grip(true),
            MockInputMsg::TriggerSelect(SelectKind::Select, SelectEvent::End),
            grip(false),
        ] { device.handle_msg(MockDeviceMsg::MessageInputSource(id, message)); }
        let events = drain_events(&mut device);
        assert_eq!(events.len(), 4);
        for (event, (kind, transition, primary, squeezed)) in events.iter().zip([
            (SelectKind::Select, SelectEvent::Start, true, false),
            (SelectKind::Squeeze, SelectEvent::Start, true, true),
            (SelectKind::Select, SelectEvent::Select, false, true),
            (SelectKind::Squeeze, SelectEvent::Select, false, false),
        ]) {
            let Event::Select(_, actual_kind, actual_transition, frame) = event else { panic!("Action event expected") };
            assert_eq!((*actual_kind, *actual_transition), (kind, transition));
            let input = &frame.inputs[0];
            assert_eq!((input.pressed, input.squeezed), (primary, squeezed));
            assert_eq!(input.button_pressed, [primary, squeezed]);
        }
    }

    #[test]
    fn simulated_primary_click_restores_a_held_action_without_releasing_grip() {
        let mut device = data();
        let id = controller(&mut device);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, grip(true)));
        device.trigger_action(id, SelectKind::Select, SelectEvent::Start);
        drain_events(&mut device);
        device.trigger_action(id, SelectKind::Select, SelectEvent::Start);
        assert!(drain_events(&mut device).is_empty());
        device.trigger_action(id, SelectKind::Select, SelectEvent::Select);
        let events = drain_events(&mut device);
        assert_eq!(events.len(), 2);
        for (event, (transition, pressed)) in events.iter().zip([(SelectEvent::Select, false), (SelectEvent::Start, true)]) {
            let Event::Select(_, SelectKind::Select, actual, frame) = event else { panic!("Primary event expected") };
            assert_eq!(*actual, transition);
            assert_eq!((frame.inputs[0].pressed, frame.inputs[0].squeezed), (pressed, true));
        }
    }

    #[test]
    fn disconnected_input_updates_are_retained_without_emitting_input_events() {
        let mut device = data();
        let id = controller(&mut device);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::Disconnect));
        drain_events(&mut device);
        device.trigger_action(id, SelectKind::Select, SelectEvent::Start);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, grip(true)));
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::SetProfiles(vec!["pending".into()])));
        assert!(drain_events(&mut device).is_empty());
        assert!(device.get_frame(&device.sessions[0], vec![]).inputs.is_empty());
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::Reconnect));
        let events = drain_events(&mut device);
        assert!(matches!(events.as_slice(), [Event::AddInput(source)] if source.profiles == ["pending"]));
        let frame = device.get_frame(&device.sessions[0], vec![]);
        assert!(frame.inputs[0].pressed && frame.inputs[0].squeezed);
    }

    #[test]
    fn source_removal_cancels_both_actions_before_removal_with_released_snapshots() {
        let mut device = data();
        let id = controller(&mut device);
        device.trigger_action(id, SelectKind::Select, SelectEvent::Start);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, grip(true)));
        drain_events(&mut device);
        device.handle_msg(MockDeviceMsg::MessageInputSource(id, MockInputMsg::Disconnect));
        let events = drain_events(&mut device);
        assert_eq!(events.len(), 3);
        for (event, expected) in events[..2].iter().zip([SelectKind::Select, SelectKind::Squeeze]) {
            let Event::Select(_, kind, SelectEvent::End, frame) = event else { panic!("Cancellation event expected") };
            assert_eq!(*kind, expected);
            assert!(!frame.inputs[0].pressed && !frame.inputs[0].squeezed);
            assert_eq!(frame.inputs[0].button_pressed, [false, false]);
        }
        assert!(matches!(events[2], Event::RemoveInput(removed) if removed == id));
        assert!(device.inputs[0].clicking && device.inputs[0].action_pressed(SelectKind::Squeeze));
    }

    fn init(eye: ViewEye, width: i32, height: i32, observer: bool) -> MockViewInit<ApiSpace> {
        MockViewInit {
            transform: RigidTransform3D::identity(),
            projection: Transform3D::identity(),
            viewport: Rect::from_size(Size2D::new(width, height)),
            fov: None,
            eye,
            is_first_person_observer: observer,
            visibility_mask: None,
        }
    }
    fn primary() -> MockViewsInit {
        vec![
            init(ViewEye::Left, 63, 95, false),
            init(ViewEye::Right, 64, 80, false),
        ]
    }
    fn data() -> HeadlessDeviceData {
        HeadlessDeviceData {
            floor_transform: None,
            viewer_origin: Some(RigidTransform3D::identity()),
            viewer_emulated: false,
            supported_features: vec!["secondary-views".into()],
            views: primary()
                .into_iter()
                .enumerate()
                .map(|(id, init)| HeadlessView {
                    id: id as u64,
                    init,
                })
                .collect(),
            secondary_views: vec![
                HeadlessView {
                    id: 2,
                    init: init(ViewEye::None, 31, 41, true),
                },
                HeadlessView {
                    id: 3,
                    init: init(ViewEye::Left, 17, 59, false),
                },
            ],
            next_view_id: 4,
            inputs: vec![],
            sessions: vec![],
            disconnected: false,
            immersive: true,
            device_change_callback: None,
            world: None,
            next_id: 0,
            bounds_geometry: vec![],
        }
    }

    #[test]
    fn local_floor_rays_use_the_pose_estimate_without_inventing_bounded_calibration() {
        let mut device = data();
        let ray = Ray { origin: Vector3D::zero(), direction: Vector3D::new(0., -1., 0.) };
        let space = Space { base: BaseSpace::Floor, offset: RigidTransform3D::identity() };
        let estimated = device.native_ray(ray, space).unwrap();
        assert_eq!(estimated.origin, Vector3D::new(0., -1.6, 0.));
        assert_eq!(estimated.direction, Vector3D::new(0., -1., 0.));
        assert!(device.native_ray(ray, Space { base: BaseSpace::BoundedFloor, ..space }).is_none());
        let shifted = device.native_ray(ray, Space {
            offset: RigidTransform3D::from_translation(Vector3D::new(3., 0.5, 1.)),
            ..space
        }).unwrap();
        assert!((shifted.origin.y + 1.1).abs() < 1e-5);
        assert_eq!(shifted.origin.x, 3.);
        assert_eq!(shifted.origin.z, 1.);
        device.floor_transform = Some(RigidTransform3D::from_translation(Vector3D::new(0., 2., 0.)));
        assert_eq!(device.native_ray(ray, space).unwrap().origin, Vector3D::new(0., -2., 0.));
        assert_eq!(device.native_ray(ray, Space { base: BaseSpace::BoundedFloor, ..space }).unwrap().origin,
            Vector3D::new(0., -2., 0.));
    }
    fn session(mode: SessionMode, secondary: bool) -> PerSessionData {
        PerSessionData {
            id: 0,
            mode,
            clip_planes: Default::default(),
            quitter: None,
            events: Default::default(),
            needs_vp_update: false,
            frame_events: vec![],
            visibility_masks: vec![],
            needs_mask_update: true,
            secondary_views_enabled: secondary && mode != SessionMode::Inline,
        }
    }
    #[test]
    fn secondary_views_are_gated_per_session_and_preserve_metadata() {
        let d = data();
        for (mode, enabled, count) in [
            (SessionMode::Inline, true, 1),
            (SessionMode::ImmersiveVR, false, 2),
            (SessionMode::ImmersiveVR, true, 4),
        ] {
            let s = session(mode, enabled);
            let frame = d.get_frame(&s, vec![]);
            let views = frame.pose.unwrap().views;
            assert_eq!(views.identities().len(), count);
            if mode == SessionMode::Inline {
                assert!(matches!(views, Views::Inline));
                continue;
            }
            let Views::List(views) = views else {
                panic!("Expected described views")
            };
            assert_eq!(views[0].eye, ViewEye::Left);
            assert!(!views[0].is_first_person_observer);
            if enabled {
                assert_eq!(views[2].eye, ViewEye::None);
                assert!(views[2].is_first_person_observer);
                assert_eq!(views[3].eye, ViewEye::Left);
                assert!(!views[3].is_first_person_observer);
            }
        }
    }
    #[test]
    fn secondary_views_omission_retains_identities_and_empty_list_clears_them() {
        let mut d = data();
        d.sessions.push(session(SessionMode::ImmersiveVR, true));
        d.replace_views(primary(), None);
        assert_eq!(d.views.iter().map(|v| v.id).collect::<Vec<_>>(), vec![4, 5]);
        assert_eq!(
            d.secondary_views.iter().map(|v| v.id).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(d.sessions[0].needs_vp_update);
        d.replace_views(primary(), Some(vec![]));
        assert!(d.secondary_views.is_empty());
        assert_eq!(d.views[0].id, 6);
    }
    #[test]
    fn secondary_views_pack_asymmetric_regions_and_fit_existing_allocations() {
        let d = data();
        let s = session(SessionMode::ImmersiveVR, true);
        let viewports = d.viewports(&s);
        assert_eq!(
            viewports
                .viewports
                .iter()
                .map(|v| v.origin.x)
                .collect::<Vec<_>>(),
            vec![0, 63, 127, 158]
        );
        let geometry = LayerGeometry::new(&viewports, f32::INFINITY, Size2D::new(127, 95)).unwrap();
        assert!(geometry.framebuffer_size.width <= 127 && geometry.framebuffer_size.height <= 95);
        for (index, rect) in geometry.viewports.viewports.iter().enumerate() {
            assert!(rect.size.width > 0 && rect.size.height > 0);
            assert!(
                geometry.viewports.viewports[..index]
                    .iter()
                    .all(|other| !other.intersects(rect))
            );
        }
    }
    #[test]
    fn secondary_views_allow_arbitrary_primary_counts_and_repeated_eye_labels() {
        let mut d = data();
        let s = session(SessionMode::ImmersiveVR, true);
        d.replace_views(
            vec![
                init(ViewEye::Left, 9, 10, false),
                init(ViewEye::Left, 11, 12, false),
                init(ViewEye::Right, 13, 14, false),
            ],
            None,
        );
        let Views::List(views) = d.get_frame(&s, vec![]).pose.unwrap().views else {
            panic!("Expected list")
        };
        assert_eq!(views.len(), 5);
        assert_eq!(views[1].eye, ViewEye::Left);
        assert_eq!(
            views.iter().map(|v| v.id).collect::<Vec<_>>(),
            vec![4, 5, 6, 2, 3]
        );
    }
    fn mask() -> webxr_api::VisibilityMask {
        webxr_api::VisibilityMask { vertices: vec![0., 0., 0., 1., 1., 0.], indices: vec![0, 1, 2] }
    }

    #[test]
    fn visibility_masks_are_initial_and_change_only() {
        let mut d = data();
        d.views[0].init.visibility_mask = Some(mask());
        d.sessions.push(session(SessionMode::ImmersiveVR, false));
        let events = d.update_visibility_masks(0);
        assert!(matches!(&events[..], [FrameUpdateEvent::VisibilityMaskChanged { index: 0, eye: ViewEye::Left, mask: m }] if *m == mask()));
        assert!(d.update_visibility_masks(0).is_empty());
        let mut views = primary(); views[0].visibility_mask = Some(mask());
        d.replace_views(views, None);
        assert_eq!(d.update_visibility_masks(0).len(), 1);
        assert!(d.update_visibility_masks(0).is_empty());
    }

    #[test]
    fn clearing_masks_uses_empty_geometry_without_stale_view_indices() {
        let mut d = data();
        for view in &mut d.views { view.init.visibility_mask = Some(mask()); }
        d.sessions.push(session(SessionMode::ImmersiveVR, false));
        assert_eq!(d.update_visibility_masks(0).len(), 2);
        d.replace_views(vec![init(ViewEye::Left, 64, 64, false)], None);
        let events = d.update_visibility_masks(0);
        assert!(matches!(&events[..], [FrameUpdateEvent::VisibilityMaskChanged { index: 0, eye: ViewEye::Left, mask }] if mask.vertices.is_empty() && mask.indices.is_empty()));
    }

    #[test]
    fn visibility_masks_follow_each_sessions_secondary_view_grant() {
        let mut d = data();
        d.views[0].init.visibility_mask = Some(mask());
        d.secondary_views[0].init.visibility_mask = Some(mask());
        let first = session(SessionMode::ImmersiveVR, false);
        let mut second = session(SessionMode::ImmersiveVR, true); second.id = 1;
        let mut inline = session(SessionMode::Inline, true); inline.id = 2;
        d.sessions.extend([first, second, inline]);
        assert_eq!(d.update_visibility_masks(0).len(), 1);
        let events = d.update_visibility_masks(1);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], FrameUpdateEvent::VisibilityMaskChanged { index: 2, eye: ViewEye::None, .. }));
        assert!(d.update_visibility_masks(2).is_empty());
        assert!(d.update_visibility_masks(0).is_empty());
        assert!(d.update_visibility_masks(1).is_empty());
    }

}
