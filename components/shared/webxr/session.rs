/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::thread;
use std::time::Duration;

use euclid::{Point2D, Rect, RigidTransform3D, Size2D};
use log::warn;
use malloc_size_of_derive::MallocSizeOf;
use profile_traits::generic_callback::GenericCallback as ProfileGenericCallback;
use serde::{Deserialize, Serialize};
use servo_base::generic_channel::{self, GenericReceiver, GenericSender};

use crate::{
    ContextId, DeviceAPI, Error, Event, Floor, Frame, FrameUpdateEvent, HitTestId, HitTestSource,
    InputSource, LayerGrandManager, LayerId, LayerInfo, LayerInit, Native, Viewport, Viewports,
};

/// Closing is distinct from closed: selection may move immediately, but change
/// notification must wait until the backend has queued its session-end event.
pub(crate) struct SessionActivity {
    state: AtomicU8,
    wake_on_closed: Option<servo_base::generic_channel::GenericCallback<()>>,
}
impl SessionActivity {
    pub(crate) fn new() -> Self {
        Self { state: AtomicU8::new(0), wake_on_closed: None }
    }
    pub(crate) fn with_waker(wake: servo_base::generic_channel::GenericCallback<()>) -> Self {
        Self { state: AtomicU8::new(0), wake_on_closed: Some(wake) }
    }
    pub(crate) fn is_active(&self) -> bool { self.state.load(Ordering::Acquire) == 0 }
    pub(crate) fn is_closed(&self) -> bool { self.state.load(Ordering::Acquire) == 2 }
    pub(crate) fn begin_closing(&self) {
        let _ = self.state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }
    pub(crate) fn finish_closing(&self) {
        if self.state.swap(2, Ordering::AcqRel) != 2 {
            if let Some(wake) = &self.wake_on_closed { let _ = wake.send(()); }
        }
    }
}

// How long to wait for an rAF.
static TIMEOUT: Duration = Duration::from_millis(5);

/// <https://www.w3.org/TR/webxr/#xrsessionmode-enum>
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SessionMode {
    Inline,
    ImmersiveVR,
    ImmersiveAR,
}

/// <https://immersive-web.github.io/webxr/#dictdef-xrsessioninit>
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionInit {
    /// The requesting document origin, for embedder consent UI.
    pub origin: String,
    /// Trusted requesting top-level webview; never supplied by site script.
    #[serde(default)]
    pub webview_id: Option<servo_base::id::WebViewId>,
    pub required_features: Vec<String>,
    pub optional_features: Vec<String>,
    /// Secondary views are enabled with the `secondary-view` feature
    /// but for performance reasons we also ask users to enable this pref
    /// for now.
    pub first_person_observer_view: bool,
}

impl SessionInit {
    /// Helper function for validating a list of requested features against
    /// a list of supported features for a given mode
    pub fn validate(&self, mode: SessionMode, supported: &[String]) -> Result<Vec<String>, Error> {
        for f in &self.required_features {
            // viewer and local in immersive are granted by default
            // https://immersive-web.github.io/webxr/#default-features
            if f == "viewer" || (f == "local" && mode != SessionMode::Inline) {
                continue;
            }

            if !supported.contains(f) {
                return Err(Error::UnsupportedFeature(f.into()));
            }
        }
        let mut granted = self.required_features.clone();
        for f in &self.optional_features {
            if f == "viewer"
                || (f == "local" && mode != SessionMode::Inline)
                || supported.contains(f)
            {
                granted.push(f.clone());
            }
        }

        Ok(granted)
    }

    pub fn feature_requested(&self, f: &str) -> bool {
        self.required_features
            .iter()
            .chain(self.optional_features.iter())
            .any(|x| *x == f)
    }
}

/// <https://immersive-web.github.io/webxr-ar-module/#xrenvironmentblendmode-enum>
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, MallocSizeOf)]
pub enum EnvironmentBlendMode {
    Opaque,
    AlphaBlend,
    Additive,
}

