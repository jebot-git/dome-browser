/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use rustc_hash::{FxHashMap, FxHashSet};
use servo_canvas_traits::webgl::{
    GenericSender, WebGLMsg, WebXRCommand, WebXRLayerManagerId, webgl_channel,
};
use surfman::{Context, Device};
use webxr::SurfmanGL as WebXRSurfman;
use webxr_api::{
    ContextId as WebXRContextId, Error as WebXRError, GLContexts as WebXRContexts,
    GLTypes as WebXRTypes, LayerGrandManager as WebXRLayerGrandManager,
    LayerGrandManagerAPI as WebXRLayerGrandManagerAPI, LayerId as WebXRLayerId,
    LayerInfo as WebXRLayerInfo, LayerInit as WebXRLayerInit, LayerManager as WebXRLayerManager,
    LayerManagerAPI as WebXRLayerManagerAPI, LayerManagerFactory as WebXRLayerManagerFactory,
    SubImages as WebXRSubImages, Viewports as WebXRViewports,
};

use crate::webgl_thread::WebGLThread;

/// Bridge between WebGL and WebXR
pub(crate) struct WebXRBridge {
    factory_receiver: crossbeam_channel::Receiver<WebXRLayerManagerFactory<WebXRSurfman>>,
    managers: FxHashMap<WebXRLayerManagerId, Box<dyn WebXRLayerManagerAPI<WebXRSurfman>>>,
    next_manager_id: NonZeroU32,
    closing_managers: FxHashSet<WebXRLayerManagerId>,
    pending_layers: FxHashSet<(WebXRLayerManagerId, WebXRContextId, WebXRLayerId)>,
}

impl WebXRBridge {
    pub(crate) fn new(init: WebXRBridgeInit) -> WebXRBridge {
        let WebXRBridgeInit {
            factory_receiver, ..
        } = init;
        let managers = FxHashMap::default();
        let next_manager_id = NonZeroU32::MIN;
        WebXRBridge {
            factory_receiver,
            managers,
            next_manager_id,
            closing_managers: FxHashSet::default(),
            pending_layers: FxHashSet::default(),
        }
    }
}

impl WebXRBridge {
    pub(crate) fn create_layer_manager(
        &mut self,
        contexts: &mut dyn WebXRContexts<WebXRSurfman>,
    ) -> Result<WebXRLayerManagerId, WebXRError> {
        let factory = self
            .factory_receiver
            .recv()
            .map_err(|_| WebXRError::CommunicationError)?;
        let manager = factory.build(contexts)?;
        let manager_id = WebXRLayerManagerId::new(self.next_manager_id);
        self.next_manager_id = self
            .next_manager_id
            .checked_add(1)
            .expect("next_manager_id should not overflow");
        self.managers.insert(manager_id, manager);
        Ok(manager_id)
    }

    pub(crate) fn destroy_layer_manager(&mut self, manager_id: WebXRLayerManagerId) {
        let Some(manager) = self.managers.get(&manager_id) else {
            return;
        };
        if manager.layers().is_empty() {
            self.managers.remove(&manager_id);
            return;
        }
        self.closing_managers.insert(manager_id);
        self.pending_layers.extend(
            manager
                .layers()
                .iter()
                .map(|&(context, layer)| (manager_id, context, layer)),
        );
    }

    pub(crate) fn has_pending_retirement(&self) -> bool {
        !self.pending_layers.is_empty() || !self.closing_managers.is_empty()
    }

    pub(crate) fn has_context_layers(&self, context: WebXRContextId) -> bool {
        self.managers
            .values()
            .any(|manager| manager.layers().iter().any(|ids| ids.0 == context))
    }

