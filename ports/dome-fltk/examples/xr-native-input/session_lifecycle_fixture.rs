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
        let nominal_rate = match std::env::var("XR_INPUT_NOMINAL_RATE") {
            Ok(value) => value.parse::<f32>().ok(),
            Err(_) => Some(120.),
        };
        let rates = std::env::var("XR_INPUT_SUPPORTED_RATES").ok()
            .map(|value| value.split(',').filter_map(|part| part.parse::<f32>().ok()).collect())
            .unwrap_or_else(|| vec![72., 90., 120.]);
        let response = std::env::var("XR_INPUT_FRAME_RATE_RESPONSE").ok()
            .and_then(|value| value.parse::<f32>().ok());
        builder.spawn(move |_| {
            Ok(Device {
                features,
                events: EventBuffer::default(),
                ended: false,
                nominal_rate,
                rates,
                response,
            })
        })
    }
}
struct Device {
    features: Vec<String>,
    events: EventBuffer,
    ended: bool,
    nominal_rate: Option<f32>,
    rates: Vec<f32>,
    response: Option<f32>,
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
        self.rates.clone()
    }
    fn nominal_frame_rate(&self) -> Option<f32> {
        self.nominal_rate
    }
    fn update_frame_rate(&mut self, rate: f32) -> f32 {
        // Make a response cross the calling script's shutdown boundary reliably.
        std::thread::sleep(Duration::from_millis(30));
        let actual = self.response.unwrap_or(rate);
        if webxr_api::util::valid_frame_rate(actual) {
            self.nominal_rate = Some(actual);
        }
        actual
    }
    fn request_hit_test(&mut self, _: HitTestSource) {
        // Deliberately pending platform operation, used to verify shutdown rejection.
    }
    fn cancel_hit_test(&mut self, _: HitTestId) {}
}
