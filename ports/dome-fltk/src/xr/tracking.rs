// SPDX-License-Identifier: GPL-3.0-or-later
//! Keep meaningful head components and viewer-relative calibration independently.
use super::*;
use webxr_api::{ApiSpace, ViewDescription, Viewer, ViewerViews};

const ORIENTATION_VALID: u64 = 1;
const POSITION_VALID: u64 = 2;
const POSITION_TRACKED: u64 = 8;

#[derive(Clone, Default)]
pub(super) struct State {
    position: Option<(i64, Vector3D<f32, Native>)>,
    last_pose: Option<(i64, RigidTransform3D<f32, Viewer, Native>)>,
    views: Option<ViewerViews>,
}

pub(super) struct Sample {
    pub pose: Option<ViewerPose>,
    pub untracked_views: Option<ViewerViews>,
    /// Uses the same meaningful transform as force-emulated DOM viewer queries.
    pub presentation: Option<ViewerPose>,
}

fn native_views(views: &ViewerViews, viewer: RigidTransform3D<f32, Viewer, Native>) -> Views {
    match views {
        ViewerViews::Inline => Views::Inline,
        ViewerViews::List(views) => Views::List(
            views
                .iter()
                .map(|view| ViewDescription {
                    id: view.id,
                    eye: view.eye,
                    is_first_person_observer: view.is_first_person_observer,
                    view: View {
                        transform: view.transform.then(&viewer),
                        projection: view.projection,
                    },
                })
                .collect(),
        ),
    }
}

/// Export calibrated projection poses in the host's stage coordinates.
pub(super) fn projection_eyes(
    pose: &ViewerPose,
    anchor: RigidTransform3D<f32, Native, Native>,
    fovs: &[Eye; 2],
) -> Option<[Eye; 2]> {
    let Views::List(views) = &pose.views else {
        return None;
    };
    if views.len() != 2 {
        return None;
    }
    let eye = |index: usize| {
        let transform = views[index].view.transform.then(&anchor);
        let q = transform.rotation;
        Eye {
            pose: Pose {
                position: [
                    transform.translation.x,
                    transform.translation.y,
                    transform.translation.z,
                ],
                orientation: [q.i, q.j, q.k, q.r],
            },
            fov: fovs[index].fov.clone(),
        }
    };
    let result = [eye(0), eye(1)];
    result
        .iter()
        .all(|eye| eye.pose.valid() && eye.fov.valid())
        .then_some(result)
}

impl State {
    /// A historical input batch cannot start from a later display prediction.
    /// Keep static calibration while dropping world estimates from the future.
    pub fn retain_through(&mut self, time: i64) {
        self.position = self.position.filter(|(observed, _)| *observed <= time);
        self.last_pose = self.last_pose.filter(|(observed, _)| *observed <= time);
    }
    pub fn reset(&mut self, new_to_old: Option<RigidTransform3D<f32, Native, Native>>) {
        let Some(new_to_old) = new_to_old else {
            self.position = None;
            self.last_pose = None;
            return;
        };
        let old_to_new = new_to_old.inverse();
        self.position = self.position.map(|(time, position)| {
            (
                time,
                old_to_new.rotation.transform_vector3d(position) + old_to_new.translation,
            )
        });
        self.last_pose = self
            .last_pose
            .map(|(time, pose)| (time, pose.then(&old_to_new)));
    }

