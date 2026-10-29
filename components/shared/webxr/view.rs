/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! This crate uses `euclid`'s typed units, and exposes different coordinate spaces.

use std::marker::PhantomData;

use crate::ApiSpace;

use euclid::{Rect, RigidTransform3D, Transform3D};
use malloc_size_of_derive::MallocSizeOf;
use serde::{Deserialize, Serialize};

/// The coordinate space of the viewer
/// <https://immersive-web.github.io/webxr/#dom-xrreferencespacetype-viewer>
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum Viewer {}

/// The coordinate space of the floor
/// <https://immersive-web.github.io/webxr/#dom-xrreferencespacetype-local-floor>
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum Floor {}

/// The coordinate space of the left eye
/// <https://immersive-web.github.io/webxr/#dom-xreye-left>
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum LeftEye {}

/// The coordinate space of the right eye
/// <https://immersive-web.github.io/webxr/#dom-xreye-right>
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum RightEye {}

/// The coordinate space of the left frustrum of a cubemap
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum CubeLeft {}

/// The coordinate space of the right frustrum of a cubemap
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum CubeRight {}

/// The coordinate space of the top frustrum of a cubemap
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum CubeTop {}

/// The coordinate space of the bottom frustrum of a cubemap
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum CubeBottom {}

/// The coordinate space of the back frustrum of a cubemap
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum CubeBack {}

/// Pattern-match on eyes
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub struct SomeEye<Eye>(u8, PhantomData<Eye>);
pub const LEFT_EYE: SomeEye<LeftEye> = SomeEye(0, PhantomData);
pub const RIGHT_EYE: SomeEye<RightEye> = SomeEye(1, PhantomData);
pub const VIEWER: SomeEye<Viewer> = SomeEye(2, PhantomData);
pub const CUBE_LEFT: SomeEye<CubeLeft> = SomeEye(3, PhantomData);
pub const CUBE_RIGHT: SomeEye<CubeRight> = SomeEye(4, PhantomData);
pub const CUBE_TOP: SomeEye<CubeTop> = SomeEye(5, PhantomData);
pub const CUBE_BOTTOM: SomeEye<CubeBottom> = SomeEye(6, PhantomData);
pub const CUBE_BACK: SomeEye<CubeBack> = SomeEye(7, PhantomData);

impl<Eye1, Eye2> PartialEq<SomeEye<Eye2>> for SomeEye<Eye1> {
    fn eq(&self, rhs: &SomeEye<Eye2>) -> bool {
        self.0 == rhs.0
    }
}

/// The native 3D coordinate space of the device
/// This is not part of the webvr specification.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Native {}

/// The normalized device coordinate space, where the display
/// is from (-1,-1) to (1,1).
// TODO: are we OK assuming that we can use the same coordinate system for all displays?
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Display {}

/// The unnormalized device coordinate space, where the display
/// is from (0,0) to (w,h), measured in pixels.
// TODO: are we OK assuming that we can use the same coordinate system for all displays?
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Viewport {}

/// The coordinate space of an input device
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Input {}

/// The coordinate space of a secondary capture view
#[derive(Clone, Copy, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum Capture {}

/// For each eye, the pose of that eye,
/// its projection onto its display.
/// For stereo displays, we have a `View<LeftEye>` and a `View<RightEye>`.
/// For mono displays, we hagve a `View<Viewer>`
/// <https://immersive-web.github.io/webxr/#xrview>
#[derive(Clone, Debug, Serialize, Deserialize, MallocSizeOf)]
pub struct View<Eye> {
    pub transform: RigidTransform3D<f32, Eye, Native>,
    pub projection: Transform3D<f32, Eye, Display>,
}

impl<Eye> Default for View<Eye> {
    fn default() -> Self {
        View {
            transform: RigidTransform3D::identity(),
            projection: Transform3D::identity(),
        }
    }
}

impl<Eye> View<Eye> {
    pub fn cast_unit<NewEye>(&self) -> View<NewEye> {
        View {
            transform: self.transform.cast_unit(),
            projection: Transform3D::from_untyped(&self.projection.to_untyped()),
        }
    }
}

/// The eye label is independent of the number or position of active views.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, MallocSizeOf)]
pub enum ViewEye {
    None,
    Left,
    Right,
}

