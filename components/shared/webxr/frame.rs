/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use euclid::RigidTransform3D;
use malloc_size_of_derive::MallocSizeOf;
use serde::{Deserialize, Serialize};

use crate::{
    ApiSpace, BaseSpace, Floor, HitTestId, HitTestResult, InputFrame, Native, SubImages, Viewer, ViewerViews, Viewports, Views,
};

/// The per-frame data that is provided by the device.
/// <https://www.w3.org/TR/webxr/#xrframe>
// TODO: other fields?
#[derive(Clone, Debug, Serialize, Deserialize, MallocSizeOf)]
pub struct Frame {
    /// The pose information of the viewer
    pub pose: Option<ViewerPose>,
    /// Calibrated eye geometry can remain available without a world pose.
    #[serde(default)]
    pub untracked_views: Option<ViewerViews>,
    /// Floor basis captured with this frame, rather than read from a later session state.
    #[serde(default)]
    pub floor_transform: Option<RigidTransform3D<f32, Native, Floor>>,
    /// Frame information for each connected input source
    pub inputs: Vec<InputFrame>,

    /// Events that occur with the frame.
    pub events: Vec<FrameUpdateEvent>,

    /// The subimages to render to
    pub sub_images: Vec<SubImages>,

    /// The hit test results for this frame, if any
    pub hit_test_results: Vec<HitTestResult>,

    /// The average point in time this XRFrame is expected to be displayed on the devices' display
    pub predicted_display_time: f64,

    /// The corresponding local monotonic instant, before conversion to the
    /// requesting document's performance timeline. None denotes an estimate.
    #[serde(default)]
    pub predicted_display_instant: Option<servo_base::cross_process_instant::CrossProcessInstant>,
}

#[derive(Clone, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum FrameUpdateEvent {
    /// Applied before animation callbacks using the new native origin.
    ReferenceSpaceChanged(#[ignore_malloc_size_of = "BaseSpace is a fixed-size identifier"] BaseSpace, Option<RigidTransform3D<f32, ApiSpace, ApiSpace>>),
    UpdateFloorTransform(Option<RigidTransform3D<f32, Native, Floor>>),
    UpdateViewports(Viewports),
    HitTestSourceAdded(HitTestId),
    /// Applied before callbacks rendering at the new compositor cadence.
    NominalFrameRateChanged(f32),
    /// Geometry corresponds to the active view list of this frame.
    VisibilityMaskChanged {
        index: u32,
        eye: crate::ViewEye,
        mask: crate::VisibilityMask,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, MallocSizeOf)]
pub struct ViewerPose {
    /// The transform from the viewer to native coordinates
    ///
    /// This is equivalent to the pose of the viewer in native coordinates.
    /// This is the inverse of the view matrix.
    pub transform: RigidTransform3D<f32, Viewer, Native>,
    #[serde(default)]
    pub emulated_position: bool,

    // The various views
    pub views: Views,
}
