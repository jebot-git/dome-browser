/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use euclid::{RigidTransform3D, Transform3D};
use js::context::JSContext;
use js::typedarray::{Float32, HeapFloat32Array};
use script_bindings::reflector::{Reflector, reflect_dom_object};
use script_bindings::trace::RootedTraceableBox;
use std::cell::Cell;
use webxr_api::{ApiSpace, Display, ViewEye, Viewer, ViewerView};

use crate::dom::bindings::buffer_source::HeapBufferSource;
use crate::dom::bindings::codegen::Bindings::XRViewBinding::{XREye, XRViewMethods};
use crate::dom::bindings::num::Finite;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::window::Window;
use crate::dom::xrframe::XRFrame;
use crate::dom::xrrigidtransform::XRRigidTransform;
use crate::dom::xrsession::{BaseSpace, XRSession, cast_transform};

#[dom_struct]
pub(crate) struct XRView {
    reflector_: Reflector,
    session: Dom<XRSession>,
    eye: XREye,
    viewport_index: usize,
    view_id: Cell<u64>,
    is_first_person_observer: Cell<bool>,
    #[ignore_malloc_size_of = "mozjs"]
    proj: HeapBufferSource<Float32>,
    #[no_trace]
    projection: Transform3D<f32, ApiSpace, Display>,
    transform: Dom<XRRigidTransform>,
    frame: Dom<XRFrame>,
}

impl XRView {
    fn new_inherited(
        session: &XRSession,
        frame: &XRFrame,
        transform: &XRRigidTransform,
        eye: XREye,
        viewport_index: usize,
        projection: Transform3D<f32, ApiSpace, Display>,
    ) -> XRView {
        XRView {
            reflector_: Reflector::new(),
            session: Dom::from_ref(session),
            eye,
            viewport_index,
            view_id: Cell::new(viewport_index as u64),
            is_first_person_observer: Cell::new(false),
            proj: HeapBufferSource::default(),
            projection,
            transform: Dom::from_ref(transform),
            frame: Dom::from_ref(frame),
        }
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        session: &XRSession,
        frame: &XRFrame,
        view: &ViewerView,
        viewport_index: usize,
        viewer_to_base: &RigidTransform3D<f32, Viewer, BaseSpace>,
    ) -> DomRoot<XRView> {
        let transform = view.transform.then(viewer_to_base);
        let transform = XRRigidTransform::new(cx, window, cast_transform(transform));
        let eye = match view.eye {
            ViewEye::None => XREye::None,
            ViewEye::Left => XREye::Left,
            ViewEye::Right => XREye::Right,
        };
        let result = reflect_dom_object(cx, Box::new(XRView::new_inherited(
            session, frame, &transform, eye, viewport_index, view.projection,
        )), window);
        result.describe(view.id, view.is_first_person_observer);
        result
    }

    pub(crate) fn session(&self) -> &XRSession {
        &self.session
    }

    pub(crate) fn frame(&self) -> &XRFrame {
        &self.frame
    }

    pub(crate) fn describe(&self, id: u64, observer: bool) {
        self.view_id.set(id);
        self.is_first_person_observer.set(observer);
    }

    pub(crate) fn view_id(&self) -> u64 {
        self.view_id.get()
    }

    pub(crate) fn viewport_index(&self) -> usize {
        self.viewport_index
    }
}

impl XRViewMethods<crate::DomTypeHolder> for XRView {
    /// <https://immersive-web.github.io/webxr/#dom-xrview-eye>
    fn Eye(&self) -> XREye {
        self.eye
    }

    fn Index(&self) -> u32 {
        self.viewport_index as u32
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrview-projectionmatrix>
    fn ProjectionMatrix(&self, cx: &mut JSContext) -> RootedTraceableBox<HeapFloat32Array> {
        if !self.proj.is_initialized() {
            // row_major since euclid uses row vectors
            let proj = self.projection.to_array();
            self.proj
                .set_data(cx, &proj)
                .expect("Failed to set projection matrix.")
        }
        self.proj
            .get_typed_array()
            .expect("Failed to get projection matrix.")
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrview-transform>
    fn Transform(&self) -> DomRoot<XRRigidTransform> {
        DomRoot::from_ref(&self.transform)
    }

    /// <https://www.w3.org/TR/webxr/#dom-xrview-recommendedviewportscale>
    fn GetRecommendedViewportScale(&self) -> Option<Finite<f64>> {
        // No performance heuristic is implemented.
        None
    }

    /// <https://www.w3.org/TR/webxr/#dom-xrview-requestviewportscale>
    fn RequestViewportScale(&self, scale: Option<Finite<f64>>) {
        self.session
            .request_viewport_scale(self.view_id(), scale.map(|scale| *scale));
    }

    /// <https://www.w3.org/TR/webxr-ar-module-1/#dom-xrview-isfirstpersonobserver>
    fn IsFirstPersonObserver(&self) -> bool {
        self.is_first_person_observer.get()
    }
}
