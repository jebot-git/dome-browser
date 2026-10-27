/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use euclid::{Angle, RigidTransform3D, Rotation3D, Transform3D, Vector3D};
use serde::{Deserialize, Serialize};

use crate::{FrameUpdateEvent, HitTestId, HitTestSource};

/// Express a native-origin reset in a reference space with an origin offset.
/// The transform maps the post-reset effective origin into pre-reset coordinates.
pub fn offset_reference_space_reset(
    offset: RigidTransform3D<f32, crate::ApiSpace, crate::ApiSpace>,
    delta: RigidTransform3D<f32, crate::ApiSpace, crate::ApiSpace>,
) -> RigidTransform3D<f32, crate::ApiSpace, crate::ApiSpace> {
    offset.then(&delta).then(&offset.inverse())
}

/// A screen interaction points from the viewer through the inline canvas.
/// Coordinates are fractions of its CSS content box, independent of backing
/// store resolution, borders, CSS transforms and device pixel ratio.
pub fn inline_pointer_ray(
    point: [f32; 2],
    vertical_fov: f32,
    aspect: f32,
) -> Option<RigidTransform3D<f32, crate::Input, crate::Viewer>> {
    if !point
        .iter()
        .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        || !vertical_fov.is_finite()
        || vertical_fov <= 0.0
        || vertical_fov >= std::f32::consts::PI
        || !aspect.is_finite()
        || aspect <= 0.0
    {
        return None;
    }
    let tangent = (vertical_fov * 0.5).tan();
    let direction: Vector3D<f32, crate::Viewer> = Vector3D::new(
        (point[0] * 2.0 - 1.0) * aspect * tangent,
        (1.0 - point[1] * 2.0) * tangent,
        -1.0,
    );
    let length = direction.length();
    if !length.is_finite() || length <= 0.0 {
        return None;
    }
    let direction = direction / length;
    let axis = Vector3D::new(direction.y, -direction.x, 0.0);
    let rotation = if axis.square_length() == 0.0 {
        Rotation3D::identity()
    } else {
        Rotation3D::around_axis(
            axis.normalize(),
            Angle::radians((-direction.z).clamp(-1.0, 1.0).acos()),
        )
    };
    Some(RigidTransform3D::new(rotation, Vector3D::zero()))
}

