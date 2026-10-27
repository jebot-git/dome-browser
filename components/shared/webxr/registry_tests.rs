use super::*;
use crate::{
    DeviceAPI, Event, Floor, Frame, InputSource, LayerId, LayerInfo, LayerInit, LayerManager,
    LayerManagerFactory, Native, Viewports,
};
use euclid::RigidTransform3D;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

struct Waker;
impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(Self)
    }
    fn wake(&self) {}
}
struct Manager;
impl crate::LayerGrandManagerAPI<()> for Manager {
    fn create_layer_manager(&self, _: LayerManagerFactory<()>) -> Result<LayerManager, Error> {
        Err(Error::NoMatchingDevice)
    }
    fn clone_layer_grand_manager(&self) -> LayerGrandManager<()> {
        LayerGrandManager::new(Self)
    }
}
#[derive(Default)]
struct Device {
    events: Option<ProfileGenericCallback<Event>>,
    frames: usize,
}
impl DeviceAPI for Device {
    fn create_layer(&mut self, _: ContextId, _: LayerInit) -> Result<LayerInfo, Error> {
        Err(Error::NoMatchingDevice)
    }
    fn destroy_layer(&mut self, _: ContextId, _: LayerId) {}
    fn floor_transform(&self) -> Option<RigidTransform3D<f32, Native, Floor>> {
        None
    }
    fn viewports(&self) -> Viewports {
        Viewports { viewports: vec![] }
    }
    fn begin_animation_frame(&mut self, _: &[(ContextId, LayerId)]) -> Option<Frame> {
        if self.frames == 0 {
            return None;
        }
        self.frames -= 1;
        Some(Frame {
            pose: None,
            inputs: vec![],
            events: vec![],
            sub_images: vec![],
            hit_test_results: vec![],
            predicted_display_time: 0.0,
            predicted_display_instant: None,
        })
    }
    fn end_animation_frame(&mut self, _: &[(ContextId, LayerId)]) {}
    fn initial_inputs(&self) -> Vec<InputSource> {
        vec![]
    }
    fn set_event_dest(&mut self, callback: ProfileGenericCallback<Event>) {
        self.events = Some(callback);
    }
    fn quit(&mut self) {
        if let Some(events) = &self.events {
            let _ = events.send(Event::SessionEnd);
        }
    }
    fn set_quitter(&mut self, _: Quitter) {}
    fn update_clip_planes(&mut self, _: f32, _: f32) {}
    fn granted_features(&self) -> &[String] {
        &[]
    }
}
struct Discovery {
    state: Arc<Mutex<Option<ImmersiveDeviceState>>>,
    queries: Arc<std::sync::atomic::AtomicUsize>,
    checks: Arc<std::sync::atomic::AtomicUsize>,
    allow_sessions: bool,
    initial_frames: usize,
}
impl DiscoveryAPI<()> for Discovery {
    fn immersive_device_state(&self) -> Option<ImmersiveDeviceState> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        *self.state.lock().unwrap()
    }
    fn supports_session(&self, mode: SessionMode) -> bool {
        if mode == SessionMode::Inline {
            return true;
        }
        self.state.lock().unwrap().is_some_and(|state| match mode {
            SessionMode::ImmersiveVR => state.supports_vr,
            SessionMode::ImmersiveAR => state.supports_ar,
            SessionMode::Inline => true,
        })
    }
    fn check_context_compatibility(
        &self,
        _: ContextId,
        _: &LayerGrandManager<()>,
    ) -> Result<(), Error> {
        self.checks.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn request_session(
        &mut self,
        _: SessionMode,
        _: &SessionInit,
        builder: SessionBuilder<()>,
    ) -> Result<Session, Error> {
        if !self.allow_sessions {
            return Err(Error::BackendSpecific("Selected device unavailable".into()));
        }
        let frames = self.initial_frames;
        builder.run_on_main_thread(move |_| {
            Ok(Device {
                frames,
                ..Device::default()
            })
        })
    }
}
fn registry() -> MainThreadRegistry<()> {
    MainThreadRegistry::new(Box::new(Waker), LayerGrandManager::new(Manager)).unwrap()
}
fn discovery(vr: bool, ar: bool, allow_sessions: bool) -> Discovery {
    Discovery {
        state: Arc::new(Mutex::new(Some(ImmersiveDeviceState {
            identity: 0,
            supports_vr: vr,
            supports_ar: ar,
        }))),
        queries: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        checks: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        allow_sessions,
        initial_frames: 0,
    }
}
fn init() -> SessionInit {
    SessionInit {
        origin: "https://xr.example".into(),
        webview_id: None,
        required_features: vec!["viewer".into()],
        optional_features: vec![],
        first_person_observer_view: false,
    }
}
fn pipeline(index: u32) -> PipelineId {
    PipelineId {
        namespace_id: servo_base::id::PipelineNamespaceId(1),
        index: servo_base::id::Index::new(index).unwrap(),
    }
}