/// Description of an active view with a stable identity within its session.
/// Replacing a view must give it a new ID, even if its geometry is identical.
#[derive(Clone, Debug, Serialize, Deserialize, MallocSizeOf)]
pub struct ViewDescription {
    pub id: u64,
    pub eye: ViewEye,
    pub view: View<ApiSpace>,
    pub is_first_person_observer: bool,
}

/// Whether a device is mono or stereo, and the views it supports.
#[derive(Clone, Debug, Serialize, Deserialize, MallocSizeOf)]
#[expect(clippy::large_enum_variant)]
pub enum Views {
    /// Mono view for inline VR, viewport and projection matrices are calculated by client
    Inline,
    Mono(View<Viewer>),
    List(Vec<ViewDescription>),
    Stereo(View<LeftEye>, View<RightEye>),
    StereoCapture(View<LeftEye>, View<RightEye>, View<Capture>),
    Cubemap(
        View<Viewer>,
        View<CubeLeft>,
        View<CubeRight>,
        View<CubeTop>,
        View<CubeBottom>,
        View<CubeBack>,
    ),
}

impl Views {
    pub fn identities(&self) -> Vec<u64> {
        let count = match self {
            Self::List(views) => return views.iter().map(|view| view.id).collect(),
            Self::Inline | Self::Mono(_) => 1,
            Self::Stereo(..) => 2,
            Self::StereoCapture(..) => 3,
            Self::Cubemap(..) => 6,
        };
        (0..count).collect()
    }
}

/// Eye geometry relative to the viewer, independent of world tracking.
#[derive(Clone, Debug, Serialize, Deserialize, MallocSizeOf)]
pub struct ViewerView {
    pub id: u64,
    pub eye: ViewEye,
    pub transform: RigidTransform3D<f32, ApiSpace, Viewer>,
    pub projection: Transform3D<f32, ApiSpace, Display>,
    pub is_first_person_observer: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, MallocSizeOf)]
pub enum ViewerViews {
    Inline,
    List(Vec<ViewerView>),
}

impl ViewerViews {
    pub fn identities(&self) -> Vec<u64> {
        match self {
            Self::Inline => vec![0],
            Self::List(views) => views.iter().map(|view| view.id).collect(),
        }
    }
}

impl Views {
    pub fn relative_to_viewer(
        &self,
        viewer_to_native: RigidTransform3D<f32, Viewer, Native>,
    ) -> ViewerViews {
        if matches!(self, Self::Inline) {
            return ViewerViews::Inline;
        }
        let native_to_viewer = viewer_to_native.inverse();
        let mut views = Vec::new();
        let mut push = |id, eye, observer, view: View<ApiSpace>| {
            views.push(ViewerView {
                id,
                eye,
                transform: view.transform.then(&native_to_viewer),
                projection: view.projection,
                is_first_person_observer: observer,
            });
        };
        match self {
            Self::Inline => unreachable!(),
            Self::List(descriptions) => {
                for description in descriptions {
                    push(description.id, description.eye,
                        description.is_first_person_observer, description.view.clone());
                }
            },
            Self::Mono(view) => push(0, ViewEye::None, false, view.cast_unit()),
            Self::Stereo(left, right) => {
                push(0, ViewEye::Left, false, left.cast_unit());
                push(1, ViewEye::Right, false, right.cast_unit());
            },
            Self::StereoCapture(left, right, capture) => {
                push(0, ViewEye::Left, false, left.cast_unit());
                push(1, ViewEye::Right, false, right.cast_unit());
                push(2, ViewEye::None, true, capture.cast_unit());
            },
            Self::Cubemap(front, left, right, top, bottom, back) => {
                push(0, ViewEye::None, false, front.cast_unit());
                push(1, ViewEye::None, false, left.cast_unit());
                push(2, ViewEye::None, false, right.cast_unit());
                push(3, ViewEye::None, false, top.cast_unit());
                push(4, ViewEye::None, false, bottom.cast_unit());
                push(5, ViewEye::None, false, back.cast_unit());
            },
        }
        ViewerViews::List(views)
    }
}

/// A list of viewports per-eye in the order of fields in Views.
///
/// Not all must be in active use.
#[derive(Clone, Debug, Serialize, Deserialize, MallocSizeOf)]
pub struct Viewports {
    pub viewports: Vec<Rect<i32, Viewport>>,
}
