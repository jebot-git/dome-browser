// SPDX-License-Identifier: GPL-3.0-or-later
//! Convert cumulative host-native origin changes into frame-scoped WebXR resets.
use super::*;
use webxr_api::{ApiSpace, BaseSpace, FrameUpdateEvent};

#[derive(Clone, Copy, Debug, Deserialize)]
pub(super) struct State {
    version: u32,
    revision: u64,
    epoch: u64,
    origin: Pose,
}
impl State {
    pub(super) fn valid(&self) -> bool {
        self.version == 1 && self.epoch <= self.revision && self.origin.valid()
    }
}
#[derive(Default)]
pub(super) struct Tracker {
    previous: Option<State>,
}
impl Tracker {
    pub fn new(previous: Option<State>) -> Self { Self { previous } }
    pub fn frame_events(&mut self, next: Option<State>, anchor: RigidTransform3D<f32, Native, Native>,
        native_floor: bool) -> Result<Vec<FrameUpdateEvent>> {
        let delta = match (self.previous, next) {
            (None, None) => return Ok(vec![]),
            (Some(_), None) => return Err("Reference-space state disappeared".into()),
            (_, Some(state)) if !state.valid() => return Err("Invalid reference-space state".into()),
            (None, Some(state)) => {
                self.previous = Some(state);
                if state.revision == 0 { return Ok(vec![]); }
                None
            },
            (Some(old), Some(state)) => {
                if state.revision < old.revision || state.epoch < old.epoch {
                    return Err("Reference-space state moved backwards".into());
                }
                if state.revision == old.revision {
                    let position_same = old.origin.position.iter().zip(state.origin.position)
                        .all(|(a,b)| (*a-b).abs() < 1e-5);
                    let dot = old.origin.orientation.iter().zip(state.origin.orientation).map(|(a,b)| a*b).sum::<f32>();
                    if state.epoch != old.epoch || !position_same || (dot.abs()-1.).abs() > 0.01 {
                        return Err("Reference-space origin changed without a revision".into());
                    }
                    return Ok(vec![]);
                }
                self.previous = Some(state);
                if state.epoch != old.epoch { None } else {
                    Some(state.origin.transform::<ApiSpace, ApiSpace>()
                        .then(&old.origin.transform::<ApiSpace, ApiSpace>().inverse()))
                }
            },
        };
        let local = delta.map(|delta| webxr_api::util::offset_reference_space_reset(anchor.cast_unit(), delta));
        let mut events = vec![FrameUpdateEvent::ReferenceSpaceChanged(BaseSpace::Local, local)];
        let floor = webxr_api::util::local_floor_transform(native_floor.then(|| {
            RigidTransform3D::from_translation(Vector3D::new(0., anchor.translation.y, 0.))
        }));
        let offset = floor.inverse().cast_unit();
        events.push(FrameUpdateEvent::ReferenceSpaceChanged(BaseSpace::Floor,
            local.map(|delta| webxr_api::util::offset_reference_space_reset(offset, delta))));
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(revision: u64, epoch: u64, x: f32) -> State {
        State { version:1, revision, epoch, origin:Pose { position:[x,0.,0.], orientation:[0.,0.,0.,1.] } }
    }
    #[test]
    fn skipped_host_frames_use_the_total_delta_and_do_not_repeat_resets() {
        let mut tracker = Tracker::new(Some(state(1,0,2.)));
        let anchor = RigidTransform3D::identity();
        let events = tracker.frame_events(Some(state(4,0,7.)),anchor,true).unwrap();
        assert_eq!(events.len(),2);
        for event in events {
            let FrameUpdateEvent::ReferenceSpaceChanged(_,Some(delta))=event else {panic!("known delta expected")};
            assert!((delta.translation.x-5.).abs()<1e-5);
        }
        assert!(tracker.frame_events(Some(state(4,0,7.)),anchor,true).unwrap().is_empty());
    }
    #[test]
    fn lost_continuity_is_null_and_replays_or_missing_extension_are_rejected() {
        let mut tracker = Tracker::new(Some(state(1,0,2.)));
        let events = tracker.frame_events(Some(state(2,1,0.)),RigidTransform3D::identity(),false).unwrap();
        assert!(matches!(events.as_slice(),[
            FrameUpdateEvent::ReferenceSpaceChanged(BaseSpace::Local,None),
            FrameUpdateEvent::ReferenceSpaceChanged(BaseSpace::Floor,None)
        ]));
        assert!(tracker.frame_events(Some(state(1,0,2.)),RigidTransform3D::identity(),false).is_err());
        assert!(tracker.frame_events(None,RigidTransform3D::identity(),false).is_err());
    }
    #[test]
    fn legacy_hosts_remain_usable_and_first_snapshot_is_only_a_baseline() {
        let mut tracker = Tracker::new(None);
        assert!(tracker.frame_events(None,RigidTransform3D::identity(),false).unwrap().is_empty());
        let mut tracker = Tracker::new(Some(state(40,3,2.)));
        assert!(tracker.frame_events(Some(state(40,3,2.)),RigidTransform3D::identity(),false).unwrap().is_empty());
    }

    #[test]
    fn estimated_floor_reset_uses_the_default_height_instead_of_native_anchor_height() {
        let mut tracker = Tracker::new(Some(state(0,0,0.)));
        let mut next = state(1,0,0.);
        next.origin.orientation = [std::f32::consts::FRAC_1_SQRT_2, 0., 0., std::f32::consts::FRAC_1_SQRT_2];
        let anchor = RigidTransform3D::from_translation(Vector3D::new(0., 2.2, 0.));
        let events = tracker.frame_events(Some(next), anchor, false).unwrap();
        let FrameUpdateEvent::ReferenceSpaceChanged(BaseSpace::Floor, Some(delta)) = events[1] else {
            panic!("Estimated floor reset expected")
        };
        assert!((delta.translation.y + 0.6).abs() < 1e-5);
        assert!((delta.translation.z - 0.6).abs() < 1e-5);
        assert!(delta.translation.x.abs() < 1e-5);
    }
}