    pub(crate) fn retry_retirement(&mut self, contexts: &mut dyn WebXRContexts<WebXRSurfman>) {
        for (manager, context, layer) in self.pending_layers.iter().copied().collect::<Vec<_>>() {
            self.destroy_layer(manager, contexts, context, layer);
        }
        for id in self.closing_managers.iter().copied().collect::<Vec<_>>() {
            if self
                .managers
                .get(&id)
                .is_none_or(|manager| manager.layers().is_empty())
            {
                self.managers.remove(&id);
                self.closing_managers.remove(&id);
            }
        }
    }

    pub(crate) fn create_layer(
        &mut self,
        manager_id: WebXRLayerManagerId,
        contexts: &mut dyn WebXRContexts<WebXRSurfman>,
        context_id: WebXRContextId,
        layer_init: WebXRLayerInit,
        viewports: WebXRViewports,
    ) -> Result<WebXRLayerInfo, WebXRError> {
        if self.closing_managers.contains(&manager_id) {
            return Err(WebXRError::NoMatchingDevice);
        }
        let manager = self
            .managers
            .get_mut(&manager_id)
            .ok_or(WebXRError::NoMatchingDevice)?;
        manager.create_layer(contexts, context_id, layer_init, viewports)
    }

    pub(crate) fn destroy_layer(
        &mut self,
        manager_id: WebXRLayerManagerId,
        contexts: &mut dyn WebXRContexts<WebXRSurfman>,
        context_id: WebXRContextId,
        layer_id: WebXRLayerId,
    ) {
        let pending = (manager_id, context_id, layer_id);
        if let Some(manager) = self.managers.get_mut(&manager_id) {
            manager.destroy_layer(contexts, context_id, layer_id);
            if manager.layers().contains(&(context_id, layer_id)) {
                self.pending_layers.insert(pending);
                return;
            }
        }
        self.pending_layers.remove(&pending);
    }

    pub(crate) fn destroy_all_layers(
        &mut self,
        contexts: &mut dyn WebXRContexts<WebXRSurfman>,
        context_id: WebXRContextId,
    ) -> bool {
        let pending: Vec<_> = self
            .managers
            .iter()
            .flat_map(|(&manager_id, manager)| {
                manager
                    .layers()
                    .iter()
                    .filter(move |ids| ids.0 == context_id)
                    .map(move |&(_, layer)| (manager_id, layer))
            })
            .collect();
        for (manager, layer) in pending {
            self.destroy_layer(manager, contexts, context_id, layer);
        }
        !self.has_context_layers(context_id)
    }

    pub(crate) fn begin_frame(
        &mut self,
        manager_id: WebXRLayerManagerId,
        contexts: &mut dyn WebXRContexts<WebXRSurfman>,
        layers: &[(WebXRContextId, WebXRLayerId)],
    ) -> Result<Vec<WebXRSubImages>, WebXRError> {
        if self.closing_managers.contains(&manager_id) {
            return Err(WebXRError::NoMatchingDevice);
        }
        let manager = self
            .managers
            .get_mut(&manager_id)
            .ok_or(WebXRError::NoMatchingDevice)?;
        // The script render state can still reference a layer whose context
        // was lost. Retired layers contribute no image until replaced, rather
        // than turning a recoverable GL loss into XR device removal.
        let active: Vec<_> = layers
            .iter()
            .filter(|ids| {
                manager.layers().contains(ids)
                    && !self.pending_layers.contains(&(manager_id, ids.0, ids.1))
            })
            .copied()
            .collect();
        manager.begin_frame(contexts, &active)
    }

    pub(crate) fn end_frame(
        &mut self,
        manager_id: WebXRLayerManagerId,
        contexts: &mut dyn WebXRContexts<WebXRSurfman>,
        layers: &[(WebXRContextId, WebXRLayerId)],
    ) -> Result<(), WebXRError> {
        if self.closing_managers.contains(&manager_id) {
            return Err(WebXRError::NoMatchingDevice);
        }
        let manager = self
            .managers
            .get_mut(&manager_id)
            .ok_or(WebXRError::NoMatchingDevice)?;
        let active: Vec<_> = layers
            .iter()
            .filter(|ids| {
                manager.layers().contains(ids)
                    && !self.pending_layers.contains(&(manager_id, ids.0, ids.1))
            })
            .copied()
            .collect();
        manager.end_frame(contexts, &active)
    }
}

