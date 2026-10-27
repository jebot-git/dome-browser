/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use embedder_traits::EventLoopWaker;
use log::warn;
use profile_traits::generic_callback::GenericCallback as ProfileGenericCallback;
use serde::{Deserialize, Serialize};
use servo_base::generic_channel::{self, GenericCallback, GenericReceiver, GenericSender};
use servo_base::id::PipelineId;
use std::collections::HashMap;

use crate::{
    ContextId, DiscoveryAPI, Error, GLTypes, LayerGrandManager, MainThreadSession, MockDeviceInit,
    MockDeviceMsg, MockDiscoveryAPI, Session, SessionBuilder, SessionId, SessionInit, SessionMode,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Registry {
    sender: GenericSender<RegistryMsg>,
    waker: MainThreadWakerImpl,
}

pub struct MainThreadRegistry<GL> {
    discoveries: Vec<Box<dyn DiscoveryAPI<GL>>>,
    sessions: Vec<Box<dyn MainThreadSession>>,
    mocks: Vec<Box<dyn MockDiscoveryAPI<GL>>>,
    sender: GenericSender<RegistryMsg>,
    receiver: GenericReceiver<RegistryMsg>,
    waker: MainThreadWakerImpl,
    grand_manager: LayerGrandManager<GL>,
    next_session_id: u32,
    device_watchers: HashMap<PipelineId, (u64, ProfileGenericCallback<()>)>,
    device_watch_epoch: u64,
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
        callback: ProfileGenericCallback<()>,
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
        dest: ProfileGenericCallback<Result<(), Error>>,
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
        let _ = self.sender.send(RegistryMsg::SupportsSession(mode, dest));
        self.waker.wake();
    }

    pub fn request_session(
        &mut self,
        mode: SessionMode,
        init: SessionInit,
        dest: ProfileGenericCallback<Result<Session, Error>>,
    ) {
        let _ = self
            .sender
            .send(RegistryMsg::RequestSession(mode, init, dest));
        self.waker.wake();
    }

    pub fn simulate_device_connection(
        &mut self,
        init: MockDeviceInit,
        dest: ProfileGenericCallback<Result<GenericSender<MockDeviceMsg>, Error>>,
    ) {
        let _ = self
            .sender
            .send(RegistryMsg::SimulateDeviceConnection(init, dest));
        self.waker.wake();
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
        Ok(MainThreadRegistry {
            discoveries,
            sessions,
            mocks,
            sender,
            receiver,
            waker,
            grand_manager,
            next_session_id: 0,
            device_watchers: HashMap::new(),
            device_watch_epoch: 0,
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

    fn configure_device_observation(&mut self, observing: bool) {
        self.device_watch_epoch = self.device_watch_epoch.wrapping_add(1);
        for index in 0..self.discoveries.len() {
            let callback = if observing {
                self.device_change_callback()
            } else {
                None
            };
            self.discoveries[index].set_device_change_callback(callback);
        }
    }

    fn notify_device_changes(&mut self) {
        let had_watchers = !self.device_watchers.is_empty();
        self.device_watchers
            .retain(|_, (_, callback)| callback.send(()).is_ok());
        if had_watchers && self.device_watchers.is_empty() {
            self.configure_device_observation(false);
        }
    }

    fn register_discovery(&mut self, mut discovery: Box<dyn DiscoveryAPI<GL>>, first: bool) {
        let observing = !self.device_watchers.is_empty();
        let immersive = observing
            && (discovery.supports_session(SessionMode::ImmersiveVR)
                || discovery.supports_session(SessionMode::ImmersiveAR));
        if observing {
            discovery.set_device_change_callback(self.device_change_callback());
        }
        if first {
            self.discoveries.insert(0, discovery);
        } else {
            self.discoveries.push(discovery);
        }
        if immersive {
            self.notify_device_changes();
        }
    }

    pub fn register_mock<D>(&mut self, discovery: D)
    where
        D: MockDiscoveryAPI<GL>,
    {
        self.mocks.push(Box::new(discovery));
    }

    pub fn run_on_main_thread<S>(&mut self, session: S)
    where
        S: MainThreadSession,
    {
        self.sessions.push(Box::new(session));
    }

    pub fn run_one_frame(&mut self) {
        while let Ok(msg) = self.receiver.try_recv() {
            self.handle_msg(msg);
        }
        for session in &mut self.sessions {
            session.run_one_frame();
        }
        self.sessions.retain(|session| session.running());
    }

    pub fn running(&self) -> bool {
        self.sessions.iter().any(|session| session.running())
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
                let start = self.device_watchers.is_empty();
                self.device_watchers
                    .insert(pipeline, (generation, callback));
                if start {
                    self.configure_device_observation(true);
                }
            },
            RegistryMsg::StopObservingDeviceChanges(pipeline, generation) => {
                if self
                    .device_watchers
                    .get(&pipeline)
                    .is_some_and(|(current, _)| *current == generation)
                {
                    self.device_watchers.remove(&pipeline);
                    if self.device_watchers.is_empty() {
                        self.configure_device_observation(false);
                    }
                }
            },
            RegistryMsg::DeviceAvailabilityChanged(epoch) => {
                if epoch == self.device_watch_epoch {
                    self.notify_device_changes();
                }
            },
            RegistryMsg::CheckContextCompatibility(context, dest) => {
                let result = self
                    .discoveries
                    .iter()
                    .find(|d| {
                        d.supports_session(SessionMode::ImmersiveVR)
                            || d.supports_session(SessionMode::ImmersiveAR)
                    })
                    .ok_or(Error::NoMatchingDevice)
                    .and_then(|d| d.check_context_compatibility(context, &self.grand_manager));
                let _ = dest.send(result);
            },
            RegistryMsg::SupportsSession(mode, dest) => {
                let _ = dest.send(self.supports_session(mode));
            },
            RegistryMsg::RequestSession(mode, init, dest) => {
                let _ = dest.send(self.request_session(mode, init));
            },
            RegistryMsg::SimulateDeviceConnection(init, dest) => {
                let _ = dest.send(self.simulate_device_connection(init));
            },
        }
    }

    fn supports_session(&mut self, mode: SessionMode) -> Result<(), Error> {
        for discovery in &self.discoveries {
            if discovery.supports_session(mode) {
                return Ok(());
            }
        }
        Err(Error::NoMatchingDevice)
    }

    fn request_session(&mut self, mode: SessionMode, init: SessionInit) -> Result<Session, Error> {
        for discovery in &mut self.discoveries {
            if discovery.supports_session(mode) {
                let id = SessionId(self.next_session_id);
                self.next_session_id += 1;
                let xr = SessionBuilder::new(&mut self.sessions, self.grand_manager.clone(), id);
                match discovery.request_session(mode, &init, xr) {
                    Ok(session) => return Ok(session),
                    Err(err) => warn!("XR device error {:?}", err),
                }
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
    ObserveDeviceChanges(PipelineId, u64, ProfileGenericCallback<()>),
    StopObservingDeviceChanges(PipelineId, u64),
    DeviceAvailabilityChanged(u64),
    CheckContextCompatibility(ContextId, ProfileGenericCallback<Result<(), Error>>),
    RequestSession(
        SessionMode,
        SessionInit,
        ProfileGenericCallback<Result<Session, Error>>,
    ),
    SupportsSession(SessionMode, ProfileGenericCallback<Result<(), Error>>),
    SimulateDeviceConnection(
        MockDeviceInit,
        ProfileGenericCallback<Result<GenericSender<MockDeviceMsg>, Error>>,
    ),
}