// The messages that are sent from the content thread to the session thread.
#[derive(Debug, Serialize, Deserialize)]
enum SessionMsg {
    CreateLayer(
        ContextId,
        LayerInit,
        GenericSender<Result<LayerInfo, Error>>,
    ),
    DestroyLayer(ContextId, LayerId),
    SetLayers(Vec<(ContextId, LayerId)>),
    SetLayerViewports(ContextId, LayerId, Viewports),
    SetEventDest(ProfileGenericCallback<Event>),
    SetFrameDest(ProfileGenericCallback<Frame>),
    UpdateClipPlanes(/* near */ f32, /* far */ f32),
    StartRenderLoop,
    RenderAnimationFrame,
    Abort,
    RequestHitTest(HitTestSource),
    CancelHitTest(HitTestId),
    UpdateFrameRate(f32, ProfileGenericCallback<f32>),
    Quit,
    GetBoundsGeometry(GenericSender<Option<Vec<Point2D<f32, Floor>>>>),
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Quitter {
    sender: GenericSender<SessionMsg>,
}

impl Quitter {
    pub fn quit(&self) {
        let _ = self.sender.send(SessionMsg::Quit);
    }

    pub fn abort(&self) {
        let _ = self.sender.send(SessionMsg::Abort);
    }
}

/// An object that represents an XR session.
/// This is owned by the content thread.
/// <https://www.w3.org/TR/webxr/#xrsession-interface>
#[derive(Serialize, Deserialize, MallocSizeOf)]
pub struct Session {
    floor_transform: Option<RigidTransform3D<f32, Native, Floor>>,
    viewports: Viewports,
    sender: GenericSender<SessionMsg>,
    environment_blend_mode: EnvironmentBlendMode,
    initial_inputs: Vec<InputSource>,
    granted_features: Vec<String>,
    id: SessionId,
    supported_frame_rates: Vec<f32>,
    dynamic_viewport_scaling: bool,
    #[serde(default)]
    nominal_frame_rate: Option<f32>,
}

/// A session response that has not yet been adopted by a live document. Keep
/// this guard inside queued tasks so cancelling or dropping a task aborts the
/// backend session instead of losing its only content-side handle.
pub struct PendingSession {
    session: Option<Session>,
}

impl PendingSession {
    pub fn new(session: Session) -> Self {
        Self {
            session: Some(session),
        }
    }

