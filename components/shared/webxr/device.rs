/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Traits to be implemented by backends

use euclid::{Point2D, RigidTransform3D};
use profile_traits::generic_callback::GenericCallback as ProfileGenericCallback;

use crate::{
    ContextId, EnvironmentBlendMode, Error, Event, Floor, Frame, HitTestId, HitTestSource,
    InputSource, LayerGrandManager, LayerId, LayerInfo, LayerInit, Native, Quitter, Session,
    SessionBuilder, SessionId, SessionInit, SessionMode, Viewports,
};

/// A trait for discovering XR devices
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImmersiveDeviceState {
    /// Backend-local identity; a runtime restart or replacement changes it.
    pub identity: u64,
    pub supports_vr: bool,
    pub supports_ar: bool,
}

pub trait DiscoveryAPI<GL>: 'static {
    fn request_session(
        &mut self,
        mode: SessionMode,
        init: &SessionInit,
        xr: SessionBuilder<GL>,
    ) -> Result<Session, Error>;
    fn supports_session(&self, mode: SessionMode) -> bool;

    /// Prepare consent asynchronously without creating a session or holding up
    /// the browser event loop. False leaves this request pending for a later poll.
    fn prepare_session(
        &mut self,
        _id: SessionId,
        _mode: SessionMode,
        _init: &SessionInit,
    ) -> Result<bool, Error> {
        Ok(true)
    }

    /// Release any UI/resources for a pending preparation. Also called after
    /// successful creation so backend preparation state never outlives a request.
    fn cancel_session_preparation(&mut self, _id: SessionId) {}

    fn immersive_device_state(&self) -> Option<ImmersiveDeviceState> {
        let supports_vr = self.supports_session(SessionMode::ImmersiveVR);
        let supports_ar = self.supports_session(SessionMode::ImmersiveAR);
        (supports_vr || supports_ar).then_some(ImmersiveDeviceState {
            identity: 0,
            supports_vr,
            supports_ar,
        })
    }

    /// Observe changes to immersive-device availability without starting a
    /// session. None stops observation; implementations must not retain a DOM
    /// object or perform tracking in order to answer availability.
    fn set_device_change_callback(&mut self, _callback: Option<ProfileGenericCallback<()>>) {}

    /// Verify that a context can supply surfaces to this immersive device.
    /// This must not begin a session, consume a lease or request tracking data.
    fn check_context_compatibility(
        &self,
        _context: ContextId,
        _manager: &LayerGrandManager<GL>,
    ) -> Result<(), Error> {
        Err(Error::NoMatchingDevice)
    }
}

/// A trait for using an XR device
pub trait DeviceAPI: 'static {
    /// Create a new layer
    fn create_layer(&mut self, context_id: ContextId, init: LayerInit) -> Result<LayerInfo, Error>;

    /// Destroy a layer
    fn destroy_layer(&mut self, context_id: ContextId, layer_id: LayerId);

    /// The transform from native coordinates to the floor.
    fn floor_transform(&self) -> Option<RigidTransform3D<f32, Native, Floor>>;

    fn viewports(&self) -> Viewports;

    /// Whether the compositor can consume independently scaled per-view rectangles.
    fn supports_dynamic_viewport_scaling(&self) -> bool {
        false
    }

    /// Rectangles committed by the application for the current frame. This does
    /// not resize the allocation or change the device's full-sized viewports.
    fn set_layer_viewports(&mut self, _context: ContextId, _layer: LayerId, _viewports: Viewports) {
    }

    /// Begin an animation frame.
    fn begin_animation_frame(&mut self, layers: &[(ContextId, LayerId)]) -> Option<Frame>;

    /// End an animation frame, render the layer to the device, and block waiting for the next frame.
    fn end_animation_frame(&mut self, layers: &[(ContextId, LayerId)]);

    /// Inputs registered with the device on initialization. More may be added, which
    /// should be communicated through a yet-undecided event mechanism
    fn initial_inputs(&self) -> Vec<InputSource>;

    /// Sets the event handling channel
    fn set_event_dest(&mut self, dest: ProfileGenericCallback<Event>);

    /// Quit the session
    fn quit(&mut self);

    /// Poll teardown on the owning thread after quit. False retains this device
    /// and its context for retry; implementations must not start new rendering.
    fn poll_cleanup(&mut self) -> bool { true }

    fn set_quitter(&mut self, quitter: Quitter);

    fn update_clip_planes(&mut self, near: f32, far: f32);

    fn environment_blend_mode(&self) -> EnvironmentBlendMode {
        // for VR devices, override for AR
        EnvironmentBlendMode::Opaque
    }

    fn granted_features(&self) -> &[String];

    fn request_hit_test(&mut self, _source: HitTestSource) {
        panic!("This device does not support requesting hit tests");
    }

    fn cancel_hit_test(&mut self, _id: HitTestId) {
        panic!("This device does not support hit tests");
    }

    fn update_frame_rate(&mut self, rate: f32) -> f32 {
        rate
    }

    fn supported_frame_rates(&self) -> Vec<f32> {
        Vec::new()
    }

    fn reference_space_bounds(&self) -> Option<Vec<Point2D<f32, Floor>>> {
        None
    }
}

impl<GL: 'static> DiscoveryAPI<GL> for Box<dyn DiscoveryAPI<GL>> {
    fn prepare_session(
        &mut self,
        id: SessionId,
        mode: SessionMode,
        init: &SessionInit,
    ) -> Result<bool, Error> {
        (**self).prepare_session(id, mode, init)
    }
    fn cancel_session_preparation(&mut self, id: SessionId) {
        (**self).cancel_session_preparation(id);
    }

    fn immersive_device_state(&self) -> Option<ImmersiveDeviceState> {
        (**self).immersive_device_state()
    }
    fn set_device_change_callback(&mut self, callback: Option<ProfileGenericCallback<()>>) {
        (**self).set_device_change_callback(callback);
    }

    fn check_context_compatibility(
        &self,
        context: ContextId,
        manager: &LayerGrandManager<GL>,
    ) -> Result<(), Error> {
        (**self).check_context_compatibility(context, manager)
    }
    fn request_session(
        &mut self,
        mode: SessionMode,
        init: &SessionInit,
        xr: SessionBuilder<GL>,
    ) -> Result<Session, Error> {
        (**self).request_session(mode, init, xr)
    }

    fn supports_session(&self, mode: SessionMode) -> bool {
        (**self).supports_session(mode)
    }
}
