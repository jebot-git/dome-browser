/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use crate::session::SessionActivity;
use embedder_traits::EventLoopWaker;
use log::warn;
use profile_traits::generic_callback::GenericCallback as ProfileGenericCallback;
use serde::{Deserialize, Serialize};
use servo_base::generic_channel::{self, GenericCallback, GenericReceiver, GenericSender};
use servo_base::id::PipelineId;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::{
    ContextId, DiscoveryAPI, Error, GLTypes, ImmersiveDeviceState, LayerGrandManager,
    MainThreadSession, MockDeviceInit, MockDeviceMsg, MockDiscoveryAPI, Quitter, Session,
    SessionBuilder, SessionId, SessionInit, SessionMode,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct DeviceChange {
    pub generation: u64,
    /// Baseline messages reconcile contexts without manufacturing an event.
    pub notify: bool,
}

struct DiscoveryEntry<GL> {
    id: u64,
    device: Box<dyn DiscoveryAPI<GL>>,
    state: Option<ImmersiveDeviceState>,
}

struct ActiveSession {
    id: SessionId,
    owner: Option<DocumentSessionOwner>,
    active: Arc<SessionActivity>,
    abort: Quitter,
    monitor_devices: bool,
}

struct QueuedSessionRequest {
    mode: SessionMode,
    init: SessionInit,
    owner: Option<DocumentSessionOwner>,
    destination: ProfileGenericCallback<Result<Session, Error>>,
    preparing: Option<(u64, SessionId, u64)>,
    tried: Vec<u64>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct DocumentSessionOwner {
    pipeline: PipelineId,
    generation: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Registry {
    sender: GenericSender<RegistryMsg>,
    waker: MainThreadWakerImpl,
}

pub struct MainThreadRegistry<GL: 'static> {
    discoveries: Vec<DiscoveryEntry<GL>>,
    sessions: Vec<Box<dyn MainThreadSession>>,
    mocks: Vec<Box<dyn MockDiscoveryAPI<GL>>>,
    sender: GenericSender<RegistryMsg>,
    receiver: Option<GenericReceiver<RegistryMsg>>,
    closing: bool,
    waker: MainThreadWakerImpl,
    grand_manager: LayerGrandManager<GL>,
    next_session_id: u32,
    device_watchers: HashMap<PipelineId, (u64, ProfileGenericCallback<DeviceChange>, u64)>,
    device_watch_epoch: u64,
    observing_devices: bool,
    next_discovery_id: u64,
    selected_device: Option<(u64, ImmersiveDeviceState)>,
    device_generation: u64,
    enumerated_devices: bool,
    active_sessions: Vec<ActiveSession>,
    pending_requests: VecDeque<QueuedSessionRequest>,
    defer_immersive_preparation: bool,
    draining_sessions: Vec<Arc<SessionActivity>>,
    pending_device_change: bool,
    device_change_serial: u64,
    _cleanup_wakeup: crate::deferred_cleanup::WakeRegistration,
}

#[derive(Clone, Serialize, Deserialize)]
struct MainThreadWakerImpl {
    callback: GenericCallback<()>,
}

impl MainThreadWakerImpl {
    fn new(waker: Box<dyn EventLoopWaker>) -> Result<MainThreadWakerImpl, Error> {
        let callback =
            GenericCallback::new(move |_| waker.wake()).expect("Could not construct callback");
        Ok(MainThreadWakerImpl { callback })
    }

    fn wake(&self) {
        let _ = self.callback.send(());
    }
}

impl Registry {
    /// Serialize simulated disconnection with observer registration. Both
    /// originate on the document thread, but the mock device has its own worker.
    pub fn disconnect_mock_device(
        &self,
        device: GenericSender<MockDeviceMsg>,
        callback: ProfileGenericCallback<()>,
    ) -> Result<(), Error> {
        self.sender
            .send(RegistryMsg::DisconnectMockDevice(device, callback))
            .map_err(|_| Error::CommunicationError)?;
        self.waker.wake();
        Ok(())
    }

    pub fn observe_device_changes(
        &self,
        pipeline: PipelineId,
        generation: u64,
        callback: ProfileGenericCallback<DeviceChange>,
    ) -> Result<(), Error> {
        self.sender
            .send(RegistryMsg::ObserveDeviceChanges(
                pipeline, generation, callback,
            ))
            .map_err(|_| Error::CommunicationError)?;
        self.waker.wake();
        Ok(())
    }

    pub fn stop_observing_device_changes(&self, pipeline: PipelineId, generation: u64) {
        let _ = self.sender.send(RegistryMsg::StopObservingDeviceChanges(
            pipeline, generation,
        ));
        self.waker.wake();
    }

    pub fn check_context_compatibility(
        &mut self,
        context: ContextId,
        dest: ProfileGenericCallback<Result<u64, Error>>,
    ) -> Result<(), Error> {
        self.sender
            .send(RegistryMsg::CheckContextCompatibility(context, dest))
            .map_err(|_| Error::CommunicationError)?;
        self.waker.wake();
        Ok(())
    }
    pub fn supports_session(
        &mut self,
        mode: SessionMode,
        dest: ProfileGenericCallback<Result<(), Error>>,
    ) {
        let failure = dest.clone();
        if self
            .sender
            .send(RegistryMsg::SupportsSession(mode, dest))
            .is_err()
        {
            let _ = failure.send(Err(Error::CommunicationError));
        }
        self.waker.wake();
    }

    pub fn request_session(
        &mut self,
        mode: SessionMode,
        init: SessionInit,
        dest: ProfileGenericCallback<Result<Session, Error>>,
    ) {
        let failure = dest.clone();
        if self
            .sender
            .send(RegistryMsg::RequestSession(mode, init, None, dest))
            .is_err()
        {
            let _ = failure.send(Err(Error::CommunicationError));
        }
        self.waker.wake();
    }

    pub fn request_session_for_document(
        &mut self,
        pipeline: PipelineId,
        generation: u64,
        mode: SessionMode,
        init: SessionInit,
        dest: ProfileGenericCallback<Result<Session, Error>>,
    ) {
        let owner = DocumentSessionOwner {
            pipeline,
            generation,
        };
        let failure = dest.clone();
        if self
            .sender
            .send(RegistryMsg::RequestSession(mode, init, Some(owner), dest))
            .is_err()
        {
            let _ = failure.send(Err(Error::CommunicationError));
        }
        self.waker.wake();
    }

    pub fn cancel_document_sessions(&self, pipeline: PipelineId, generation: u64) {
        let _ = self
            .sender
            .send(RegistryMsg::CancelDocumentSessions(pipeline, generation));
        self.waker.wake();
    }

    pub fn simulate_device_connection(
        &mut self,
        init: MockDeviceInit,
        dest: ProfileGenericCallback<Result<GenericSender<MockDeviceMsg>, Error>>,
    ) {
        let failure = dest.clone();
        if self
            .sender
            .send(RegistryMsg::SimulateDeviceConnection(init, dest))
            .is_err()
        {
            let _ = failure.send(Err(Error::CommunicationError));
        }
        self.waker.wake();
    }
}

impl<GL: 'static> MainThreadRegistry<GL> {
    /// Stop accepting work before discoveries, callbacks or the cleanup wakeup
    /// are destroyed. Native devices may finish on the owner-thread queue.
    pub fn shutdown(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        if let Some(receiver) = self.receiver.take() {
            // Constellation shutdown normally leaves a finite queue. Bound this
            // pass so a stale external client cannot keep the owner here forever.
            for _ in 0..1024 {
                let Ok(message) = receiver.try_recv() else {
                    break;
                };
                Self::reject_closing_message(message);
            }
        }
        for request in std::mem::take(&mut self.pending_requests) {
            if let Some((id, session, _)) = request.preparing {
                if let Some(entry) = self.discoveries.iter_mut().find(|entry| entry.id == id) {
                    entry.device.cancel_session_preparation(session);
                }
            }
            let _ = request.destination.send(Err(Error::CommunicationError));
        }
        self.device_watchers.clear();
        self.observing_devices = false;
        self.pending_device_change = false;
        self.device_watch_epoch = self.device_watch_epoch.wrapping_add(1);
        for discovery in &mut self.discoveries {
            discovery.device.set_device_change_callback(None);
        }
        for session in &self.active_sessions {
            session.active.begin_closing();
            session.abort.abort();
        }
        // SessionThread Drop releases queued replies, calls quit once and keeps
        // !Send devices on the owning thread until native cleanup completes.
        self.sessions.clear();
        // Discovery destruction may release monitors or native owners. Do this
        // while the caller still tracks cleanup and keeps its native host alive.
        self.discoveries.clear();
        self.mocks.clear();
    }

    pub fn shutdown_complete(&self) -> bool {
        self.closing
            && self
                .active_sessions
                .iter()
                .all(|session| session.active.is_closed())
            && self
                .draining_sessions
                .iter()
                .all(|session| session.is_closed())
    }

    fn reject_closing_message(message: RegistryMsg) {
        match message {
            RegistryMsg::SupportsSession(_, destination) => {
                let _ = destination.send(Err(Error::CommunicationError));
            },
            RegistryMsg::CheckContextCompatibility(_, destination) => {
                let _ = destination.send(Err(Error::CommunicationError));
            },
            RegistryMsg::RequestSession(_, _, _, destination) => {
                let _ = destination.send(Err(Error::CommunicationError));
            },
            RegistryMsg::SimulateDeviceConnection(_, destination) => {
                let _ = destination.send(Err(Error::CommunicationError));
            },
            RegistryMsg::DisconnectMockDevice(device, destination) => {
                if device
                    .send(MockDeviceMsg::Disconnect(destination.clone()))
                    .is_err()
                {
                    let _ = destination.send(());
                }
            },
            _ => {},
        }
    }
}
impl<GL: 'static> Drop for MainThreadRegistry<GL> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl<GL: 'static + GLTypes> MainThreadRegistry<GL> {
    pub fn new(
        waker: Box<dyn EventLoopWaker>,
        grand_manager: LayerGrandManager<GL>,
    ) -> Result<Self, Error> {
        let Some((sender, receiver)) = generic_channel::channel() else {
            return Err(Error::CommunicationError);
        };
        let discoveries = Vec::new();
        let sessions = Vec::new();
        let mocks = Vec::new();
        let waker = MainThreadWakerImpl::new(waker)?;
        let cleanup_waker = waker.clone();
        let cleanup_wakeup = crate::deferred_cleanup::register_waker(move || cleanup_waker.wake())
            .map_err(|_| Error::CommunicationError)?;
        Ok(MainThreadRegistry {
            discoveries,
            sessions,
            mocks,
            sender,
            receiver: Some(receiver),
            closing: false,
            waker,
            grand_manager,
            next_session_id: 0,
            device_watchers: HashMap::new(),
            device_watch_epoch: 0,
            observing_devices: false,
            next_discovery_id: 0,
            selected_device: None,
            device_generation: 0,
            enumerated_devices: false,
            active_sessions: Vec::new(),
            pending_requests: VecDeque::new(),
            defer_immersive_preparation: false,
            draining_sessions: Vec::new(),
            pending_device_change: false,
            device_change_serial: 0,
            _cleanup_wakeup: cleanup_wakeup,
        })
    }

    pub fn registry(&self) -> Registry {
        Registry {
            sender: self.sender.clone(),
            waker: self.waker.clone(),
        }
    }

    pub fn register<D>(&mut self, discovery: D)
    where
        D: DiscoveryAPI<GL>,
    {
        self.register_discovery(Box::new(discovery), false);
    }

    fn device_change_callback(&self) -> Option<ProfileGenericCallback<()>> {
        let sender = self.sender.clone();
        let waker = self.waker.clone();
        let epoch = self.device_watch_epoch;
        ProfileGenericCallback::new(move |_| {
            let _ = sender.send(RegistryMsg::DeviceAvailabilityChanged(epoch));
            waker.wake();
        })
        .ok()
    }

    fn reap_finished_sessions(&mut self) {
        self.draining_sessions.retain(|active| !active.is_closed());
        let draining = &mut self.draining_sessions;
        self.active_sessions.retain(|session| {
            if session.active.is_active() {
                return true;
            }
            if !session.active.is_closed() {
                draining.push(session.active.clone());
            }
            false
        });
    }

    fn configure_device_observation(&mut self) {
        self.reap_finished_sessions();
        let observing = !self.device_watchers.is_empty()
            || self
                .active_sessions
                .iter()
                .any(|session| session.monitor_devices);
        if self.observing_devices == observing {
            return;
        }
        self.observing_devices = observing;
        self.device_watch_epoch = self.device_watch_epoch.wrapping_add(1);
        for index in 0..self.discoveries.len() {
            let callback = if observing {
                self.device_change_callback()
            } else {
                None
            };
            self.discoveries[index]
                .device
                .set_device_change_callback(callback);
        }
    }

    fn notify_device_changes(&mut self) {
        self.draining_sessions.retain(|active| !active.is_closed());
        if !self.draining_sessions.is_empty() {
            self.pending_device_change = true;
            return;
        }
        self.pending_device_change = false;
        let serial = self.device_change_serial;
        let change = DeviceChange {
            generation: self.device_generation,
            notify: true,
        };
        self.device_watchers.retain(|_, (_, callback, observed)| {
            if *observed == serial {
                return true;
            }
            *observed = serial;
            callback.send(change).is_ok()
        });
        self.configure_device_observation();
    }

    fn refresh_selection(&mut self) {
        self.reap_finished_sessions();
        let mut list_changed = false;
        for entry in &mut self.discoveries {
            let state = entry.device.immersive_device_state();
            list_changed |= entry.state != state;
            entry.state = state;
        }
        let previous = self.selected_device;
        let retained = if !self.active_sessions.is_empty() {
            previous.and_then(|(id, old)| {
                self.discoveries
                    .iter()
                    .find(|entry| entry.id == id)
                    .and_then(|entry| {
                        entry
                            .state
                            .filter(|state| state.identity == old.identity)
                            .map(|state| (id, state))
                    })
            })
        } else {
            None
        };
        self.selected_device = retained.or_else(|| {
            self.discoveries
                .iter()
                .find_map(|entry| entry.state.map(|state| (entry.id, state)))
        });
        let selection_changed = previous != self.selected_device;
        if self.enumerated_devices && selection_changed {
            self.device_generation = self.device_generation.wrapping_add(1);
            for session in &self.active_sessions {
                self.draining_sessions.push(session.active.clone());
                session.abort.abort();
            }
            self.active_sessions.clear();
        }
        let notify = self.enumerated_devices && (selection_changed || list_changed);
        self.enumerated_devices = true;
        if notify {
            self.device_change_serial = self.device_change_serial.wrapping_add(1);
            self.notify_device_changes();
        }
        self.configure_device_observation();
    }

    fn register_discovery(&mut self, mut discovery: Box<dyn DiscoveryAPI<GL>>, first: bool) {
        if self.closing {
            return;
        }
        if self.observing_devices {
            discovery.set_device_change_callback(self.device_change_callback());
        }
        let entry = DiscoveryEntry {
            id: self.next_discovery_id,
            device: discovery,
            state: None,
        };
        self.next_discovery_id += 1;
        if first {
            self.discoveries.insert(0, entry);
        } else {
            self.discoveries.push(entry);
        }
        if self.enumerated_devices && self.observing_devices {
            self.refresh_selection();
        }
    }

    pub fn register_mock<D>(&mut self, discovery: D)
    where
        D: MockDiscoveryAPI<GL>,
    {
        if self.closing {
            return;
        }
        self.mocks.push(Box::new(discovery));
    }

    pub fn run_on_main_thread<S>(&mut self, session: S)
    where
        S: MainThreadSession,
    {
        if !self.closing {
            self.sessions.push(Box::new(session));
        }
    }

    pub fn run_one_frame(&mut self) {
        crate::deferred_cleanup::poll_scheduled();
        if self.closing {
            return;
        }
        while let Some(msg) = self
            .receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            self.handle_msg(msg);
        }
        self.poll_session_requests();
        for session in &mut self.sessions {
            session.run_one_frame();
        }
        self.sessions.retain(|session| session.running());
        self.configure_device_observation();
        if self.pending_device_change {
            self.notify_device_changes();
        }
    }

    /// Whether XR needs continuous main-thread rendering. Pending cleanup has
    /// its own wakeups, so it must not force busy rendering while backing off.
    pub fn running(&self) -> bool {
        !self.closing
            && self
                .sessions
                .iter()
                .any(|session| session.needs_animation_frames())
    }

    fn handle_msg(&mut self, msg: RegistryMsg) {
        match msg {
            RegistryMsg::DisconnectMockDevice(device, callback) => {
                if device
                    .send(MockDeviceMsg::Disconnect(callback.clone()))
                    .is_err()
                {
                    let _ = callback.send(());
                }
            },
            RegistryMsg::ObserveDeviceChanges(pipeline, generation, callback) => {
                self.refresh_selection();
                let _ = callback.send(DeviceChange {
                    generation: self.device_generation,
                    notify: false,
                });
                self.device_watchers
                    .insert(pipeline, (generation, callback, self.device_change_serial));
                self.configure_device_observation();
            },
            RegistryMsg::StopObservingDeviceChanges(pipeline, generation) => {
                if self
                    .device_watchers
                    .get(&pipeline)
                    .is_some_and(|(current, _, _)| *current == generation)
                {
                    self.device_watchers.remove(&pipeline);
                    self.configure_device_observation();
                }
            },
            RegistryMsg::DeviceAvailabilityChanged(epoch) => {
                if epoch == self.device_watch_epoch {
                    self.refresh_selection();
                }
            },
            RegistryMsg::CheckContextCompatibility(context, dest) => {
                self.refresh_selection();
                let result = self
                    .selected_device
                    .and_then(|(id, _)| self.discoveries.iter().find(|entry| entry.id == id))
                    .ok_or(Error::NoMatchingDevice)
                    .and_then(|entry| {
                        entry
                            .device
                            .check_context_compatibility(context, &self.grand_manager)
                    })
                    .map(|()| self.device_generation);
                let _ = dest.send(result);
            },
            RegistryMsg::SupportsSession(mode, dest) => {
                let _ = dest.send(self.supports_session(mode));
            },
            RegistryMsg::RequestSession(mode, init, owner, dest) => {
                self.pending_requests.push_back(QueuedSessionRequest {
                    mode,
                    init,
                    owner,
                    destination: dest,
                    preparing: None,
                    tried: Vec::new(),
                });
            },
            RegistryMsg::CancelDocumentSessions(pipeline, generation) => {
                let pending = std::mem::take(&mut self.pending_requests);
                for request in pending {
                    if request.owner.is_some_and(|owner| {
                        owner.pipeline == pipeline && owner.generation <= generation
                    }) {
                        self.defer_immersive_preparation |=
                            request.mode != SessionMode::Inline && request.preparing.is_some();
                        self.cancel_preparation(request.preparing);
                        let _ = request.destination.send(Err(Error::CommunicationError));
                    } else {
                        self.pending_requests.push_back(request);
                    }
                }
                // Request and cancellation messages from the document share
                // this FIFO. No retired-document map is needed after cleanup.
                for session in &self.active_sessions {
                    if session.owner.is_some_and(|owner| {
                        owner.pipeline == pipeline && owner.generation <= generation
                    }) {
                        session.active.begin_closing();
                        session.abort.abort();
                    }
                }
                self.configure_device_observation();
            },
            RegistryMsg::SimulateDeviceConnection(init, dest) => {
                let _ = dest.send(self.simulate_device_connection(init));
            },
        }
    }

    fn cancel_preparation(&mut self, preparing: Option<(u64, SessionId, u64)>) {
        if let Some((device, id, _)) = preparing {
            if let Some(entry) = self.discoveries.iter_mut().find(|entry| entry.id == device) {
                entry.device.cancel_session_preparation(id);
            }
        }
    }

    fn deliver_session(
        &mut self,
        destination: ProfileGenericCallback<Result<Session, Error>>,
        response: Result<Session, Error>,
    ) {
        let id = response.as_ref().ok().map(Session::id);
        if destination.send(response).is_err() {
            if let Some(id) = id {
                for session in &self.active_sessions {
                    if session.id == id {
                        session.active.begin_closing();
                        session.abort.abort();
                    }
                }
                self.configure_device_observation();
            }
        }
    }

    fn poll_session_requests(&mut self) {
        // Let the normal browser loop consume focus restoration from a closed
        // native prompt before another immersive preparation is presented.
        let mut immersive_ui_changed = std::mem::take(&mut self.defer_immersive_preparation);
        let mut immersive_pending = immersive_ui_changed;
        let mut needs_next_iteration = false;
        for _ in 0..self.pending_requests.len() {
            let Some(mut request) = self.pending_requests.pop_front() else {
                break;
            };
            if request.mode != SessionMode::Inline && immersive_pending {
                needs_next_iteration |= immersive_ui_changed;
                self.pending_requests.push_back(request);
                continue;
            }
            if request.mode != SessionMode::Inline {
                self.refresh_selection();
            }
            if request.preparing.is_none() {
                let device = self
                    .discoveries
                    .iter()
                    .find(|entry| {
                        !request.tried.contains(&entry.id)
                            && (request.mode == SessionMode::Inline
                                || self.selected_device.map(|(id, _)| id) == Some(entry.id))
                            && entry.device.supports_session(request.mode)
                    })
                    .map(|entry| entry.id);
                let Some(device) = device else {
                    self.deliver_session(request.destination, Err(Error::NoMatchingDevice));
                    continue;
                };
                let id = SessionId(self.next_session_id);
                self.next_session_id += 1;
                request.preparing = Some((device, id, self.device_generation));
            }
            let (device, id, generation) = request.preparing.unwrap();
            let preparation = if request.mode != SessionMode::Inline
                && (self.selected_device.map(|(id, _)| id) != Some(device)
                    || generation != self.device_generation)
            {
                Err(Error::NoMatchingDevice)
            } else {
                self.discoveries
                    .iter_mut()
                    .find(|entry| entry.id == device)
                    .ok_or(Error::NoMatchingDevice)
                    .and_then(|entry| {
                        entry
                            .device
                            .prepare_session(id, request.mode, &request.init)
                    })
            };
            let response = match preparation {
                Ok(false) => {
                    immersive_pending |= request.mode != SessionMode::Inline;
                    self.pending_requests.push_back(request);
                    continue;
                },
                Ok(true) => self.request_session_prepared(
                    request.mode,
                    request.init.clone(),
                    request.owner,
                    request.preparing,
                ),
                Err(error) => Err(error),
            };
            self.cancel_preparation(request.preparing);
            immersive_pending |= request.mode != SessionMode::Inline;
            immersive_ui_changed |= request.mode != SessionMode::Inline;
            if response.is_err() && request.mode == SessionMode::Inline {
                request.tried.push(device);
                request.preparing = None;
                needs_next_iteration = true;
                self.pending_requests.push_back(request);
                continue;
            }
            self.deliver_session(request.destination, response);
        }
        if needs_next_iteration {
            self.waker.wake();
        }
    }

    fn supports_session(&mut self, mode: SessionMode) -> Result<(), Error> {
        if mode == SessionMode::Inline {
            return Ok(());
        }
        self.refresh_selection();
        let supported = self.selected_device.is_some_and(|(_, state)| match mode {
            SessionMode::ImmersiveVR => state.supports_vr,
            SessionMode::ImmersiveAR => state.supports_ar,
            SessionMode::Inline => true,
        });
        if supported {
            Ok(())
        } else {
            Err(Error::NoMatchingDevice)
        }
    }

    #[cfg(test)]
    fn request_session(&mut self, mode: SessionMode, init: SessionInit) -> Result<Session, Error> {
        self.request_session_owned(mode, init, None)
    }

    #[cfg(test)]
    fn request_session_owned(
        &mut self,
        mode: SessionMode,
        init: SessionInit,
        owner: Option<DocumentSessionOwner>,
    ) -> Result<Session, Error> {
        if self.closing {
            return Err(Error::CommunicationError);
        }
        self.request_session_prepared(mode, init, owner, None)
    }

    fn request_session_prepared(
        &mut self,
        mode: SessionMode,
        init: SessionInit,
        owner: Option<DocumentSessionOwner>,
        prepared: Option<(u64, SessionId, u64)>,
    ) -> Result<Session, Error> {
        if mode != SessionMode::Inline {
            self.refresh_selection();
        }
        if mode != SessionMode::Inline
            && prepared.is_some_and(|(_, _, generation)| generation != self.device_generation)
        {
            return Err(Error::NoMatchingDevice);
        }
        for entry in &mut self.discoveries {
            if prepared.is_some_and(|(device, _, _)| device != entry.id) {
                continue;
            }
            if mode != SessionMode::Inline
                && self.selected_device.map(|(id, _)| id) != Some(entry.id)
            {
                continue;
            }
            if !entry.device.supports_session(mode) {
                continue;
            }
            let id = if let Some((_, id, _)) = prepared {
                id
            } else {
                let id = SessionId(self.next_session_id);
                self.next_session_id += 1;
                id
            };
            let active = Arc::new(SessionActivity::with_waker(self.waker.callback.clone()));
            let xr = SessionBuilder::new(
                &mut self.sessions,
                self.grand_manager.clone(),
                id,
                active.clone(),
            );
            match entry.device.request_session(mode, &init, xr) {
                Ok(session) => {
                    let monitor_devices = mode != SessionMode::Inline
                        || session.granted_features().iter().any(|feature| {
                            matches!(
                                feature.as_str(),
                                "local"
                                    | "local-floor"
                                    | "bounded-floor"
                                    | "unbounded"
                                    | "hit-test"
                                    | "anchors"
                                    | "plane-detection"
                                    | "mesh-detection"
                                    | "hand-tracking"
                            )
                        });
                    self.active_sessions.push(ActiveSession {
                        id,
                        owner,
                        active,
                        abort: session.abort_handle(),
                        monitor_devices,
                    });
                    self.configure_device_observation();
                    return Ok(session);
                },
                Err(err) => warn!("XR device error {:?}", err),
            }
        }
        warn!("no device could support the session");
        Err(Error::NoMatchingDevice)
    }

    fn simulate_device_connection(
        &mut self,
        init: MockDeviceInit,
    ) -> Result<GenericSender<MockDeviceMsg>, Error> {
        let mut obtained = None;
        for mock in &mut self.mocks {
            let Some((sender, receiver)) = generic_channel::channel() else {
                return Err(Error::CommunicationError);
            };
            if let Ok(discovery) = mock.simulate_device_connection(init.clone(), receiver) {
                obtained = Some((sender, discovery));
                break;
            }
        }
        if let Some((sender, discovery)) = obtained {
            self.register_discovery(discovery, true);
            Ok(sender)
        } else {
            Err(Error::NoMatchingDevice)
        }
    }
}

#[derive(Serialize, Deserialize)]
#[expect(clippy::large_enum_variant)]
enum RegistryMsg {
    DisconnectMockDevice(GenericSender<MockDeviceMsg>, ProfileGenericCallback<()>),
    ObserveDeviceChanges(PipelineId, u64, ProfileGenericCallback<DeviceChange>),
    StopObservingDeviceChanges(PipelineId, u64),
    DeviceAvailabilityChanged(u64),
    CheckContextCompatibility(ContextId, ProfileGenericCallback<Result<u64, Error>>),
    RequestSession(
        SessionMode,
        SessionInit,
        Option<DocumentSessionOwner>,
        ProfileGenericCallback<Result<Session, Error>>,
    ),
    CancelDocumentSessions(PipelineId, u64),
    SupportsSession(SessionMode, ProfileGenericCallback<Result<(), Error>>),
    SimulateDeviceConnection(
        MockDeviceInit,
        ProfileGenericCallback<Result<GenericSender<MockDeviceMsg>, Error>>,
    ),
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod selection_tests;