#[test]
fn mode_queries_and_compatibility_share_one_selected_device() {
    let mut registry = registry();
    let vr = discovery(true, false, true);
    let vr_checks = vr.checks.clone();
    registry.register(vr);
    let ar = discovery(false, true, false);
    let ar_checks = ar.checks.clone();
    registry.register_discovery(Box::new(ar), true);
    assert!(registry.supports_session(SessionMode::ImmersiveAR).is_ok());
    assert!(registry.supports_session(SessionMode::ImmersiveVR).is_err());
    assert!(
        registry
            .request_session(SessionMode::ImmersiveVR, init())
            .is_err()
    );
    assert!(
        registry
            .request_session(SessionMode::ImmersiveAR, init())
            .is_err()
    );
    let (send, receive) = mpsc::channel();
    registry.handle_msg(RegistryMsg::CheckContextCompatibility(
        ContextId(1),
        ProfileGenericCallback::new(move |value| {
            send.send(value).unwrap();
        })
        .unwrap(),
    ));
    assert!(matches!(
        receive.recv_timeout(Duration::from_secs(2)).unwrap(),
        Ok(Ok(0))
    ));
    assert_eq!(vr_checks.load(Ordering::Relaxed), 0);
    assert_eq!(ar_checks.load(Ordering::Relaxed), 1);
}

#[test]
fn active_sessions_retain_selection_until_their_real_shutdown() {
    for mode in [SessionMode::Inline, SessionMode::ImmersiveVR] {
        let mut registry = registry();
        registry.register(discovery(true, false, true));
        registry.supports_session(SessionMode::ImmersiveVR).unwrap();
        let original = registry.selected_device;
        let generation = registry.device_generation;
        let mut session = registry.request_session(mode, init()).unwrap();
        registry.register_discovery(Box::new(discovery(false, true, true)), true);
        assert_eq!(registry.selected_device, original);
        assert_eq!(registry.device_generation, generation);
        assert!(registry.supports_session(SessionMode::ImmersiveAR).is_err());
        session.end_session();
        registry.run_one_frame();
        assert!(registry.supports_session(SessionMode::ImmersiveAR).is_ok());
        assert!(registry.supports_session(SessionMode::ImmersiveVR).is_err());
        assert_eq!(registry.device_generation, generation + 1);
    }
}

#[test]
fn viewer_only_inline_sessions_do_not_enumerate_or_monitor_immersive_devices() {
    let mut registry = registry();
    let device = discovery(true, false, true);
    let queries = device.queries.clone();
    registry.register(device);
    let mut session = registry
        .request_session(SessionMode::Inline, init())
        .unwrap();
    registry.run_one_frame();
    assert_eq!(queries.load(Ordering::Relaxed), 0);
    assert!(!registry.observing_devices);
    session.end_session();
    registry.run_one_frame();
    assert!(registry.active_sessions.is_empty());
}

