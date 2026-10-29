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
    LayerManager, MockButton, MockDeviceInit, MockDeviceMsg, MockDiscoveryAPI, MockInputMsg,
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
    clicking: bool,
    buttons: Vec<MockButton>,
}

struct HeadlessDevice {
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
}

#[derive(Clone)]
struct HeadlessView {
    id: u64,
    init: MockViewInit<ApiSpace>,
}

struct HeadlessDeviceData {
    floor_transform: Option<RigidTransform3D<f32, Native, Floor>>,
    viewer_origin: Option<RigidTransform3D<f32, Viewer, Native>>,
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
            supported_features: init.supported_features,
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
            bounds_geometry: vec![],
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
            secondary_views_enabled: mode != SessionMode::Inline
                && granted_features
                    .iter()
                    .any(|feature| feature == "secondary-views"),
        };
        d.sessions.push(per_session);

        let layer_manager = None;
        drop(d);
        xr.spawn(move |grand_manager| {
            Ok(HeadlessDevice {
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
        vec![]
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
            ViewerPose { transform, views }
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
                    pressed: i.clicking,
                    squeezed: button_pressed.get(1).copied().unwrap_or(false),
                    hand: None,
                    button_values,
                    button_pressed,
                    button_touched,
                    axis_values,
                    input_changed: true,
                }
            })
            .collect();
        Frame {
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
        }
    }

    fn trigger_select(&mut self, id: InputId, kind: SelectKind, event: SelectEvent) {
        for i in 0..self.sessions.len() {
            let frame = self.get_frame(&self.sessions[i], Vec::new());
            self.sessions[i]
                .events
                .callback(Event::Select(id, kind, event, frame));
        }
    }

    fn handle_msg(&mut self, msg: MockDeviceMsg) -> bool {
        match msg {
            MockDeviceMsg::SetWorld(w) => self.world = Some(w),
            MockDeviceMsg::ClearWorld => self.world = None,
            MockDeviceMsg::SetViewerOrigin(viewer_origin) => {
                self.viewer_origin = viewer_origin;
            },
            MockDeviceMsg::SetFloorOrigin(floor_origin) => {
                let delta = self
                    .floor_transform
                    .zip(floor_origin)
                    .map(|(old, new)| new.then(&old).cast_unit());
                self.floor_transform = floor_origin.map(|f| f.inverse());
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
                        delta,
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
                    active: true,
                    clicking: false,
                    buttons: init.supported_buttons,
                });
                with_all_sessions!(self, |s| s
                    .events
                    .callback(Event::AddInput(init.source.clone())))
            },
            MockDeviceMsg::MessageInputSource(id, msg) => {
                if let Some(ref mut input) = self.inputs.iter_mut().find(|i| i.source.id == id) {
                    match msg {
                        MockInputMsg::SetHandedness(h) => {
                            input.source.handedness = h;
                            with_all_sessions!(self, |s| {
                                s.events
                                    .callback(Event::UpdateInput(id, input.source.clone()))
                            });
                        },
                        MockInputMsg::SetProfiles(p) => {
                            input.source.profiles = p;
                            with_all_sessions!(self, |s| {
                                s.events
                                    .callback(Event::UpdateInput(id, input.source.clone()))
                            });
                        },
                        MockInputMsg::SetTargetRayMode(t) => {
                            input.source.target_ray_mode = t;
                            with_all_sessions!(self, |s| {
                                s.events
                                    .callback(Event::UpdateInput(id, input.source.clone()))
                            });
                        },
                        MockInputMsg::SetPointerOrigin(p) => input.pointer = p,
                        MockInputMsg::SetGripOrigin(p) => input.grip = p,
                        MockInputMsg::TriggerSelect(kind, event) => {
                            if !input.active {
                                return true;
                            }
                            let clicking = input.clicking;
                            input.clicking = event == SelectEvent::Start;
                            match event {
                                SelectEvent::Start => {
                                    self.trigger_select(id, kind, event);
                                },
                                SelectEvent::End => {
                                    if clicking {
                                        self.trigger_select(id, kind, SelectEvent::Select);
                                    } else {
                                        self.trigger_select(id, kind, SelectEvent::End);
                                    }
                                },
                                SelectEvent::Select => {
                                    self.trigger_select(id, kind, SelectEvent::Start);
                                    self.trigger_select(id, kind, SelectEvent::Select);
                                },
                            }
                        },
                        MockInputMsg::Disconnect => {
                            if input.active {
                                with_all_sessions!(self, |s| s
                                    .events
                                    .callback(Event::RemoveInput(input.source.id)));
                                input.active = false;
                                input.clicking = false;
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
                            with_all_sessions!(self, |s| s.events.callback(Event::UpdateInput(
                                input.source.id,
                                input.source.clone()
                            )));
                        },
                        MockInputMsg::UpdateButtonState(state) => {
                            if let Some(button) = input
                                .buttons
                                .iter_mut()
                                .find(|b| b.button_type == state.button_type)
                            {
                                *button = state;
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
            BaseSpace::Floor => self.floor_transform?.inverse().cast_unit(),
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
mod secondary_views_tests {
    use super::*;
    use euclid::{Rect, Size2D, Transform3D};
    use webxr_api::{LayerGeometry, ViewEye};

    fn init(eye: ViewEye, width: i32, height: i32, observer: bool) -> MockViewInit<ApiSpace> {
        MockViewInit {
            transform: RigidTransform3D::identity(),
            projection: Transform3D::identity(),
            viewport: Rect::from_size(Size2D::new(width, height)),
            fov: None,
            eye,
            is_first_person_observer: observer,
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
    fn session(mode: SessionMode, secondary: bool) -> PerSessionData {
        PerSessionData {
            id: 0,
            mode,
            clip_planes: Default::default(),
            quitter: None,
            events: Default::default(),
            needs_vp_update: false,
            frame_events: vec![],
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
}
