/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use euclid::{RigidTransform3D, Rotation3D, Vector3D};
use js::context::JSContext;
use js::rust::HandleObject;
use js::typedarray::{Float32, HeapFloat32Array};
use script_bindings::cformat;
use script_bindings::reflector::{Reflector, reflect_dom_object_with_proto};
use script_bindings::trace::RootedTraceableBox;
use webxr_api::ApiSpace;

use crate::dom::bindings::buffer_source::HeapBufferSource;
use crate::dom::bindings::codegen::Bindings::DOMPointBinding::DOMPointInit;
use crate::dom::bindings::codegen::Bindings::XRRigidTransformBinding::XRRigidTransformMethods;
use crate::dom::bindings::error::{Error, Fallible};
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::{DomRoot, MutNullableDom};
use crate::dom::dompointreadonly::DOMPointReadOnly;
use crate::dom::window::Window;
use crate::dom::xrsession::ApiRigidTransform;

// DOMPointInit uses doubles. Keep them until a native backend needs floats.
type DomRigidTransform = RigidTransform3D<f64, ApiSpace, ApiSpace>;

#[dom_struct]
pub(crate) struct XRRigidTransform {
    reflector_: Reflector,
    position: MutNullableDom<DOMPointReadOnly>,
    orientation: MutNullableDom<DOMPointReadOnly>,
    #[no_trace]
    transform: DomRigidTransform,
    inverse: MutNullableDom<XRRigidTransform>,
    #[ignore_malloc_size_of = "defined in mozjs"]
    matrix: HeapBufferSource<Float32>,
}

impl XRRigidTransform {
    fn new_inherited(transform: DomRigidTransform) -> XRRigidTransform {
        XRRigidTransform {
            reflector_: Reflector::new(),
            position: MutNullableDom::default(),
            orientation: MutNullableDom::default(),
            transform,
            inverse: MutNullableDom::default(),
            matrix: HeapBufferSource::default(),
        }
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        transform: ApiRigidTransform,
    ) -> DomRoot<XRRigidTransform> {
        let rotation = transform.rotation;
        let translation = transform.translation;
        let transform = RigidTransform3D::new(
            Rotation3D::quaternion(
                rotation.i.into(),
                rotation.j.into(),
                rotation.k.into(),
                rotation.r.into(),
            ),
            Vector3D::new(
                translation.x.into(),
                translation.y.into(),
                translation.z.into(),
            ),
        );
        Self::new_with_proto(cx, window, None, transform)
    }

    fn new_with_proto(
        cx: &mut JSContext,
        window: &Window,
        proto: Option<HandleObject>,
        transform: DomRigidTransform,
    ) -> DomRoot<XRRigidTransform> {
        reflect_dom_object_with_proto(
            cx,
            Box::new(XRRigidTransform::new_inherited(transform)),
            window,
            proto,
        )
    }

    pub(crate) fn identity(cx: &mut JSContext, window: &Window) -> DomRoot<XRRigidTransform> {
        let transform = RigidTransform3D::identity();
        XRRigidTransform::new(cx, window, transform)
    }
}

impl XRRigidTransformMethods<crate::DomTypeHolder> for XRRigidTransform {
    /// <https://immersive-web.github.io/webxr/#dom-xrrigidtransform-xrrigidtransform>
    fn Constructor(
        cx: &mut JSContext,
        window: &Window,
        proto: Option<HandleObject>,
        position: &DOMPointInit,
        orientation: &DOMPointInit,
    ) -> Fallible<DomRoot<Self>> {
        if position.w != 1.0 {
            return Err(Error::Type(cformat!(
                "XRRigidTransform must be constructed with a position that has a w value of of 1.0, not {}",
                position.w
            )));
        }

        if !position.x.is_finite() ||
            !position.y.is_finite() ||
            !position.z.is_finite() ||
            !position.w.is_finite()
        {
            return Err(Error::Type(
                c"Position must not contain non-finite values".into(),
            ));
        }

        if !orientation.x.is_finite() ||
            !orientation.y.is_finite() ||
            !orientation.z.is_finite() ||
            !orientation.w.is_finite()
        {
            return Err(Error::Type(
                c"Orientation must not contain non-finite values".into(),
            ));
        }

        let length = (orientation.x * orientation.x +
            orientation.y * orientation.y +
            orientation.z * orientation.z +
            orientation.w * orientation.w)
            .sqrt();
        if length == 0.0 || !length.is_finite() {
            return Err(Error::InvalidState(None));
        }
        let translate = Vector3D::new(position.x, position.y, position.z);
        let rotate = Rotation3D::quaternion(
            orientation.x / length,
            orientation.y / length,
            orientation.z / length,
            orientation.w / length,
        );
        let transform = RigidTransform3D::new(rotate, translate);
        Ok(XRRigidTransform::new_with_proto(
            cx, window, proto, transform,
        ))
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrrigidtransform-position>
    fn Position(&self, cx: &mut JSContext) -> DomRoot<DOMPointReadOnly> {
        self.position.or_init(|| {
            let t = &self.transform.translation;
            DOMPointReadOnly::new(cx, &self.global(), t.x, t.y, t.z, 1.0)
        })
    }
    /// <https://immersive-web.github.io/webxr/#dom-xrrigidtransform-orientation>
    fn Orientation(&self, cx: &mut JSContext) -> DomRoot<DOMPointReadOnly> {
        self.orientation.or_init(|| {
            let r = &self.transform.rotation;
            DOMPointReadOnly::new(
                cx,
                &self.global(),
                r.i,
                r.j,
                r.k,
                r.r,
            )
        })
    }
    /// <https://immersive-web.github.io/webxr/#dom-xrrigidtransform-inverse>
    fn Inverse(&self, cx: &mut JSContext) -> DomRoot<XRRigidTransform> {
        self.inverse.or_init(|| {
            let transform = XRRigidTransform::new_with_proto(
                cx,
                self.global().as_window(),
                None,
                self.transform.inverse(),
            );
            transform.inverse.set(Some(self));
            transform
        })
    }
    /// <https://immersive-web.github.io/webxr/#dom-xrrigidtransform-matrix>
    fn Matrix(&self, cx: &mut JSContext) -> RootedTraceableBox<HeapFloat32Array> {
        if !self.matrix.is_initialized() || self.matrix.is_detached_buffer(cx) {
            let data = self.transform.to_transform().to_array().map(|value| value as f32);
            self.matrix
                .set_data(cx, &data)
                .expect("Failed to set on data on transform's internal matrix.")
        }

        self.matrix
            .get_typed_array()
            .expect("Failed to get transform's internal matrix.")
    }
}

impl XRRigidTransform {
    /// <https://immersive-web.github.io/webxr/#dom-xrpose-transform>
    pub(crate) fn transform(&self) -> ApiRigidTransform {
        let rotation = self.transform.rotation;
        let translation = self.transform.translation;
        RigidTransform3D::new(
            Rotation3D::quaternion(
                rotation.i as f32,
                rotation.j as f32,
                rotation.k as f32,
                rotation.r as f32,
            ),
            Vector3D::new(
                translation.x as f32,
                translation.y as f32,
                translation.z as f32,
            ),
        )
    }

    pub(crate) fn then(&self, cx: &mut JSContext, next: &Self) -> DomRoot<Self> {
        Self::new_with_proto(
            cx,
            self.global().as_window(),
            None,
            self.transform.then(&next.transform),
        )
    }
}