    pub fn into_session(mut self) -> Session {
        self.session
            .take()
            .expect("Session response already adopted")
    }
}

impl Drop for PendingSession {
    fn drop(&mut self) {
        if let Some(session) = &mut self.session {
            session.abort_session();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize, MallocSizeOf)]
pub struct SessionId(pub(crate) u32);

impl Session {
    pub(crate) fn abort_handle(&self) -> Quitter {
        Quitter {
            sender: self.sender.clone(),
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn floor_transform(&self) -> Option<RigidTransform3D<f32, Native, Floor>> {
        self.floor_transform
    }

    pub fn reference_space_bounds(&self) -> Option<Vec<Point2D<f32, Floor>>> {
        let (sender, receiver) = generic_channel::channel()?;
        let _ = self.sender.send(SessionMsg::GetBoundsGeometry(sender));
        receiver.recv().ok()?
    }

    pub fn initial_inputs(&self) -> &[InputSource] {
        &self.initial_inputs
    }

    pub fn environment_blend_mode(&self) -> EnvironmentBlendMode {
        self.environment_blend_mode
    }

    pub fn supports_dynamic_viewport_scaling(&self) -> bool {
        self.dynamic_viewport_scaling
    }

    pub fn set_layer_viewports(&self, context: ContextId, layer: LayerId, viewports: Viewports) {
        let _ = self
            .sender
            .send(SessionMsg::SetLayerViewports(context, layer, viewports));
    }

    pub fn viewports(&self) -> &[Rect<i32, Viewport>] {
        &self.viewports.viewports
    }

    /// A resolution large enough to contain all the viewports.
    /// <https://immersive-web.github.io/webxr/#recommended-webgl-framebuffer-resolution>
    ///
    /// Returns None if the session is inline
    pub fn recommended_framebuffer_resolution(&self) -> Option<Size2D<i32, Viewport>> {
        self.viewports()
            .iter()
            .fold(None::<Rect<_, _>>, |acc, vp| {
                Some(acc.map(|a| a.union(vp)).unwrap_or(*vp))
            })
            .map(|rect| Size2D::new(rect.max_x(), rect.max_y()))
    }

    pub fn create_layer(&self, context_id: ContextId, init: LayerInit) -> Result<LayerInfo, Error> {
        let Some((sender, receiver)) = generic_channel::channel() else {
            return Err(Error::CommunicationError);
        };
        let _ = self
            .sender
            .send(SessionMsg::CreateLayer(context_id, init, sender));
        receiver.recv().map_err(|_| Error::CommunicationError)?
    }

    /// Destroy a layer
    pub fn destroy_layer(&self, context_id: ContextId, layer_id: LayerId) {
        let _ = self
            .sender
            .send(SessionMsg::DestroyLayer(context_id, layer_id));
    }

    pub fn set_layers(&self, layers: Vec<(ContextId, LayerId)>) {
        let _ = self.sender.send(SessionMsg::SetLayers(layers));
    }

    pub fn start_render_loop(&mut self) {
        let _ = self.sender.send(SessionMsg::StartRenderLoop);
    }

    pub fn update_clip_planes(&mut self, near: f32, far: f32) {
        let _ = self.sender.send(SessionMsg::UpdateClipPlanes(near, far));
    }

    pub fn set_event_dest(&mut self, dest: ProfileGenericCallback<Event>) {
        let _ = self.sender.send(SessionMsg::SetEventDest(dest));
    }

    pub fn set_frame_dest(&mut self, dest: ProfileGenericCallback<Frame>) {
        let _ = self.sender.send(SessionMsg::SetFrameDest(dest));
    }

    pub fn render_animation_frame(&mut self) {
        let _ = self.sender.send(SessionMsg::RenderAnimationFrame);
    }

    pub fn end_session(&mut self) {
        let _ = self.sender.send(SessionMsg::Quit);
    }

    /// Stop a device when its document is no longer active. Unlike an orderly
    /// end(), this cannot wait for a frame callback in a suspended script realm.
    pub fn abort_session(&mut self) {
        let _ = self.sender.send(SessionMsg::Abort);
    }

    pub fn apply_event(&mut self, event: FrameUpdateEvent) {
        match event {
            FrameUpdateEvent::NominalFrameRateChanged(rate) => {
                if crate::util::valid_frame_rate(rate) {
                    self.nominal_frame_rate = Some(rate);
                }
            },
            FrameUpdateEvent::UpdateFloorTransform(floor) => self.floor_transform = floor,
            FrameUpdateEvent::UpdateViewports(vp) => self.viewports = vp,
            FrameUpdateEvent::HitTestSourceAdded(_)
            | FrameUpdateEvent::ReferenceSpaceChanged(..) => (),
        }
    }

    pub fn granted_features(&self) -> &[String] {
        &self.granted_features
    }

    pub fn request_hit_test(&self, source: HitTestSource) {
        let _ = self.sender.send(SessionMsg::RequestHitTest(source));
    }

    pub fn cancel_hit_test(&self, id: HitTestId) {
        let _ = self.sender.send(SessionMsg::CancelHitTest(id));
    }

    pub fn update_frame_rate(&mut self, rate: f32, sender: ProfileGenericCallback<f32>) {
        let _ = self.sender.send(SessionMsg::UpdateFrameRate(rate, sender));
    }

    pub fn supported_frame_rates(&self) -> &[f32] {
        &self.supported_frame_rates
    }

    pub fn nominal_frame_rate(&self) -> Option<f32> {
        self.nominal_frame_rate.filter(|rate| crate::util::valid_frame_rate(*rate))
    }
}

#[derive(PartialEq)]
enum RenderState {
    NotInRenderLoop,
    InRenderLoop,
    PendingQuit,
}

/// For devices that want to do their own thread management, the `SessionThread` type is exposed.
pub struct SessionThread<Device: DeviceAPI> {
    receiver: Option<GenericReceiver<SessionMsg>>,
    sender: GenericSender<SessionMsg>,
    layers: Vec<(ContextId, LayerId)>,
    pending_layers: Option<Vec<(ContextId, LayerId)>>,
    frame_count: u64,
    frame_sender: Option<ProfileGenericCallback<Frame>>,
    running: bool,
    closing: bool,
    device: crate::session_device::SessionDevice<Device>,
    id: SessionId,
    render_state: RenderState,
    active: Arc<SessionActivity>,
}

impl<Device: DeviceAPI> Drop for SessionThread<Device> {
    fn drop(&mut self) {
        // Release queued reply senders before any potentially deferred cleanup.
        self.receiver.take();
        self.device.retire_or_defer();
    }
}

impl<Device> SessionThread<Device>
where
    Device: DeviceAPI,
{
    pub fn new(device: Device, id: SessionId) -> Result<Self, Error> {
        Self::with_activity(device, id, Arc::new(SessionActivity::new()))
    }
    fn with_activity(device: Device, id: SessionId, active: Arc<SessionActivity>) -> Result<Self, Error> {
        let mut device = crate::session_device::SessionDevice::new(device, active.clone());
        let Some((sender, receiver)) = generic_channel::channel() else {
            return Err(Error::CommunicationError);
        };
        device.set_quitter(Quitter {
            sender: sender.clone(),
        });
        let frame_count = 0;
        let running = true;
        let layers = Vec::new();
        let pending_layers = None;
        Ok(SessionThread {
            sender,
            receiver: Some(receiver),
            device,
            layers,
            pending_layers,
            frame_count,
            frame_sender: None,
            running,
            closing: false,
            id,
            render_state: RenderState::NotInRenderLoop,
            active,
        })
    }

    pub fn new_session(&mut self) -> Session {
        let floor_transform = self.device.floor_transform();
        let viewports = self.device.viewports();
        let sender = self.sender.clone();
        let initial_inputs = self.device.initial_inputs();
        let environment_blend_mode = self.device.environment_blend_mode();
        let granted_features = self.device.granted_features().into();
        let nominal_frame_rate = self.device.nominal_frame_rate()
            .filter(|rate| crate::util::valid_frame_rate(*rate));
        // Target-rate support requires a known nominal rate. Keep backend
        // enumeration order while discarding invalid and duplicate entries.
        let mut supported_frame_rates = Vec::new();
        if nominal_frame_rate.is_some() {
            for rate in self.device.supported_frame_rates() {
                if crate::util::valid_frame_rate(rate) && !supported_frame_rates.contains(&rate) {
                    supported_frame_rates.push(rate);
                }
            }
        }
        Session {
            floor_transform,
            viewports,
            sender,
            initial_inputs,
            environment_blend_mode,
            granted_features,
            id: self.id,
            supported_frame_rates,
            nominal_frame_rate,
            dynamic_viewport_scaling: self.device.supports_dynamic_viewport_scaling(),
        }
    }

    pub fn run(&mut self) {
        while let Some(receiver) = self.receiver.as_ref() {
            let Ok(msg) = receiver.recv() else { break; };
            if !self.handle_msg(msg) { break; }
        }
        self.quit();
        self.drain_cleanup();
    }

    fn drain_cleanup(&mut self) {
        let mut delay = Duration::from_millis(10);
        while !self.device.poll_cleanup() {
            thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_millis(500));
        }
        self.running = false;
        self.active.finish_closing();
    }

    fn handle_msg(&mut self, msg: SessionMsg) -> bool {
        log::debug!("processing {:?}", msg);
        match msg {
            SessionMsg::SetEventDest(dest) => {
                self.device.set_event_dest(dest);
            },
            SessionMsg::SetFrameDest(dest) => {
                self.frame_sender = Some(dest);
            },
            SessionMsg::RequestHitTest(source) => {
                self.device.request_hit_test(source);
            },
            SessionMsg::CancelHitTest(id) => {
                self.device.cancel_hit_test(id);
            },
            SessionMsg::CreateLayer(context_id, layer_init, sender) => {
                let result = self.device.create_layer(context_id, layer_init);
                let _ = sender.send(result);
            },
            SessionMsg::DestroyLayer(context_id, layer_id) => {
                self.layers.retain(|&(_, other_id)| layer_id != other_id);
                self.device.destroy_layer(context_id, layer_id);
            },
            SessionMsg::SetLayerViewports(context, layer, viewports) => {
                if self.layers.contains(&(context, layer)) {
                    self.device.set_layer_viewports(context, layer, viewports);
                }
            },
            SessionMsg::SetLayers(layers) => {
                self.pending_layers = Some(layers);
            },
            SessionMsg::StartRenderLoop => {
                if let Some(layers) = self.pending_layers.take() {
                    self.layers = layers;
                }
                let frame = match self.device.begin_animation_frame(&self.layers[..]) {
                    Some(frame) => frame,
                    None => {
                        warn!("Device stopped providing frames, exiting");
                        self.quit();
                        return false;
                    },
                };
                self.render_state = RenderState::InRenderLoop;
                self.send_frame(frame);
            },
            SessionMsg::UpdateClipPlanes(near, far) => self.device.update_clip_planes(near, far),
            SessionMsg::RenderAnimationFrame => {
                self.frame_count += 1;

                self.device.end_animation_frame(&self.layers[..]);

                if self.render_state == RenderState::PendingQuit {
                    self.quit();
                    return false;
                }

                if let Some(layers) = self.pending_layers.take() {
                    self.layers = layers;
                }
                #[expect(unused_mut)]
                let mut frame = match self.device.begin_animation_frame(&self.layers[..]) {
                    Some(frame) => frame,
                    None => {
                        warn!("Device stopped providing frames, exiting");
                        self.quit();
                        return false;
                    },
                };

                self.send_frame(frame);
            },
            SessionMsg::UpdateFrameRate(rate, sender) => {
                let new_framerate = self.device.update_frame_rate(rate);
                let _ = sender.send(new_framerate);
            },
            SessionMsg::Quit => {
                if self.render_state == RenderState::NotInRenderLoop {
                    self.quit();
                    return false;
                } else {
                    self.render_state = RenderState::PendingQuit;
                }
            },
            SessionMsg::Abort => {
                self.quit();
                return false;
            },
            SessionMsg::GetBoundsGeometry(sender) => {
                let bounds = self.device.reference_space_bounds();
                let _ = sender.send(bounds);
            },
        }
        true
    }

    fn send_frame(&self, frame: Frame) {
        if let Some(frame_sender) = &self.frame_sender {
            let _ = frame_sender.send(frame);
        }
    }

    fn quit(&mut self) {
        if self.closing { return; }
        self.closing = true;
        self.active.begin_closing();
        // A closing session accepts no new commands. Dropping the receiver also
        // releases reply senders in commands queued behind Quit/Abort.
        self.receiver.take();
        self.render_state = RenderState::NotInRenderLoop;
        self.device.quit();
    }
}

/// Devices that need to can run sessions on the main thread.
pub trait MainThreadSession: 'static {
    fn run_one_frame(&mut self);
    fn running(&self) -> bool;
    /// Closing sessions can stay owned without requesting continuous rendering.
    fn needs_animation_frames(&self) -> bool { self.running() }
}

impl<Device> MainThreadSession for SessionThread<Device>
where
    Device: DeviceAPI,
{
    fn run_one_frame(&mut self) {
        if self.closing {
            self.running = !self.device.poll_cleanup();
            if self.running { self.device.defer_pending_cleanup(); }
            else { self.active.finish_closing(); }
            return;
        }
        let frame_count = self.frame_count;
        while frame_count == self.frame_count && self.running {
            let Some(receiver) = self.receiver.as_ref() else { self.running = false; break; };
            match receiver.try_recv_timeout(TIMEOUT) {
                Ok(msg) => self.running = self.handle_msg(msg),
                Err(generic_channel::TryReceiveError::Empty) => break,
                Err(_) => { self.running = false; break; },
            }
        }
        if !self.running {
            self.quit();
            self.running = !self.device.poll_cleanup();
            if self.running { self.device.defer_pending_cleanup(); }
            else { self.active.finish_closing(); }
        }
    }

    fn running(&self) -> bool { self.running }
    fn needs_animation_frames(&self) -> bool { self.running && !self.closing }
}

/// A type for building XR sessions
pub struct SessionBuilder<'a, GL> {
    sessions: &'a mut Vec<Box<dyn MainThreadSession>>,
    layer_grand_manager: LayerGrandManager<GL>,
    id: SessionId,
    active: Arc<SessionActivity>,
}

impl<'a, GL: 'static> SessionBuilder<'a, GL> {
    pub fn id(&self) -> SessionId {
        self.id
    }

