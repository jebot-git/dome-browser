// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in logical device for promise/lifetime tests; no tracking or headset resources.
use super::*;
use webxr_api::{HitTestId, HitTestSource};

pub(super) struct Discovery;
impl DiscoveryAPI<SurfmanGL> for Discovery {
    fn supports_session(&self, mode: SessionMode) -> bool {
        mode == SessionMode::ImmersiveVR
    }
    fn request_session(
        &mut self,
        mode: SessionMode,
        init: &SessionInit,
        builder: SessionBuilder<SurfmanGL>,
    ) -> Result<Session, Error> {
        if !self.supports_session(mode) {
            return Err(Error::NoMatchingDevice);
        }
        let features = init.validate(mode, &["local".into(), "hit-test".into()])?;
        builder.spawn(move |_| {
            Ok(Device {
                features,
                events: EventBuffer::default(),
                ended: false,
            })
        })
    }
}
struct Device {
    features: Vec<String>,
    events: EventBuffer,
    ended: bool,
}
impl DeviceAPI for Device {
    fn create_layer(&mut self, _: ContextId, _: LayerInit) -> Result<LayerInfo, Error> {
        Err(backend(
            "This logical device does not allocate graphics layers",
        ))
    }
    fn destroy_layer(&mut self, _: ContextId, _: LayerId) {}
    fn floor_transform(&self) -> Option<RigidTransform3D<f32, Native, Floor>> {
        None
    }
    fn viewports(&self) -> Viewports {
        Viewports { viewports: vec![] }
    }
    fn begin_animation_frame(&mut self, _: &[(ContextId, LayerId)]) -> Option<Frame> {
        (!self.ended).then(|| Frame {
            pose: None,
            untracked_views: None,
            floor_transform: None,
            inputs: vec![],
            events: vec![],
            sub_images: vec![],
            hit_test_results: vec![],
            predicted_display_time: 0.,
            predicted_display_instant: None,
        })
    }
    fn end_animation_frame(&mut self, _: &[(ContextId, LayerId)]) {
        std::thread::sleep(Duration::from_millis(16));
    }
    fn initial_inputs(&self) -> Vec<InputSource> {
        vec![]
    }
    fn set_event_dest(
        &mut self,
        dest: servo::profile_traits::generic_callback::GenericCallback<webxr_api::Event>,
    ) {
        self.events.upgrade(dest);
    }
    fn quit(&mut self) {
        if !self.ended {
            self.ended = true;
            self.events.callback(webxr_api::Event::SessionEnd);
        }
    }
    fn set_quitter(&mut self, _: Quitter) {}
    fn update_clip_planes(&mut self, _: f32, _: f32) {}
    fn granted_features(&self) -> &[String] {
        &self.features
    }
    fn supported_frame_rates(&self) -> Vec<f32> {
        vec![72., 90., 120.]
    }
    fn update_frame_rate(&mut self, rate: f32) -> f32 {
        // Make a response cross the calling script's shutdown boundary reliably.
        std::thread::sleep(Duration::from_millis(30));
        rate
    }
    fn request_hit_test(&mut self, _: HitTestSource) {
        // Deliberately pending platform operation, used to verify shutdown rejection.
    }
    fn cancel_hit_test(&mut self, _: HitTestId) {}
}
