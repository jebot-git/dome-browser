// SPDX-License-Identifier: GPL-3.0-or-later
//! Sensor-free inline sessions use their existing HTML canvas, never an XR lease.
use super::*;
use std::time::Instant;

pub(super) struct Discovery;
impl DiscoveryAPI<SurfmanGL> for Discovery {
    fn supports_session(&self, mode: SessionMode) -> bool {
        mode == SessionMode::Inline
    }
    fn request_session(
        &mut self,
        mode: SessionMode,
        init: &SessionInit,
        builder: SessionBuilder<SurfmanGL>,
    ) -> std::result::Result<Session, Error> {
        if !self.supports_session(mode) {
            return Err(Error::NoMatchingDevice);
        }
        let features = init.validate(mode, &[])?;
        builder.spawn(move |_| Ok(Device::new(features)))
    }
}

struct Device {
    features: Vec<String>,
    events: EventBuffer,
    ended: bool,
    deadline: Instant,
}
impl Device {
    fn new(features: Vec<String>) -> Self {
        Self {
            features,
            events: EventBuffer::default(),
            ended: false,
            deadline: Instant::now(),
        }
    }
}
impl DeviceAPI for Device {
    fn create_layer(&mut self, _: ContextId, _: LayerInit) -> std::result::Result<LayerInfo, Error> {
        // Inline XRWebGLLayer uses the canvas's default framebuffer in Servo.
        Err(backend(
            "An inline session does not allocate a compositor layer",
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
        if self.ended {
            return None;
        }
        Some(Frame {
            pose: Some(ViewerPose {
                transform: RigidTransform3D::identity(),
                views: Views::Inline,
            }),
            inputs: vec![],
            events: vec![],
            sub_images: vec![],
            hit_test_results: vec![],
            predicted_display_time: 0.,
            predicted_display_instant: None,
        })
    }
    fn end_animation_frame(&mut self, _: &[(ContextId, LayerId)]) {
        std::thread::sleep(self.deadline.saturating_duration_since(Instant::now()));
        self.deadline = Instant::now() + Duration::from_nanos(1_000_000_000 / 60);
    }
    fn initial_inputs(&self) -> Vec<InputSource> {
        vec![]
    }
    fn set_event_dest(
        &mut self,
        dest: servo::profile_traits::generic_callback::GenericCallback<Event>,
    ) {
        self.events.upgrade(dest);
    }
    fn quit(&mut self) {
        if !self.ended {
            self.ended = true;
            self.events.callback(Event::SessionEnd);
        }
    }
    fn set_quitter(&mut self, _: Quitter) {}
    fn update_clip_planes(&mut self, _: f32, _: f32) {}
    fn granted_features(&self) -> &[String] {
        &self.features
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn virtual_viewer_frames_have_no_headset_inputs_or_compositor_resources() {
        let mut device = Device::new(vec!["viewer".into()]);
        let frame = device.begin_animation_frame(&[]).unwrap();
        assert!(matches!(frame.pose.unwrap().views, Views::Inline));
        assert!(frame.inputs.is_empty() && frame.sub_images.is_empty());
        assert!(device.viewports().viewports.is_empty());
        assert!(device.floor_transform().is_none());
        device.quit();
        assert!(device.begin_animation_frame(&[]).is_none());
    }
    #[test]
    fn inline_features_do_not_claim_sensor_or_immersive_capabilities() {
        let mut init = SessionInit {
            origin: "https://example.invalid".into(),
            webview_id: None,
            required_features: vec!["viewer".into()],
            optional_features: vec!["local-floor".into()],
            first_person_observer_view: false,
        };
        assert_eq!(init.validate(SessionMode::Inline, &[]).unwrap(), ["viewer"]);
        init.required_features.push("local".into());
        assert!(init.validate(SessionMode::Inline, &[]).is_err());
        assert!(Discovery.supports_session(SessionMode::Inline));
        assert!(!Discovery.supports_session(SessionMode::ImmersiveVR));
    }
}