    pub(crate) fn new(
        sessions: &'a mut Vec<Box<dyn MainThreadSession>>,
        layer_grand_manager: LayerGrandManager<GL>,
        id: SessionId,
        active: Arc<SessionActivity>,
    ) -> Self {
        SessionBuilder {
            sessions,
            layer_grand_manager,
            id,
            active,
        }
    }

    /// For devices which are happy to hand over thread management to webxr.
    pub fn spawn<Device, Factory>(self, factory: Factory) -> Result<Session, Error>
    where
        Factory: 'static + FnOnce(LayerGrandManager<GL>) -> Result<Device, Error> + Send,
        Device: DeviceAPI,
    {
        let Some((acks, ackr)) = generic_channel::channel() else {
            return Err(Error::CommunicationError);
        };
        let layer_grand_manager = self.layer_grand_manager;
        let id = self.id;
        let active = self.active;
        thread::spawn(move || {
            match factory(layer_grand_manager).and_then(|device| SessionThread::with_activity(device, id, active.clone())) {
                Ok(mut thread) => {
                    let session = thread.new_session();
                    if acks.send(Ok(session)).is_ok() {
                        thread.run();
                    } else {
                        thread.quit();
                        thread.drain_cleanup();
                    }
                },
                Err(err) => {
                    let _ = acks.send(Err(err));
                },
            }
            // Failed factories may retain private native contexts on this
            // thread. The request result has already been delivered above.
            crate::deferred_cleanup::drain();
            active.finish_closing();
        });
        ackr.recv().unwrap_or(Err(Error::CommunicationError))
    }