    pub fn sample(
        &mut self,
        host: &HostFrame,
        anchor: RigidTransform3D<f32, Native, Native>,
        clip: ClipPlanes,
    ) -> Sample {
        if !host.visible {
            return Sample {
                pose: None,
                untracked_views: None,
                presentation: None,
            };
        }
        let local = anchor.inverse();
        let observed_position = (host.head_flags & POSITION_VALID != 0
            && host.head.valid_components(2))
        .then(|| {
            let p = host.head.position;
            local
                .rotation
                .transform_vector3d(Vector3D::new(p[0], p[1], p[2]))
                + local.translation
        })
        .filter(|p| p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
        let position_valid = observed_position.is_some();
        let orientation_valid =
            host.head_flags & ORIENTATION_VALID != 0 && host.head.valid_components(1);
        if let Some(position) = observed_position {
            if self.position.is_none_or(|(time, _)| host.time >= time) {
                self.position = Some((host.time, position));
            }
        }
        // Historical input frames may precede the latest display prediction.
        // Only estimates known at or before this frame's time are meaningful.
        let position = observed_position
            .or_else(|| {
                self.position
                    .filter(|(time, _)| *time <= host.time)
                    .map(|(_, position)| position)
            })
            .or_else(|| {
                self.last_pose
                    .filter(|(time, _)| *time <= host.time)
                    .map(|(_, pose)| pose.translation)
            });
        // Only complete, flagged head and eye poses can establish calibration.
        // Unflagged components may contain placeholders and must not be read.
        if position_valid
            && orientation_valid
            && host.view_flags & 3 == 3
            && host.views.iter().all(|eye| eye.pose.valid())
        {
            let raw_head = host.head.transform::<Viewer, Native>().then(&local);
            let descriptions = host
                .views
                .iter()
                .enumerate()
                .map(|(index, eye)| ViewDescription {
                    id: index as u64,
                    eye: if index == 0 {
                        webxr_api::ViewEye::Left
                    } else {
                        webxr_api::ViewEye::Right
                    },
                    is_first_person_observer: false,
                    view: View {
                        transform: eye.pose.transform::<ApiSpace, Native>().then(&local),
                        projection: fov_to_projection_matrix(
                            eye.fov.left,
                            eye.fov.right,
                            eye.fov.up,
                            eye.fov.down,
                            clip,
                        ),
                    },
                })
                .collect();
            self.views = Some(Views::List(descriptions).relative_to_viewer(raw_head));
        }
        if let Some(ViewerViews::List(views)) = &mut self.views {
            for (view, eye) in views.iter_mut().zip(&host.views) {
                view.projection = fov_to_projection_matrix(
                    eye.fov.left,
                    eye.fov.right,
                    eye.fov.up,
                    eye.fov.down,
                    clip,
                );
            }
        }
        let transform = if orientation_valid {
            position.map(|position| {
                let q = host.head.orientation;
                let orientation =
                    Rotation3D::<f32, Viewer, Native>::quaternion(q[0], q[1], q[2], q[3])
                        .normalize()
                        .then(&local.rotation);
                RigidTransform3D::new(orientation, position)
            })
        } else {
            None
        };
        let pose = transform
            .zip(self.views.as_ref())
            .map(|(transform, views)| {
                if self.last_pose.is_none_or(|(time, _)| host.time >= time) {
                    self.last_pose = Some((host.time, transform));
                }
                ViewerPose {
                    transform,
                    views: native_views(views, transform),
                    emulated_position: !position_valid || host.head_flags & POSITION_TRACKED == 0,
                }
            });
        let presentation = pose.clone().or_else(|| {
            self.last_pose
                .filter(|(time, _)| *time <= host.time)
                .zip(self.views.as_ref())
                .map(|((_, transform), views)| ViewerPose {
                    transform,
                    views: native_views(views, transform),
                    emulated_position: true,
                })
        });
        Sample {
            untracked_views: if pose.is_none() {
                self.views.clone()
            } else {
                None
            },
            pose,
            presentation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn host() -> HostFrame {
        serde_json::from_value(serde_json::json!({
            "frame_id":1,"time":1,"should_render":true,"visible":true,"focused":true,
            "head_flags":15,"view_flags":15,
            "head":{"position":[1.,1.6,2.],"orientation":[0.,0.,0.,1.]},
            "views":[
                {"pose":{"position":[0.97,1.6,2.],"orientation":[0.,0.,0.,1.]},"fov":{"left":-0.7,"right":0.7,"up":0.7,"down":-0.7}},
                {"pose":{"position":[1.03,1.6,2.],"orientation":[0.,0.,0.,1.]},"fov":{"left":-0.7,"right":0.7,"up":0.7,"down":-0.7}}
            ]
        })).unwrap()
    }
    fn setup() -> (State, HostFrame, RigidTransform3D<f32, Native, Native>) {
        let host = host();
        let anchor = anchor(&host.head);
        let mut state = State::default();
        assert!(
            !state
                .sample(&host, anchor, ClipPlanes::default())
                .pose
                .unwrap()
                .emulated_position
        );
        (state, host, anchor)
    }
    #[test]
    fn head_tracking_advances_without_reading_unflagged_eye_components() {
        let (mut state, mut host, anchor) = setup();
        host.head.position[0] = 2.;
        host.view_flags = 0;
        host.views[0].pose.position = [f32::NAN; 3];
        let pose = state
            .sample(&host, anchor, ClipPlanes::default())
            .pose
            .unwrap();
        assert!((pose.transform.translation.x - 1.).abs() < 1e-5);
        assert!(!pose.emulated_position);
        let Views::List(views) = pose.views else {
            panic!("stereo list required")
        };
        assert!((views[0].view.transform.translation.x - 0.97).abs() < 1e-5);
    }
    #[test]
    fn valid_but_untracked_position_is_an_estimate() {
        let (mut state, mut host, anchor) = setup();
        host.head_flags = 7;
        assert!(
            state
                .sample(&host, anchor, ClipPlanes::default())
                .pose
                .unwrap()
                .emulated_position
        );
    }
    #[test]
    fn orientation_only_updates_rotation_without_reading_unflagged_position() {
        let (mut state, mut host, anchor) = setup();
        host.head_flags = 5;
        host.view_flags = 0;
        host.head.position = [f32::NAN; 3];
        host.head.orientation = [
            0.,
            std::f32::consts::FRAC_1_SQRT_2,
            0.,
            std::f32::consts::FRAC_1_SQRT_2,
        ];
        let sample = state.sample(&host, anchor, ClipPlanes::default());
        let pose = sample.pose.unwrap();
        assert!(pose.emulated_position);
        assert!(pose.transform.translation.length() < 1e-5);
        let direction = pose
            .transform
            .rotation
            .transform_vector3d(Vector3D::new(1., 0., 0.));
        assert!((direction.z + 1.).abs() < 1e-5);
        let Views::List(views) = pose.views else {
            panic!("stereo list required")
        };
        assert!((views[0].view.transform.translation.z - 0.03).abs() < 1e-5);
        assert!(sample.presentation.unwrap().emulated_position);
    }
    #[test]
    fn projection_export_uses_the_same_estimate_in_host_stage_coordinates() {
        let (mut state, mut host, anchor) = setup();
        host.head_flags = 5;
        host.view_flags = 0;
        host.head.position = [f32::NAN; 3];
        host.head.orientation = [
            0.,
            std::f32::consts::FRAC_1_SQRT_2,
            0.,
            std::f32::consts::FRAC_1_SQRT_2,
        ];
        let sample = state.sample(&host, anchor, ClipPlanes::default());
        let eyes = projection_eyes(&sample.presentation.unwrap(), anchor, &host.views).unwrap();
        assert!((eyes[0].pose.position[0] - 1.).abs() < 1e-5);
        assert!((eyes[0].pose.position[2] - 2.03).abs() < 1e-5);
        assert!((eyes[1].pose.position[2] - 1.97).abs() < 1e-5);
        assert!(eyes.iter().all(|eye| eye.pose.valid()));
    }
    #[test]
    fn total_loss_preserves_presentation_but_does_not_invent_an_ordinary_pose() {
        let (mut state, mut host, anchor) = setup();
        host.head_flags = 0;
        host.view_flags = 0;
        host.head.position = [f32::NAN; 3];
        host.head.orientation = [0.; 4];
        let sample = state.sample(&host, anchor, ClipPlanes::default());
        assert!(sample.pose.is_none());
        assert!(sample.untracked_views.is_some());
        assert!(sample.presentation.unwrap().emulated_position);
    }
    #[test]
    fn unknown_origin_continuity_clears_world_history_but_retains_calibration() {
        let (mut state, mut host, anchor) = setup();
        state.reset(None);
        host.head_flags = 5;
        host.view_flags = 0;
        let sample = state.sample(&host, anchor, ClipPlanes::default());
        assert!(sample.pose.is_none());
        assert!(sample.presentation.is_none());
        assert!(sample.untracked_views.is_some());
    }
    #[test]
    fn known_origin_delta_rebases_estimated_position() {
        let (mut state, mut host, anchor) = setup();
        state.reset(Some(RigidTransform3D::from_translation(Vector3D::new(
            1., 2., 3.,
        ))));
        host.head_flags = 5;
        host.view_flags = 0;
        let pose = state
            .sample(&host, anchor, ClipPlanes::default())
            .pose
            .unwrap();
        assert!((pose.transform.translation - Vector3D::new(-1., -2., -3.)).length() < 1e-5);
    }
    #[test]
    fn hidden_samples_do_not_advance_visible_pose_history() {
        let (mut state, mut host, anchor) = setup();
        host.visible = false;
        host.head.position[0] = 99.;
        assert!(
            state
                .sample(&host, anchor, ClipPlanes::default())
                .presentation
                .is_none()
        );
        host.visible = true;
        host.head_flags = 5;
        host.view_flags = 0;
        assert!(
            state
                .sample(&host, anchor, ClipPlanes::default())
                .pose
                .unwrap()
                .transform
                .translation
                .length()
                < 1e-5
        );
    }
    #[test]
    fn projection_updates_during_loss_and_returned_snapshots_stay_independent() {
        let (mut state, mut host, anchor) = setup();
        let old = state
            .sample(&host, anchor, ClipPlanes::default())
            .pose
            .unwrap();
        host.head_flags = 0;
        host.view_flags = 0;
        let mut clip = ClipPlanes::default();
        clip.update(0.25, 50.);
        let sample = state.sample(&host, anchor, clip);
        let Views::List(views) = sample.presentation.unwrap().views else {
            panic!("stereo list required")
        };
        let Views::List(old_views) = old.views else {
            panic!("stereo list required")
        };
        assert!(
            (views[0].view.projection.to_array()[14] - (-2. * 50. * 0.25 / (50. - 0.25))).abs()
                < 1e-5
        );
        assert_ne!(views[0].view.projection, old_views[0].view.projection);
    }
    #[test]
    fn an_older_input_frame_cannot_use_a_future_position_estimate() {
        let (mut state, mut host, anchor) = setup();
        host.time = 20;
        state.sample(&host, anchor, ClipPlanes::default());
        host.time = 10;
        host.head_flags = 5;
        host.view_flags = 0;
        host.head.position = [f32::NAN; 3];
        let snapshot = state.sample(&host, anchor, ClipPlanes::default());
        assert!(snapshot.pose.is_none());
        assert!(snapshot.presentation.is_none());
        assert!(snapshot.untracked_views.is_some());
    }
    #[test]
    fn an_old_real_pose_is_reportable_without_replacing_newer_history() {
        let (mut state, mut host, anchor) = setup();
        host.time = 20;
        state.sample(&host, anchor, ClipPlanes::default());
        host.time = 10;
        host.head.position[0] = 5.;
        host.views[0].pose.position[0] = 4.97;
        host.views[1].pose.position[0] = 5.03;
        let old = state
            .sample(&host, anchor, ClipPlanes::default())
            .pose
            .unwrap();
        assert!((old.transform.translation.x - 4.).abs() < 1e-5);
        host.time = 30;
        host.head_flags = 5;
        host.view_flags = 0;
        host.head.position = [f32::NAN; 3];
        let current = state
            .sample(&host, anchor, ClipPlanes::default())
            .pose
            .unwrap();
        assert!(current.transform.translation.length() < 1e-5);
        assert!(current.emulated_position);
    }
    #[test]
    fn historical_estimates_advance_inside_their_own_chronology() {
        let (mut current, mut host, anchor) = setup();
        host.time = 20;
        current.sample(&host, anchor, ClipPlanes::default());
        let mut history = current.clone();
        history.retain_through(10);
        host.time = 10;
        host.head.position[0] = 5.;
        host.views[0].pose.position[0] = 4.97;
        host.views[1].pose.position[0] = 5.03;
        history.sample(&host, anchor, ClipPlanes::default());
        host.time = 15;
        host.head_flags = 5;
        host.view_flags = 0;
        host.head.position = [f32::NAN; 3];
        history.retain_through(host.time);
        let action = history
            .sample(&host, anchor, ClipPlanes::default())
            .pose
            .unwrap();
        assert!((action.transform.translation.x - 4.).abs() < 1e-5);
        assert!(action.emulated_position);
        host.time = 30;
        let display = current
            .sample(&host, anchor, ClipPlanes::default())
            .pose
            .unwrap();
        assert!(display.transform.translation.length() < 1e-5);
    }
}
