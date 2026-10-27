/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use euclid::RigidTransform3D;
use js::context::JSContext;
use js::jsapi::Heap;
use js::jsval::{JSVal, UndefinedValue};
use js::rust::MutableHandleValue;
use script_bindings::reflector::reflect_dom_object;
use webxr_api::{Viewer, ViewerPose, Views};

use crate::dom::bindings::codegen::Bindings::XRViewBinding::XREye;
use crate::dom::bindings::codegen::Bindings::XRViewerPoseBinding::XRViewerPoseMethods;
use crate::dom::bindings::root::DomRoot;
use crate::dom::window::Window;
use crate::dom::xrframe::XRFrame;
use crate::dom::xrpose::XRPose;
use crate::dom::xrrigidtransform::XRRigidTransform;
use crate::dom::xrsession::{BaseSpace, BaseTransform, XRSession, cast_transform};
use crate::dom::xrview::XRView;
use crate::realms::enter_auto_realm;

#[dom_struct]
pub(crate) struct XRViewerPose {
    pose: XRPose,
    #[ignore_malloc_size_of = "mozjs"]
    views: Heap<JSVal>,
}

impl XRViewerPose {
    fn new_inherited(transform: &XRRigidTransform) -> XRViewerPose {
        XRViewerPose {
            pose: XRPose::new_inherited(transform),
            views: Heap::default(),
        }
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        session: &XRSession,
        frame: &XRFrame,
        to_base: BaseTransform,
        viewer_pose: &ViewerPose,
    ) -> DomRoot<XRViewerPose> {
        let mut realm = enter_auto_realm(cx, window);
        let cx = &mut realm.current_realm();
        rooted_vec!(let mut views);
        match &viewer_pose.views {
            Views::Inline => views.push(XRView::new(
                cx,
                window,
                session,
                frame,
                &session.inline_view(),
                XREye::None,
                0,
                &to_base,
            )),
            Views::List(descriptions) => {
                for (index, description) in descriptions.iter().enumerate() {
                    let eye = match description.eye {
                        webxr_api::ViewEye::None => XREye::None,
                        webxr_api::ViewEye::Left => XREye::Left,
                        webxr_api::ViewEye::Right => XREye::Right,
                    };
                    let view = XRView::new(
                        cx,
                        window,
                        session,
                        frame,
                        &description.view,
                        eye,
                        index,
                        &to_base,
                    );
                    view.describe(description.id, description.is_first_person_observer);
                    views.push(view);
                }
            },
            Views::Mono(view) => views.push(XRView::new(
                cx,
                window,
                session,
                frame,
                view,
                XREye::None,
                0,
                &to_base,
            )),
            Views::Stereo(left, right) => {
                views.push(XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    left,
                    XREye::Left,
                    0,
                    &to_base,
                ));
                views.push(XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    right,
                    XREye::Right,
                    1,
                    &to_base,
                ));
            },
            Views::StereoCapture(left, right, third_eye) => {
                views.push(XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    left,
                    XREye::Left,
                    0,
                    &to_base,
                ));
                views.push(XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    right,
                    XREye::Right,
                    1,
                    &to_base,
                ));
                let observer = XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    third_eye,
                    XREye::None,
                    2,
                    &to_base,
                );
                observer.describe(2, true);
                views.push(observer);
            },
            Views::Cubemap(front, left, right, top, bottom, back) => {
                views.push(XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    front,
                    XREye::None,
                    0,
                    &to_base,
                ));
                views.push(XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    left,
                    XREye::None,
                    1,
                    &to_base,
                ));
                views.push(XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    right,
                    XREye::None,
                    2,
                    &to_base,
                ));
                views.push(XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    top,
                    XREye::None,
                    3,
                    &to_base,
                ));
                views.push(XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    bottom,
                    XREye::None,
                    4,
                    &to_base,
                ));
                views.push(XRView::new(
                    cx,
                    window,
                    session,
                    frame,
                    back,
                    XREye::None,
                    5,
                    &to_base,
                ));
            },
        };
        let transform: RigidTransform3D<f32, Viewer, BaseSpace> =
            viewer_pose.transform.then(&to_base);
        let transform = XRRigidTransform::new(cx, window, cast_transform(transform));
        let pose = reflect_dom_object(
            cx,
            Box::new(XRViewerPose::new_inherited(&transform)),
            window,
        );

        rooted!(&in(cx) let mut jsval = UndefinedValue());
        script_bindings::utils::to_frozen_array(cx, &views, jsval.handle_mut());
        pose.views.set(jsval.get());

        pose
    }
}

impl XRViewerPoseMethods<crate::DomTypeHolder> for XRViewerPose {
    /// <https://immersive-web.github.io/webxr/#dom-xrviewerpose-views>
    fn Views(&self, mut retval: MutableHandleValue) {
        retval.set(self.views.get())
    }
}
