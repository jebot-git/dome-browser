/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Native values only: no DOM roots, and snapshots never consult a later frame.
use euclid::RigidTransform3D;
use malloc_size_of_derive::MallocSizeOf;
use webxr_api::{BaseSpace, Frame, ViewerViews};

use super::{ApiRigidTransform, cast_transform};

#[derive(Clone, MallocSizeOf)]
pub(crate) struct LocatedViewer {
    pub transform: ApiRigidTransform,
    pub emulated: bool,
}

#[derive(Clone, Default, MallocSizeOf)]
pub(crate) struct ViewerFrameState {
    pub local: Option<LocatedViewer>,
    pub floor: Option<LocatedViewer>,
    pub bounded_floor: Option<LocatedViewer>,
    pub views: Option<ViewerViews>,
}

#[derive(Clone, Default, MallocSizeOf)]
pub(crate) struct ViewerCache {
    local: Option<ApiRigidTransform>,
    floor: Option<ApiRigidTransform>,
    bounded_floor: Option<ApiRigidTransform>,
    views: Option<ViewerViews>,
}

impl ViewerCache {
    pub fn observe(&mut self, frame: &Frame) -> ViewerFrameState {
        let local = frame.pose.as_ref().map(|pose| LocatedViewer {
            transform: cast_transform(pose.transform),
            emulated: pose.emulated_position,
        });
        let floor = frame.pose.as_ref().zip(frame.floor_transform).map(|(pose, floor)| LocatedViewer {
            transform: cast_transform(pose.transform.then(&floor)),
            emulated: pose.emulated_position,
        });
        if let Some(pose) = &local { self.local = Some(pose.transform); }
        if let Some(pose) = &floor {
            self.floor = Some(pose.transform);
            self.bounded_floor = Some(pose.transform);
        }
        if let Some(pose) = &frame.pose {
            self.views = Some(pose.views.relative_to_viewer(pose.transform));
        } else if let Some(views) = &frame.untracked_views {
            self.views = Some(views.clone());
        }
        ViewerFrameState {
            local: local.or_else(|| self.local.map(|transform| LocatedViewer { transform, emulated: true })),
            floor: floor.clone().or_else(|| self.floor.map(|transform| LocatedViewer { transform, emulated: true })),
            bounded_floor: floor.or_else(|| self.bounded_floor.map(|transform| LocatedViewer { transform, emulated: true })),
            views: self.views.clone(),
        }
    }

    pub fn reset(&mut self, base: BaseSpace, new_to_old: Option<ApiRigidTransform>) {
        let update = |value: &mut Option<ApiRigidTransform>| {
            *value = value.zip(new_to_old).map(|(pose, delta)| pose.then(&delta.inverse()));
        };
        match base {
            BaseSpace::Local => update(&mut self.local),
            BaseSpace::Floor => update(&mut self.floor),
            BaseSpace::BoundedFloor => update(&mut self.bounded_floor),
            BaseSpace::Viewer => {
                self.local = self.local.zip(new_to_old).map(|(pose, delta)| delta.then(&pose));
                self.floor = self.floor.zip(new_to_old).map(|(pose, delta)| delta.then(&pose));
                self.bounded_floor = self.bounded_floor.zip(new_to_old).map(|(pose, delta)| delta.then(&pose));
            },
            _ => {},
        }
    }
}

impl ViewerFrameState {
    pub fn locate(&self, base: BaseSpace) -> Option<LocatedViewer> {
        match base {
            BaseSpace::Viewer => Some(LocatedViewer { transform: RigidTransform3D::identity(), emulated: false }),
            BaseSpace::Local => self.local.clone(),
            BaseSpace::Floor => self.floor.clone(),
            BaseSpace::BoundedFloor => self.bounded_floor.clone(),
            _ => None,
        }
    }
}