#[test]
fn backend_replacement_ends_sessions_before_notifying_existing_observers() {
    let mut registry = registry();
    let device = discovery(true, false, true);
    let state = device.state.clone();
    registry.register(device);
    registry.supports_session(SessionMode::ImmersiveVR).unwrap();
    let (send, receive) = mpsc::channel();
    let changes = send.clone();
    registry.handle_msg(RegistryMsg::ObserveDeviceChanges(
        pipeline(1),
        1,
        ProfileGenericCallback::new(move |value: Result<DeviceChange, _>| {
            changes
                .send(if value.unwrap().notify {
                    "change"
                } else {
                    "baseline"
                })
                .unwrap();
        })
        .unwrap(),
    ));
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(2)).unwrap(),
        "baseline"
    );
    let mut session = registry
        .request_session(SessionMode::ImmersiveVR, init())
        .unwrap();
    session.set_event_dest(
        ProfileGenericCallback::new(move |value| {
            if matches!(value, Ok(Event::SessionEnd)) {
                send.send("end").unwrap();
            }
        })
        .unwrap(),
    );
    registry.run_one_frame();
    state.lock().unwrap().as_mut().unwrap().identity = 1;
    registry.refresh_selection();
    assert!(receive.try_recv().is_err());
    assert!(registry.pending_device_change);
    registry.run_one_frame();
    assert_eq!(receive.recv_timeout(Duration::from_secs(2)).unwrap(), "end");
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(2)).unwrap(),
        "change"
    );
    assert_eq!(registry.device_generation, 1);
    assert!(registry.active_sessions.is_empty());
}

#[test]
fn a_new_observer_does_not_replay_a_change_pending_session_shutdown() {
    let mut registry = registry();
    let device = discovery(true, false, true);
    let state = device.state.clone();
    registry.register(device);
    registry.supports_session(SessionMode::ImmersiveVR).unwrap();
    let _session = registry
        .request_session(SessionMode::ImmersiveVR, init())
        .unwrap();
    state.lock().unwrap().as_mut().unwrap().identity = 1;
    registry.refresh_selection();
    assert!(registry.pending_device_change);
    let (send, receive) = mpsc::channel();
    registry.handle_msg(RegistryMsg::ObserveDeviceChanges(
        pipeline(2),
        1,
        ProfileGenericCallback::new(move |value| {
            send.send(value).unwrap();
        })
        .unwrap(),
    ));
    assert!(
        !receive
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap()
            .notify
    );
    registry.run_one_frame();
    assert!(receive.try_recv().is_err());
    assert!(!registry.pending_device_change);
}

#[test]
fn closing_sessions_neither_pin_selection_nor_release_notifications_early() {
    let mut registry = registry();
    registry.register(discovery(true, false, true));
    registry.supports_session(SessionMode::ImmersiveVR).unwrap();
    let mut session = registry
        .request_session(SessionMode::ImmersiveVR, init())
        .unwrap();
    registry.active_sessions[0].active.begin_closing();
    registry.register_discovery(Box::new(discovery(false, true, true)), true);
    registry.refresh_selection();
    assert!(registry.supports_session(SessionMode::ImmersiveAR).is_ok());
    assert!(registry.pending_device_change);
    assert_eq!(registry.draining_sessions.len(), 1);
    session.end_session();
    registry.run_one_frame();
    assert!(registry.draining_sessions.is_empty());
    assert!(!registry.pending_device_change);
}