#[cfg(test)]
mod inline_pointer_tests {
    use super::*;
    #[test]
    fn screen_ray_matches_inline_projection_and_canvas_y_direction() {
        for (point, expected) in [
            ([0.5, 0.5], [0.0, 0.0, -1.0]),
            ([0.75, 0.5], [1.0, 0.0, -1.0]),
            ([0.5, 0.0], [0.0, 1.0, -1.0]),
            ([0.0, 1.0], [-2.0, -1.0, -1.0]),
        ] {
            let pose = inline_pointer_ray(point, std::f32::consts::FRAC_PI_2, 2.0).unwrap();
            let actual = pose
                .rotation
                .transform_vector3d(Vector3D::new(0.0, 0.0, -1.0));
            let expected: Vector3D<f32, crate::Viewer> =
                Vector3D::new(expected[0], expected[1], expected[2]).normalize();
            assert!((actual - expected).length() < 0.00001);
            assert_eq!(pose.translation, Vector3D::zero());
        }
    }
    #[test]
    fn invalid_or_outside_screen_geometry_has_no_ray() {
        for (point, fov, aspect) in [
            ([f32::NAN, 0.5], 1.0, 1.0),
            ([1.01, 0.5], 1.0, 1.0),
            ([0.5, -0.01], 1.0, 1.0),
            ([0.5, 0.5], 0.0, 1.0),
            ([0.5, 0.5], std::f32::consts::PI, 1.0),
            ([0.5, 0.5], 1.0, 0.0),
            ([0.5, 0.5], 1.0, f32::INFINITY),
        ] {
            assert!(inline_pointer_ray(point, fov, aspect).is_none());
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ClipPlanes {
    pub near: f32,
    pub far: f32,
    /// Was there an update that needs propagation to the client?
    update: bool,
}

impl Default for ClipPlanes {
    fn default() -> Self {
        ClipPlanes {
            near: 0.1,
            far: 1000.,
            update: false,
        }
    }
}

impl ClipPlanes {
    pub fn update(&mut self, near: f32, far: f32) {
        self.near = near;
        self.far = far;
        self.update = true;
    }

    /// Checks for and clears the pending update flag
    pub fn recently_updated(&mut self) -> bool {
        if self.update {
            self.update = false;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
/// Holds on to hit tests
pub struct HitTestList {
    tests: Vec<HitTestSource>,
    uncommitted_tests: Vec<HitTestSource>,
}

impl HitTestList {
    pub fn request_hit_test(&mut self, source: HitTestSource) {
        self.uncommitted_tests.push(source)
    }

    pub fn commit_tests(&mut self) -> Vec<FrameUpdateEvent> {
        let mut events = vec![];
        for test in self.uncommitted_tests.drain(..) {
            events.push(FrameUpdateEvent::HitTestSourceAdded(test.id));
            self.tests.push(test);
        }
        events
    }

    pub fn tests(&self) -> &[HitTestSource] {
        &self.tests
    }

    pub fn cancel_hit_test(&mut self, id: HitTestId) {
        self.tests.retain(|s| s.id != id);
        self.uncommitted_tests.retain(|s| s.id != id);
    }
}

#[inline]
/// Construct a projection matrix given the four angles from the center for the faces of the viewing frustum
pub fn fov_to_projection_matrix<T, U>(
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    clip_planes: ClipPlanes,
) -> Transform3D<f32, T, U> {
    let near = clip_planes.near;
    // XXXManishearth deal with infinite planes
    let left = left.tan() * near;
    let right = right.tan() * near;
    let top = top.tan() * near;
    let bottom = bottom.tan() * near;

    frustum_to_projection_matrix(left, right, top, bottom, clip_planes)
}

#[inline]
/// Construct matrix given the actual extent of the viewing frustum on the near plane
pub fn frustum_to_projection_matrix<T, U>(
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    clip_planes: ClipPlanes,
) -> Transform3D<f32, T, U> {
    let near = clip_planes.near;
    let far = clip_planes.far;

    let w = right - left;
    let h = top - bottom;
    let d = far - near;

    // Column-major order
    Transform3D::new(
        2. * near / w,
        0.,
        0.,
        0.,
        0.,
        2. * near / h,
        0.,
        0.,
        (right + left) / w,
        (top + bottom) / h,
        -(far + near) / d,
        -1.,
        0.,
        0.,
        -2. * far * near / d,
        0.,
    )
}

#[cfg(test)]
mod reference_reset_tests {
    use super::*;
    use crate::ApiSpace;
    type Pose = RigidTransform3D<f32, ApiSpace, ApiSpace>;

    fn close(actual: euclid::Point3D<f32, ApiSpace>, expected: [f32; 3]) {
        for (a, b) in [actual.x, actual.y, actual.z].into_iter().zip(expected) {
            assert!((a - b).abs() < 1e-5, "{actual:?} != {expected:?}");
        }
    }
    #[test]
    fn reset_rotation_moves_an_offset_origin_around_the_native_origin() {
        let offset = Pose::from_translation(Vector3D::new(2., 0., 0.));
        let delta = Pose::new(Rotation3D::around_y(Angle::radians(std::f32::consts::FRAC_PI_2)), Vector3D::zero());
        let reset = offset_reference_space_reset(offset, delta);
        close(reset.rotation.transform_point3d(euclid::Point3D::origin()) + reset.translation, [-2., 0., -2.]);
        close(reset.rotation.transform_point3d(euclid::Point3D::new(1., 0., 0.)) + reset.translation, [-2., 0., -3.]);
    }
    #[test]
    fn reset_translation_is_expressed_in_the_offset_axes() {
        let offset = Pose::new(Rotation3D::around_y(Angle::radians(std::f32::consts::FRAC_PI_2)), Vector3D::zero());
        let delta = Pose::from_translation(Vector3D::new(1., 0., 0.));
        let reset = offset_reference_space_reset(offset, delta);
        close(reset.translation.to_point(), [0., 0., 1.]);
    }
}