    /// For devices that need to run on the main thread.
    pub fn run_on_main_thread<Device, Factory>(self, factory: Factory) -> Result<Session, Error>
    where
        Factory: 'static + FnOnce(LayerGrandManager<GL>) -> Result<Device, Error>,
        Device: DeviceAPI,
    {
        let device = factory(self.layer_grand_manager)?;
        let mut session_thread = SessionThread::with_activity(device, self.id, self.active)?;
        let session = session_thread.new_session();
        self.sessions.push(Box::new(session_thread));
        Ok(session)
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct DelayedDevice { polls: Rc<Cell<usize>>, quits: Rc<Cell<usize>>, dropped: Rc<Cell<usize>>, during_quit: Option<Box<dyn FnMut()>> }
    impl Drop for DelayedDevice { fn drop(&mut self) { self.dropped.set(self.dropped.get()+1); } }
    impl DeviceAPI for DelayedDevice {
        fn create_layer(&mut self, _: ContextId, _: LayerInit) -> Result<LayerInfo, Error> { unreachable!() }
        fn destroy_layer(&mut self, _: ContextId, _: LayerId) { unreachable!() }
        fn floor_transform(&self) -> Option<RigidTransform3D<f32, Native, Floor>> { None }
        fn viewports(&self) -> Viewports { unreachable!() }
        fn begin_animation_frame(&mut self, _: &[(ContextId, LayerId)]) -> Option<Frame> { unreachable!() }
        fn end_animation_frame(&mut self, _: &[(ContextId, LayerId)]) { unreachable!() }
        fn initial_inputs(&self) -> Vec<InputSource> { vec![] }
        fn set_event_dest(&mut self, _: ProfileGenericCallback<Event>) {}
        fn quit(&mut self) { self.quits.set(self.quits.get()+1); if let Some(probe) = self.during_quit.as_mut() { probe(); } }
        fn poll_cleanup(&mut self) -> bool { self.polls.set(self.polls.get()+1); self.polls.get() >= 3 }
        fn set_quitter(&mut self, _: Quitter) {}
        fn update_clip_planes(&mut self, _: f32, _: f32) {}
        fn granted_features(&self) -> &[String] { &[] }
    }
    fn session() -> SessionThread<DelayedDevice> {
        SessionThread::new(DelayedDevice { polls: Rc::new(Cell::new(0)), quits: Rc::new(Cell::new(0)), dropped: Rc::new(Cell::new(0)), during_quit: None }, SessionId(1)).unwrap()
    }
    #[test]
    fn main_thread_session_retains_pending_cleanup_without_blocking() {
        let mut session = session();
        let polls = session.device.polls.clone();
        session.quit(); session.quit();
        assert_eq!(session.device.quits.get(),1);
        for attempt in 0..2 {
            if attempt!=0 { crate::deferred_cleanup::poll(); }
            session.run_one_frame();
            assert!(session.running());
            assert!(!session.active.is_active());
            assert!(!session.active.is_closed());
        }
        crate::deferred_cleanup::poll();
        session.run_one_frame();
        assert!(!session.running());
        assert!(session.active.is_closed());
        assert_eq!(polls.get(),3);
        assert!(!session.needs_animation_frames());
    }
    #[test]
    fn dedicated_session_drains_before_announcing_closed() {
        let mut session = session();
        session.quit(); session.drain_cleanup();
        assert_eq!(session.device.polls.get(),3);
        assert_eq!(session.device.quits.get(),1);
        assert!(session.active.is_closed());
    }
    #[test]
    fn forced_session_drop_retains_device_and_closing_activity_until_cleanup() {
        let session = session();
        let active = session.active.clone(); let polls = session.device.polls.clone();
        let quits = session.device.quits.clone(); let dropped = session.device.dropped.clone();
        drop(session);
        let retained = dropped.get()==0 && !active.is_closed();
        // Record expectations before draining, but always finish cleanup before
        // asserting so failed controls do not strand thread-local native owners.
        let pending = crate::deferred_cleanup::pending();
        crate::deferred_cleanup::drain();
        assert!(retained && pending);
        assert_eq!(polls.get(),3); assert_eq!(quits.get(),1); assert_eq!(dropped.get(),1);
        assert!(active.is_closed());
    }

    #[test]
    fn guard_before_controller_construction_retains_device() {
        let polls = Rc::new(Cell::new(0)); let quits = Rc::new(Cell::new(0)); let dropped = Rc::new(Cell::new(0));
        let active = Arc::new(SessionActivity::new());
        let device = crate::session_device::SessionDevice::new(DelayedDevice {
            polls: polls.clone(), quits: quits.clone(), dropped: dropped.clone(), during_quit: None,
        },active.clone());
        drop(device);
        let retained = dropped.get()==0 && !active.is_closed();
        crate::deferred_cleanup::drain();
        assert!(retained); assert_eq!(polls.get(),3); assert_eq!(quits.get(),1); assert_eq!(dropped.get(),1);
        assert!(active.is_closed());
    }
    #[test]
    fn partial_session_cleanup_transfers_without_repeating_quit() {
        let mut session = session();
        let polls = session.device.polls.clone(); let quits = session.device.quits.clone();
        let dropped = session.device.dropped.clone(); let active = session.active.clone();
        session.quit(); session.run_one_frame();
        assert_eq!(polls.get(),1);
        drop(session);
        let retained = dropped.get()==0 && polls.get()==1 && !active.is_closed();
        crate::deferred_cleanup::drain();
        assert!(retained); assert_eq!(polls.get(),3); assert_eq!(quits.get(),1); assert_eq!(dropped.get(),1);
        assert!(active.is_closed());
    }
    #[test]
    fn completed_session_drop_does_not_poll_native_cleanup_again() {
        let mut session = session();
        let polls = session.device.polls.clone(); let dropped = session.device.dropped.clone();
        session.quit(); session.drain_cleanup(); drop(session);
        assert_eq!(polls.get(),3); assert_eq!(dropped.get(),1);
        assert!(!crate::deferred_cleanup::pending());
    }

    #[test]
    fn closing_mailbox_releases_queued_response_before_native_cleanup() {
        let mut session = session();
        let polls = session.device.polls.clone();
        let (send,reply) = generic_channel::channel().unwrap();
        session.sender.send(SessionMsg::GetBoundsGeometry(send)).unwrap();
        session.quit();
        let released = matches!(reply.try_recv_timeout(Duration::from_millis(30)),
            Err(generic_channel::TryReceiveError::ReceiveError(_)));
        let still_owned = polls.get()==0 && !session.active.is_closed();
        session.drain_cleanup();
        assert!(released && still_owned);
    }
    #[test]
    fn closing_mailbox_rejects_new_requests_without_waiting_for_native_cleanup() {
        let mut session = session();
        session.quit();
        let (send,reply) = generic_channel::channel().unwrap();
        let rejected = session.sender.send(SessionMsg::GetBoundsGeometry(send)).is_err();
        let released = matches!(reply.try_recv_timeout(Duration::from_millis(30)),
            Err(generic_channel::TryReceiveError::ReceiveError(_)));
        session.drain_cleanup();
        assert!(rejected && released);
    }

    #[test]
    fn closing_mailbox_discards_requests_queued_behind_quit() {
        let mut session = session();
        let active = session.active.clone(); let dropped = session.device.dropped.clone();
        let (send,reply) = generic_channel::channel().unwrap();
        session.sender.send(SessionMsg::Quit).unwrap();
        session.sender.send(SessionMsg::GetBoundsGeometry(send)).unwrap();
        session.run_one_frame();
        let released = matches!(reply.try_recv_timeout(Duration::from_millis(30)),
            Err(generic_channel::TryReceiveError::ReceiveError(_)));
        let cleanup_pending = session.running() && !active.is_closed() && dropped.get()==0;
        crate::deferred_cleanup::drain(); session.run_one_frame();
        assert!(released && cleanup_pending);
        assert!(!session.running() && active.is_closed());
    }
    #[test]
    fn closing_mailbox_forced_drop_closes_replies_before_device_quit() {
        let mut session = session();
        let (send,reply) = generic_channel::channel().unwrap();
        session.sender.send(SessionMsg::GetBoundsGeometry(send)).unwrap();
        let closed = Rc::new(Cell::new(false)); let recorded = closed.clone();
        session.device.during_quit = Some(Box::new(move || {
            recorded.set(matches!(reply.try_recv(),Err(generic_channel::TryReceiveError::ReceiveError(_))));
        }));
        drop(session); crate::deferred_cleanup::drain();
        assert!(closed.get());
    }

}