#[test]
fn frame_production_failure_ends_session_and_releases_selection() {
    for frames in [0, 1] {
        let mut registry = registry();
        let mut device = discovery(true, false, true);
        device.initial_frames = frames;
        registry.register(device);
        let mut session = registry
            .request_session(SessionMode::ImmersiveVR, init())
            .unwrap();
        let (send, receive) = mpsc::channel();
        session.set_event_dest(
            ProfileGenericCallback::new(move |event| {
                if matches!(event, Ok(Event::SessionEnd)) {
                    send.send(()).unwrap();
                }
            })
            .unwrap(),
        );
        session.start_render_loop();
        registry.run_one_frame();
        if frames == 1 {
            session.render_animation_frame();
            registry.run_one_frame();
        }
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(registry.active_sessions.is_empty());
        assert!(registry.sessions.is_empty());
    }
}

#[test]
fn pending_response_drop_aborts_the_unadopted_backend() {
    let mut registry = registry();
    registry.register(discovery(true, false, true));
    let session = registry
        .request_session(SessionMode::Inline, init())
        .unwrap();
    let pending = crate::PendingSession::new(session);
    assert_eq!(registry.active_sessions.len(), 1);
    drop(pending);
    registry.run_one_frame();
    assert!(registry.active_sessions.is_empty());
    assert!(registry.sessions.is_empty());
}

#[test]
fn pending_response_adoption_keeps_the_session_live() {
    let mut registry = registry();
    registry.register(discovery(true, false, true));
    let session = registry
        .request_session(SessionMode::Inline, init())
        .unwrap();
    let mut session = crate::PendingSession::new(session).into_session();
    registry.run_one_frame();
    assert_eq!(registry.active_sessions.len(), 1);
    session.end_session();
    registry.run_one_frame();
    assert!(registry.active_sessions.is_empty());
}

#[test]
fn pending_document_cancellation_is_scoped_to_pipeline_and_generation() {
    let mut registry = registry();
    registry.register(discovery(true, false, true));
    let a = pipeline(1);
    let b = pipeline(2);
    let first = DocumentSessionOwner {
        pipeline: a,
        generation: 1,
    };
    let newer = DocumentSessionOwner {
        pipeline: a,
        generation: 2,
    };
    let other = DocumentSessionOwner {
        pipeline: b,
        generation: 1,
    };
    let _sessions = [first, newer, other].map(|owner| {
        registry
            .request_session_owned(SessionMode::Inline, init(), Some(owner))
            .unwrap()
    });
    registry.handle_msg(RegistryMsg::CancelDocumentSessions(a, 1));
    registry.run_one_frame();
    assert_eq!(registry.active_sessions.len(), 2);
    assert!(registry.draining_sessions.is_empty());
    assert!(registry.active_sessions.iter().all(|session| {
        session
            .owner
            .is_some_and(|owner| owner.pipeline == b || owner.generation == 2)
    }));
    registry.handle_msg(RegistryMsg::CancelDocumentSessions(a, 1));
    registry.run_one_frame();
    assert_eq!(registry.active_sessions.len(), 2);
    registry.handle_msg(RegistryMsg::CancelDocumentSessions(a, 2));
    registry.handle_msg(RegistryMsg::CancelDocumentSessions(b, 1));
    registry.run_one_frame();
    assert!(registry.active_sessions.is_empty());
    assert!(registry.draining_sessions.is_empty());
}

#[test]
fn pending_document_cancellation_closes_a_response_retained_outside_the_dom() {
    let mut registry = registry();
    registry.register(discovery(true, false, true));
    registry.supports_session(SessionMode::ImmersiveVR).unwrap();
    let mut client = registry.registry();
    let (send, receive) = mpsc::channel();
    client.request_session_for_document(
        pipeline(1),
        1,
        SessionMode::Inline,
        init(),
        ProfileGenericCallback::new(move |response| {
            send.send(response).unwrap();
        })
        .unwrap(),
    );
    client.cancel_document_sessions(pipeline(1), 1);
    registry.run_one_frame();
    let _response = receive
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(registry.active_sessions.is_empty());
    registry.register_discovery(Box::new(discovery(false, true, true)), true);
    assert!(registry.supports_session(SessionMode::ImmersiveAR).is_ok());
}
