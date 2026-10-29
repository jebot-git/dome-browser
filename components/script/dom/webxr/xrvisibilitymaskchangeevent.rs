/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use js::context::JSContext;
use js::rust::HandleObject;
use js::typedarray::{Float32, HeapFloat32Array, HeapUint32Array, Uint32};
use script_bindings::reflector::reflect_dom_object_with_proto;
use script_bindings::trace::RootedTraceableBox;
use stylo_atoms::Atom;

use crate::dom::bindings::buffer_source::HeapBufferSource;
use crate::dom::bindings::codegen::Bindings::EventBinding::Event_Binding::EventMethods;
use crate::dom::bindings::codegen::Bindings::XRViewBinding::XREye;
use crate::dom::bindings::codegen::Bindings::XRVisibilityMaskChangeEventBinding::{
    XRVisibilityMaskChangeEventInit, XRVisibilityMaskChangeEventMethods,
};
use crate::dom::bindings::error::{Error, Fallible};
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::bindings::str::DOMString;
use crate::dom::event::Event;
use crate::dom::window::Window;
use crate::dom::xrsession::XRSession;

#[dom_struct]
pub(crate) struct XRVisibilityMaskChangeEvent {
    event: Event,
    session: Dom<XRSession>,
    eye: XREye,
    index: u32,
    #[ignore_malloc_size_of = "mozjs"]
    vertices: HeapBufferSource<Float32>,
    #[ignore_malloc_size_of = "mozjs"]
    indices: HeapBufferSource<Uint32>,
}

impl XRVisibilityMaskChangeEvent {
    #[expect(clippy::too_many_arguments)]
    fn new_with_proto(
        cx: &mut JSContext,
        window: &Window,
        proto: Option<HandleObject>,
        type_: Atom,
        session: &XRSession,
        eye: XREye,
        index: u32,
        vertices: HeapBufferSource<Float32>,
        indices: HeapBufferSource<Uint32>,
        bubbles: bool,
        cancelable: bool,
    ) -> DomRoot<Self> {
        let result = reflect_dom_object_with_proto(
            cx,
            Box::new(Self {
                event: Event::new_inherited(),
                session: Dom::from_ref(session),
                eye,
                index,
                vertices,
                indices,
            }),
            window,
            proto,
        );
        result
            .upcast::<Event>()
            .init_event(type_, bubbles, cancelable);
        result
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        session: &XRSession,
        eye: webxr_api::ViewEye,
        index: u32,
        mask: &webxr_api::VisibilityMask,
    ) -> Fallible<DomRoot<Self>> {
        let eye = match eye {
            webxr_api::ViewEye::None => XREye::None,
            webxr_api::ViewEye::Left => XREye::Left,
            webxr_api::ViewEye::Right => XREye::Right,
        };
        // Reflect first so both arrays remain traced while either allocation can GC.
        let event = Self::new_with_proto(
            cx,
            window,
            None,
            Atom::from("visibilitymaskchange"),
            session,
            eye,
            index,
            HeapBufferSource::default(),
            HeapBufferSource::default(),
            false,
            false,
        );
        event
            .vertices
            .set_data(cx, &mask.vertices)
            .map_err(|_| Error::JSFailed)?;
        event
            .indices
            .set_data(cx, &mask.indices)
            .map_err(|_| Error::JSFailed)?;
        Ok(event)
    }
}

impl XRVisibilityMaskChangeEventMethods<crate::DomTypeHolder> for XRVisibilityMaskChangeEvent {
    #[expect(unsafe_code)]
    fn Constructor(
        cx: &mut JSContext,
        window: &Window,
        proto: Option<HandleObject>,
        type_: DOMString,
        init: &XRVisibilityMaskChangeEventInit,
    ) -> Fallible<DomRoot<Self>> {
        // The generated dictionary roots the supplied typed arrays. Retain their
        // identity, as required by the Event constructor and SameObject attributes.
        rooted!(&in(cx) let vertices = unsafe { init.vertices.underlying_object().get() });
        rooted!(&in(cx) let indices = unsafe { init.indices.underlying_object().get() });
        Ok(Self::new_with_proto(
            cx,
            window,
            proto,
            Atom::from(type_),
            &init.session,
            init.eye,
            init.index,
            HeapBufferSource::new(vertices.handle()),
            HeapBufferSource::new(indices.handle()),
            init.parent.bubbles,
            init.parent.cancelable,
        ))
    }

    fn Session(&self) -> DomRoot<XRSession> {
        DomRoot::from_ref(&*self.session)
    }
    fn Eye(&self) -> XREye {
        self.eye
    }
    fn Index(&self) -> u32 {
        self.index
    }
    fn Vertices(&self) -> RootedTraceableBox<HeapFloat32Array> {
        self.vertices
            .get_typed_array()
            .expect("Initialized visibility-mask vertices")
    }
    fn Indices(&self) -> RootedTraceableBox<HeapUint32Array> {
        self.indices
            .get_typed_array()
            .expect("Initialized visibility-mask indices")
    }
    fn IsTrusted(&self) -> bool {
        self.event.IsTrusted()
    }
}
