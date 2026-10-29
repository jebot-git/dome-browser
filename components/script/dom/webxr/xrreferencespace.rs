/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::{cell::Cell, rc::Rc};

use dom_struct::dom_struct;
use euclid::{Point2D, RigidTransform3D};
use js::context::JSContext;
use script_bindings::reflector::reflect_weak_referenceable_dom_object;
use webxr_api::{self, Floor, Frame, Space};

use crate::dom::bindings::codegen::Bindings::XRReferenceSpaceBinding::{
    XRReferenceSpaceMethods, XRReferenceSpaceType,
};
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::globalscope::GlobalScope;
use crate::dom::xrboundedreferencespace::XRBoundedReferenceSpace;
use crate::dom::window::Window;
use crate::dom::xrrigidtransform::XRRigidTransform;
use crate::dom::xrsession::{ApiPose, ApiRigidTransform, BaseTransform, XRSession, cast_transform};
use crate::dom::xrspace::XRSpace;

#[dom_struct]
pub(crate) struct XRReferenceSpace {
    xrspace: XRSpace,
    offset: Dom<XRRigidTransform>,
    ty: XRReferenceSpaceType,
    reset_listener_count: Cell<usize>,
}

impl XRReferenceSpace {
    pub(crate) fn new_inherited(
        session: &XRSession,
        offset: &XRRigidTransform,
        ty: XRReferenceSpaceType,
    ) -> XRReferenceSpace {
        XRReferenceSpace {
            xrspace: XRSpace::new_inherited(session),
            offset: Dom::from_ref(offset),
            ty,
            reset_listener_count: Cell::new(0),
        }
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        session: &XRSession,
        ty: XRReferenceSpaceType,
    ) -> DomRoot<XRReferenceSpace> {
        let offset = XRRigidTransform::identity(cx, window);
        Self::new_offset(cx, &window.global(), session, ty, &offset)
    }

    pub(crate) fn new_offset(
        cx: &mut JSContext,
        global: &GlobalScope,
        session: &XRSession,
        ty: XRReferenceSpaceType,
        offset: &XRRigidTransform,
    ) -> DomRoot<XRReferenceSpace> {
        let space = reflect_weak_referenceable_dom_object(
            cx,
            Rc::new(XRReferenceSpace::new_inherited(session, offset, ty)),
            global,
        );
        session.register_reference_space(&space);
        space
    }

    pub(crate) fn space(&self) -> Space {
        let base = match self.ty {
            XRReferenceSpaceType::Local => webxr_api::BaseSpace::Local,
            XRReferenceSpaceType::Viewer => webxr_api::BaseSpace::Viewer,
            XRReferenceSpaceType::Local_floor => webxr_api::BaseSpace::Floor,
            XRReferenceSpaceType::Bounded_floor => webxr_api::BaseSpace::BoundedFloor,
            _ => panic!("unsupported reference space found"),
        };
        let offset = self.offset.transform();
        Space { base, offset }
    }

    pub(crate) fn reset_transform(&self, delta: ApiRigidTransform) -> ApiRigidTransform {
        webxr_api::util::offset_reference_space_reset(self.offset.transform(), delta)
    }

    pub(crate) fn reset_listener_added(&self) {
        let count = self.reset_listener_count.get();
        self.reset_listener_count.set(count + 1);
        if count == 0 {
            self.upcast::<XRSpace>().session().retain_reset_listener_space(self);
        }
    }

    pub(crate) fn reset_listener_removed(&self) {
        let count = self.reset_listener_count.get();
        debug_assert!(count > 0);
        self.reset_listener_count.set(count.saturating_sub(1));
        if count == 1 {
            self.upcast::<XRSpace>().session().release_reset_listener_space(self);
        }
    }

    pub(crate) fn bounds_geometry(&self) -> Option<Vec<euclid::Point3D<f32, webxr_api::ApiSpace>>> {
        let inverse = self.offset.transform().inverse();
        Some(self.get_bounds()?.into_iter().map(|p| {
            inverse.rotation.transform_point3d(euclid::Point3D::new(p.x, 0., p.y)) + inverse.translation
        }).collect())
    }

    pub(crate) fn ty(&self) -> XRReferenceSpaceType {
        self.ty
    }
}

impl XRReferenceSpaceMethods<crate::DomTypeHolder> for XRReferenceSpace {
    /// <https://immersive-web.github.io/webxr/#dom-xrreferencespace-getoffsetreferencespace>
    fn GetOffsetReferenceSpace(&self, cx: &mut JSContext, new: &XRRigidTransform) -> DomRoot<Self> {
        let offset = new.then(cx, &self.offset);
        let session = self.upcast::<XRSpace>().session();
        if self.is::<XRBoundedReferenceSpace>() {
            let bounded = XRBoundedReferenceSpace::new_offset(cx, &self.global(), session, &offset);
            DomRoot::from_ref(bounded.reference_space())
        } else {
            Self::new_offset(cx, &self.global(), session, self.ty, &offset)
        }
    }

    // https://www.w3.org/TR/webxr/#dom-xrreferencespace-onreset
    event_handler!(reset, GetOnreset, SetOnreset);
}

impl XRReferenceSpace {
    /// Get a transform that can be used to locate the base space
    ///
    /// This is equivalent to `get_pose(self).inverse()` (in column vector notation),
    /// but with better types
    pub(crate) fn get_base_transform(&self, base_pose: &Frame) -> Option<BaseTransform> {
        let pose = self.get_pose(base_pose)?;
        Some(pose.inverse().cast_unit())
    }

    /// Gets pose represented by this space
    ///
    /// The reference origin used is common between all
    /// get_pose calls for spaces from the same device, so this can be used to compare
    /// with other spaces
    pub(crate) fn get_pose(&self, base_pose: &Frame) -> Option<ApiPose> {
        let pose = self.get_unoffset_pose(base_pose)?;
        let offset = self.offset.transform();
        // pose is a transform from the unoffset space to native space,
        // offset is a transform from offset space to unoffset space,
        // we want a transform from unoffset space to native space,
        // which is pose * offset in column vector notation
        Some(offset.then(&pose))
    }

    /// Gets pose represented by this space
    ///
    /// Does not apply originOffset, use get_viewer_pose instead if you need it
    pub(crate) fn get_unoffset_pose(&self, base_pose: &Frame) -> Option<ApiPose> {
        match self.ty {
            XRReferenceSpaceType::Local => {
                // The eye-level pose is basically whatever the headset pose was at t=0, which
                // for most devices is (0, 0, 0)
                Some(RigidTransform3D::identity())
            },
            XRReferenceSpaceType::Local_floor => {
                Some(cast_transform(webxr_api::util::local_floor_transform(base_pose.floor_transform).inverse()))
            },
            XRReferenceSpaceType::Bounded_floor => {
                let native_to_floor = base_pose.floor_transform?;
                Some(cast_transform(native_to_floor.inverse()))
            },
            XRReferenceSpaceType::Viewer => {
                Some(cast_transform(base_pose.pose.as_ref()?.transform))
            },
            _ => unimplemented!(),
        }
    }

    pub(crate) fn get_bounds(&self) -> Option<Vec<Point2D<f32, Floor>>> {
        self.upcast::<XRSpace>()
            .session()
            .with_session(|s| s.reference_space_bounds())
    }
}
