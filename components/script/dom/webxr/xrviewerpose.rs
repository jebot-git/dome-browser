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
use webxr_api::{ApiSpace, ViewEye, Viewer, ViewerView, ViewerViews};

use crate::dom::bindings::codegen::Bindings::XRViewerPoseBinding::XRViewerPoseMethods;
use crate::dom::bindings::root::DomRoot;
use crate::dom::window::Window;
use crate::dom::xrframe::XRFrame;
use crate::dom::xrpose::XRPose;
use crate::dom::xrrigidtransform::XRRigidTransform;
use crate::dom::xrsession::{BaseSpace, XRSession, cast_transform};
use crate::dom::xrview::XRView;
use crate::realms::enter_auto_realm;

#[dom_struct]
pub(crate) struct XRViewerPose {
    pose: XRPose,
    #[ignore_malloc_size_of = "mozjs"]
    views: Heap<JSVal>,
}

impl XRViewerPose {
    fn new_inherited(transform: &XRRigidTransform, emulated: bool) -> XRViewerPose {
        XRViewerPose {
            pose: XRPose::new_inherited_with_emulation(transform, emulated),
            views: Heap::default(),
        }
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        session: &XRSession,
        frame: &XRFrame,
        viewer_to_base: RigidTransform3D<f32, Viewer, BaseSpace>,
        descriptions: &ViewerViews,
        emulated: bool,
    ) -> DomRoot<XRViewerPose> {
        let mut realm = enter_auto_realm(cx, window);
        let cx = &mut realm.current_realm();
        rooted_vec!(let mut views);
        match descriptions {
            ViewerViews::Inline => {
                let inline = session.inline_view();
                let view = ViewerView {
                    id: 0, eye: ViewEye::None,
                    transform: RigidTransform3D::<f32, ApiSpace, Viewer>::identity(),
                    projection: inline.cast_unit::<ApiSpace>().projection,
                    is_first_person_observer: false,
                };
                views.push(XRView::new(cx, window, session, frame, &view, 0, &viewer_to_base));
            },
            ViewerViews::List(descriptions) => {
                for (index, description) in descriptions.iter().enumerate() {
                    views.push(XRView::new(cx, window, session, frame, description, index, &viewer_to_base));
                }
            },
        }
        let transform = XRRigidTransform::new(cx, window, cast_transform(viewer_to_base));
        let pose = reflect_dom_object(cx,
            Box::new(XRViewerPose::new_inherited(&transform, emulated)), window);
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