pub(crate) struct WebXRBridgeInit {
    sender: GenericSender<WebGLMsg>,
    factory_receiver: crossbeam_channel::Receiver<WebXRLayerManagerFactory<WebXRSurfman>>,
    factory_sender: crossbeam_channel::Sender<WebXRLayerManagerFactory<WebXRSurfman>>,
    factory_order: Arc<Mutex<()>>,
}

impl WebXRBridgeInit {
    pub(crate) fn new(sender: GenericSender<WebGLMsg>) -> WebXRBridgeInit {
        let (factory_sender, factory_receiver) = crossbeam_channel::unbounded();
        WebXRBridgeInit {
            sender,
            factory_sender,
            factory_receiver,
            factory_order: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn layer_grand_manager(&self) -> WebXRLayerGrandManager<WebXRSurfman> {
        WebXRLayerGrandManager::new(WebXRBridgeGrandManager {
            sender: self.sender.clone(),
            factory_sender: self.factory_sender.clone(),
            factory_order: self.factory_order.clone(),
        })
    }
}

struct WebXRBridgeGrandManager {
    sender: GenericSender<WebGLMsg>,
    // WebXR layer manager factories use generic trait objects under the
    // hood, which aren't deserializable (even using typetag)
    // so we can't send them over the regular webgl channel.
    // Fortunately, the webgl thread runs in the same process as
    // the webxr threads, so we can use a crossbeam channel to send
    // factories.
    factory_sender: crossbeam_channel::Sender<WebXRLayerManagerFactory<WebXRSurfman>>,
    factory_order: Arc<Mutex<()>>,
}

impl WebXRLayerGrandManagerAPI<WebXRSurfman> for WebXRBridgeGrandManager {
    fn create_layer_manager(
        &self,
        factory: WebXRLayerManagerFactory<WebXRSurfman>,
    ) -> Result<WebXRLayerManager, WebXRError> {
        let (sender, receiver) = webgl_channel().ok_or(WebXRError::CommunicationError)?;
        {
            // Factories and commands travel through different channels. Keep
            // each pair ordered across every clone, including XR session and
            // compatibility work running on different threads.
            let _order = self
                .factory_order
                .lock()
                .map_err(|_| WebXRError::CommunicationError)?;
            self.factory_sender
                .send(factory)
                .map_err(|_| WebXRError::CommunicationError)?;
            self.sender
                .send(WebGLMsg::WebXRCommand(WebXRCommand::CreateLayerManager(
                    sender,
                )))
                .map_err(|_| WebXRError::CommunicationError)?;
        }
        let sender = self.sender.clone();
        let manager_id = receiver
            .recv()
            .map_err(|_| WebXRError::CommunicationError)??;
        let layers = Vec::new();
        Ok(WebXRLayerManager::new(WebXRBridgeManager {
            manager_id,
            sender,
            layers,
        }))
    }

    fn clone_layer_grand_manager(&self) -> WebXRLayerGrandManager<WebXRSurfman> {
        WebXRLayerGrandManager::new(WebXRBridgeGrandManager {
            sender: self.sender.clone(),
            factory_sender: self.factory_sender.clone(),
            factory_order: self.factory_order.clone(),
        })
    }
}

struct WebXRBridgeManager {
    sender: GenericSender<WebGLMsg>,
    manager_id: WebXRLayerManagerId,
    layers: Vec<(WebXRContextId, WebXRLayerId)>,
}

impl<GL: WebXRTypes> WebXRLayerManagerAPI<GL> for WebXRBridgeManager {
    fn create_layer(
        &mut self,
        _: &mut dyn WebXRContexts<GL>,
        context_id: WebXRContextId,
        init: WebXRLayerInit,
        viewports: WebXRViewports,
    ) -> Result<WebXRLayerInfo, WebXRError> {
        let (sender, receiver) = webgl_channel().ok_or(WebXRError::CommunicationError)?;
        let _ = self
            .sender
            .send(WebGLMsg::WebXRCommand(WebXRCommand::CreateLayer(
                self.manager_id,
                context_id,
                init,
                viewports,
                sender,
            )));
        let layer_id = receiver
            .recv()
            .map_err(|_| WebXRError::CommunicationError)??;
        self.layers.push((context_id, layer_id.id));
        Ok(layer_id)
    }

    fn destroy_layer(
        &mut self,
        _: &mut dyn WebXRContexts<GL>,
        context_id: WebXRContextId,
        layer_id: WebXRLayerId,
    ) {
        self.layers.retain(|&ids| ids != (context_id, layer_id));
        let _ = self
            .sender
            .send(WebGLMsg::WebXRCommand(WebXRCommand::DestroyLayer(
                self.manager_id,
                context_id,
                layer_id,
            )));
    }

    fn layers(&self) -> &[(WebXRContextId, WebXRLayerId)] {
        &self.layers[..]
    }

    fn begin_frame(
        &mut self,
        _: &mut dyn WebXRContexts<GL>,
        layers: &[(WebXRContextId, WebXRLayerId)],
    ) -> Result<Vec<WebXRSubImages>, WebXRError> {
        let (sender, receiver) = webgl_channel().ok_or(WebXRError::CommunicationError)?;
        let _ = self
            .sender
            .send(WebGLMsg::WebXRCommand(WebXRCommand::BeginFrame(
                self.manager_id,
                layers.to_vec(),
                sender,
            )));
        receiver
            .recv()
            .map_err(|_| WebXRError::CommunicationError)?
    }

    fn end_frame(
        &mut self,
        _: &mut dyn WebXRContexts<GL>,
        layers: &[(WebXRContextId, WebXRLayerId)],
    ) -> Result<(), WebXRError> {
        let (sender, receiver) = webgl_channel().ok_or(WebXRError::CommunicationError)?;
        let _ = self
            .sender
            .send(WebGLMsg::WebXRCommand(WebXRCommand::EndFrame(
                self.manager_id,
                layers.to_vec(),
                sender,
            )));
        receiver
            .recv()
            .map_err(|_| WebXRError::CommunicationError)?
    }
}

impl Drop for WebXRBridgeManager {
    fn drop(&mut self) {
        let _ = self
            .sender
            .send(WebGLMsg::WebXRCommand(WebXRCommand::DestroyLayerManager(
                self.manager_id,
            )));
    }
}

impl WebXRContexts<WebXRSurfman> for WebGLThread {
    fn context_lost(&self, context_id: WebXRContextId) -> bool {
        self.native_context_lost(context_id.into())
    }
    fn device(&self, context_id: WebXRContextId) -> Option<Rc<Device>> {
        self.maybe_device_for_context(context_id.into())
    }

    fn context(&mut self, context_id: WebXRContextId) -> Option<&mut Context> {
        if self.native_context_lost(context_id.into()) {
            return self.context_for_retirement(context_id.into());
        }
        let data = self.make_current_if_needed_mut(context_id.into())?;
        Some(&mut data.ctx)
    }

    fn bindings(&mut self, context_id: WebXRContextId) -> Option<&glow::Context> {
        let data = self.make_current_if_needed(context_id.into())?;
        Some(&data.gl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;

    struct NoContexts;
    impl WebXRContexts<WebXRSurfman> for NoContexts {
        fn device(&self, _: WebXRContextId) -> Option<<WebXRSurfman as WebXRTypes>::Device> {
            None
        }
        fn context(
            &mut self,
            _: WebXRContextId,
        ) -> Option<&mut <WebXRSurfman as WebXRTypes>::Context> {
            None
        }
        fn bindings(
            &mut self,
            _: WebXRContextId,
        ) -> Option<&<WebXRSurfman as WebXRTypes>::Bindings> {
            None
        }
    }

    struct Tagged([(WebXRContextId, WebXRLayerId); 1]);
    impl WebXRLayerManagerAPI<WebXRSurfman> for Tagged {
        fn create_layer(
            &mut self,
            _: &mut dyn WebXRContexts<WebXRSurfman>,
            _: WebXRContextId,
            _: WebXRLayerInit,
            _: WebXRViewports,
        ) -> Result<WebXRLayerInfo, WebXRError> {
            Err(WebXRError::NoMatchingDevice)
        }
        fn destroy_layer(
            &mut self,
            _: &mut dyn WebXRContexts<WebXRSurfman>,
            _: WebXRContextId,
            _: WebXRLayerId,
        ) {
        }
        fn layers(&self) -> &[(WebXRContextId, WebXRLayerId)] {
            &self.0
        }
        fn begin_frame(
            &mut self,
            _: &mut dyn WebXRContexts<WebXRSurfman>,
            _: &[(WebXRContextId, WebXRLayerId)],
        ) -> Result<Vec<WebXRSubImages>, WebXRError> {
            Err(WebXRError::NoMatchingDevice)
        }
        fn end_frame(
            &mut self,
            _: &mut dyn WebXRContexts<WebXRSurfman>,
            _: &[(WebXRContextId, WebXRLayerId)],
        ) -> Result<(), WebXRError> {
            Ok(())
        }
    }

    #[test]
    fn concurrent_factories_stay_paired_with_their_commands() {
        const THREADS: usize = 32;
        const ROUNDS: usize = 8;
        let (sender, receiver) = webgl_channel().unwrap();
        let init = WebXRBridgeInit::new(sender);
        let factories = init.factory_receiver.clone();
        let grand = init.layer_grand_manager();
        let reader = thread::spawn(move || {
            let (mut created, mut paired, mut closed) = (0, 0, 0);
            while closed < THREADS * ROUNDS {
                match receiver.recv().unwrap() {
                    WebGLMsg::WebXRCommand(WebXRCommand::CreateLayerManager(reply)) => {
                        let manager = factories.recv().unwrap().build(&mut NoContexts).unwrap();
                        let tag = manager.layers()[0].0.0 as u32;
                        reply
                            .send(Ok(WebXRLayerManagerId::new(NonZeroU32::new(tag).unwrap())))
                            .unwrap();
                        created += 1;
                    },
                    WebGLMsg::WebXRCommand(WebXRCommand::DestroyLayer(manager, context, _)) => {
                        assert_eq!(
                            manager.get() as u64,
                            context.0,
                            "factory result went to a different caller"
                        );
                        paired += 1;
                    },
                    WebGLMsg::WebXRCommand(WebXRCommand::DestroyLayerManager(_)) => closed += 1,
                    other => panic!("unexpected factory-test message: {other:?}"),
                }
            }
            assert_eq!(created, THREADS * ROUNDS);
            assert_eq!(paired, created);
        });
        let barrier = Arc::new(Barrier::new(THREADS));
        let callers: Vec<_> = (0..THREADS)
            .map(|index| {
                let grand = grand.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    for round in 0..ROUNDS {
                        let tag = (round * THREADS + index + 1) as u64;
                        let mut manager = grand
                            .create_layer_manager(move |_| {
                                Ok(Tagged([(WebXRContextId(tag), WebXRLayerId::default())]))
                            })
                            .unwrap();
                        manager.destroy_layer(WebXRContextId(tag), WebXRLayerId::default());
                    }
                })
            })
            .collect();
        for caller in callers {
            caller.join().unwrap();
        }
        reader.join().unwrap();
    }
    struct RetainedManager {
        layers: Vec<(WebXRContextId, WebXRLayerId)>,
        ready: Rc<std::cell::Cell<bool>>,
        dropped: Rc<std::cell::Cell<u32>>,
        frame_layers: Rc<std::cell::RefCell<Vec<(WebXRContextId, WebXRLayerId)>>>,
    }
    impl Drop for RetainedManager {
        fn drop(&mut self) {
            self.dropped.set(self.dropped.get() + 1);
        }
    }
    impl WebXRLayerManagerAPI<WebXRSurfman> for RetainedManager {
        fn create_layer(
            &mut self,
            _: &mut dyn WebXRContexts<WebXRSurfman>,
            _: WebXRContextId,
            _: WebXRLayerInit,
            _: WebXRViewports,
        ) -> Result<WebXRLayerInfo, WebXRError> {
            Err(WebXRError::NoMatchingDevice)
        }
        fn destroy_layer(
            &mut self,
            _: &mut dyn WebXRContexts<WebXRSurfman>,
            context: WebXRContextId,
            layer: WebXRLayerId,
        ) {
            if self.ready.get() {
                self.layers.retain(|ids| *ids != (context, layer));
            }
        }
        fn layers(&self) -> &[(WebXRContextId, WebXRLayerId)] {
            &self.layers
        }
        fn begin_frame(
            &mut self,
            _: &mut dyn WebXRContexts<WebXRSurfman>,
            layers: &[(WebXRContextId, WebXRLayerId)],
        ) -> Result<Vec<WebXRSubImages>, WebXRError> {
            *self.frame_layers.borrow_mut() = layers.to_vec();
            Ok(vec![])
        }
        fn end_frame(
            &mut self,
            _: &mut dyn WebXRContexts<WebXRSurfman>,
            layers: &[(WebXRContextId, WebXRLayerId)],
        ) -> Result<(), WebXRError> {
            *self.frame_layers.borrow_mut() = layers.to_vec();
            Ok(())
        }
    }
    #[test]
    fn retirement_keeps_a_manager_that_still_owns_layers() {
        let (sender, _) = webgl_channel().unwrap();
        let mut bridge = WebXRBridge::new(WebXRBridgeInit::new(sender));
        let id = WebXRLayerManagerId::new(NonZeroU32::MIN);
        let dropped = Rc::new(std::cell::Cell::new(0));
        bridge.managers.insert(
            id,
            Box::new(RetainedManager {
                layers: vec![(WebXRContextId(1), WebXRLayerId::default())],
                ready: Rc::new(std::cell::Cell::new(false)),
                dropped: dropped.clone(),
                frame_layers: Rc::default(),
            }),
        );
        bridge.destroy_layer_manager(id);
        assert_eq!(
            dropped.get(),
            0,
            "manager with an unreleased native layer was dropped"
        );
        assert!(bridge.managers.contains_key(&id));
    }
    #[test]
    fn retirement_excludes_pending_layers_from_frame_use() {
        let (sender, _) = webgl_channel().unwrap();
        let mut bridge = WebXRBridge::new(WebXRBridgeInit::new(sender));
        let id = WebXRLayerManagerId::new(NonZeroU32::MIN);
        let ids = (WebXRContextId(1), WebXRLayerId::default());
        let frame_layers = Rc::default();
        bridge.managers.insert(
            id,
            Box::new(RetainedManager {
                layers: vec![ids],
                ready: Rc::new(std::cell::Cell::new(false)),
                dropped: Rc::default(),
                frame_layers: Rc::clone(&frame_layers),
            }),
        );
        bridge.destroy_layer(id, &mut NoContexts, ids.0, ids.1);
        bridge.begin_frame(id, &mut NoContexts, &[ids]).unwrap();
        assert!(
            frame_layers.borrow().is_empty(),
            "partially retired layer was reused for a frame"
        );
    }
    #[test]
    fn retirement_retries_closing_manager_until_native_owners_are_released() {
        let (sender, _) = webgl_channel().unwrap();
        let mut bridge = WebXRBridge::new(WebXRBridgeInit::new(sender));
        let id = WebXRLayerManagerId::new(NonZeroU32::MIN);
        let ids = (WebXRContextId(1), WebXRLayerId::default());
        let ready = Rc::new(std::cell::Cell::new(false));
        let dropped = Rc::new(std::cell::Cell::new(0));
        bridge.managers.insert(
            id,
            Box::new(RetainedManager {
                layers: vec![ids],
                ready: ready.clone(),
                dropped: dropped.clone(),
                frame_layers: Rc::default(),
            }),
        );
        bridge.destroy_layer_manager(id);
        bridge.destroy_layer_manager(id);
        assert_eq!(bridge.pending_layers.len(), 1);
        assert!(bridge.begin_frame(id, &mut NoContexts, &[ids]).is_err());
        assert!(bridge.end_frame(id, &mut NoContexts, &[ids]).is_err());
        for _ in 0..3 {
            bridge.retry_retirement(&mut NoContexts);
            assert!(bridge.has_pending_retirement());
            assert!(bridge.has_context_layers(ids.0));
            assert_eq!(dropped.get(), 0);
        }
        ready.set(true);
        bridge.retry_retirement(&mut NoContexts);
        assert!(!bridge.has_pending_retirement());
        assert!(!bridge.has_context_layers(ids.0));
        assert!(!bridge.managers.contains_key(&id));
        assert_eq!(dropped.get(), 1);
        bridge.destroy_layer_manager(id);
        bridge.retry_retirement(&mut NoContexts);
        assert_eq!(dropped.get(), 1);
    }
    #[test]
    fn retirement_context_drain_reports_retained_layers_and_preserves_other_context() {
        let (sender, _) = webgl_channel().unwrap();
        let mut bridge = WebXRBridge::new(WebXRBridgeInit::new(sender));
        let id = WebXRLayerManagerId::new(NonZeroU32::MIN);
        let first = (WebXRContextId(1), WebXRLayerId::default());
        let other = (WebXRContextId(2), WebXRLayerId::default());
        let ready = Rc::new(std::cell::Cell::new(false));
        let dropped = Rc::new(std::cell::Cell::new(0));
        let frames = Rc::default();
        bridge.managers.insert(
            id,
            Box::new(RetainedManager {
                layers: vec![first, other],
                ready: ready.clone(),
                dropped: dropped.clone(),
                frame_layers: Rc::clone(&frames),
            }),
        );
        assert!(!bridge.destroy_all_layers(&mut NoContexts, first.0));
        bridge
            .begin_frame(id, &mut NoContexts, &[first, other])
            .unwrap();
        assert_eq!(*frames.borrow(), [other]);
        bridge
            .end_frame(id, &mut NoContexts, &[first, other])
            .unwrap();
        assert_eq!(*frames.borrow(), [other]);
        ready.set(true);
        bridge.retry_retirement(&mut NoContexts);
        assert!(!bridge.has_context_layers(first.0));
        assert!(bridge.has_context_layers(other.0));
        assert_eq!(dropped.get(), 0);
        assert!(bridge.destroy_all_layers(&mut NoContexts, other.0));
        bridge.destroy_layer_manager(id);
        assert_eq!(dropped.get(), 1);
        assert!(!bridge.has_pending_retirement());
    }
    #[test]
    fn retirement_empty_and_unknown_managers_need_no_retry() {
        let (sender, _) = webgl_channel().unwrap();
        let mut bridge = WebXRBridge::new(WebXRBridgeInit::new(sender));
        let id = WebXRLayerManagerId::new(NonZeroU32::MIN);
        let dropped = Rc::new(std::cell::Cell::new(0));
        bridge.destroy_layer_manager(id);
        bridge.managers.insert(
            id,
            Box::new(RetainedManager {
                layers: vec![],
                ready: Rc::default(),
                dropped: dropped.clone(),
                frame_layers: Rc::default(),
            }),
        );
        bridge.destroy_layer_manager(id);
        assert_eq!(dropped.get(), 1);
        assert!(!bridge.has_pending_retirement());
    }
}
