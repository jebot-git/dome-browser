/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, PI};
use std::rc::Rc;
use std::{mem, ptr};

use script_bindings::reflector::reflect_weak_referenceable_dom_object;
use dom_struct::dom_struct;
use js::context::JSContext;
use js::realm::CurrentRealm;
use euclid::{RigidTransform3D, Transform3D, Vector3D};
use js::jsapi::JSObject;
use js::rust::MutableHandleValue;
use js::typedarray::HeapFloat32Array;
use profile_traits::generic_callback::GenericCallback as ProfileGenericCallback;
use rustc_hash::FxBuildHasher;
use script_bindings::callback::{RootedCallback, TracedCallback};
use script_bindings::trace::RootedTraceableBox;
use servo_base::cross_process_instant::CrossProcessInstant;
use stylo_atoms::Atom;
use webxr_api::{
    self, ApiSpace, ContextId as WebXRContextId, Display, EntityTypes, EnvironmentBlendMode,
    Event as XREvent, Frame, FrameUpdateEvent, HitTestId, HitTestSource, InputFrame, InputId, Ray,
    SelectEvent, SelectKind, Session, SessionId, View, Viewer, Visibility, util,
};

use crate::conversions::Convert;
use crate::canvas_context::CanvasContext;
use crate::dom::{RootedPromise, TracedPromise};
use crate::dom::bindings::weakref::WeakRef;
use crate::dom::bindings::trace::HashMapTracedValues;
use crate::dom::bindings::buffer_source::create_buffer_source;
use crate::dom::bindings::callback::ExceptionHandling;
use script_bindings::cell::DomRefCell;
use crate::dom::bindings::codegen::Bindings::NavigatorBinding::Navigator_Binding::NavigatorMethods;
use crate::dom::bindings::codegen::Bindings::DocumentBinding::DocumentMethods;
use crate::dom::bindings::codegen::Bindings::WindowBinding::Window_Binding::WindowMethods;
use crate::dom::bindings::codegen::Bindings::XRHitTestSourceBinding::{
    XRHitTestOptionsInit, XRHitTestTrackableType,
};
use crate::dom::bindings::codegen::Bindings::XRInputSourceArrayBinding::XRInputSourceArray_Binding::XRInputSourceArrayMethods;
use crate::dom::bindings::codegen::Bindings::XRReferenceSpaceBinding::XRReferenceSpaceType;
use crate::dom::bindings::codegen::Bindings::XRRenderStateBinding::{
    XRRenderStateInit, XRRenderStateMethods,
};
use crate::dom::bindings::codegen::Bindings::XRSessionBinding::{
    XREnvironmentBlendMode, XRFrameRequestCallback, XRInteractionMode, XRSessionMethods,
    XRVisibilityState,
};
use crate::dom::bindings::codegen::Bindings::XRSystemBinding::XRSessionMode;
use crate::dom::bindings::error::{Error, ErrorResult};
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::num::Finite;
use crate::dom::bindings::refcounted::Trusted;
use crate::dom::bindings::reflector::{DomGlobal};
use crate::dom::bindings::root::{Dom, DomRoot, MutDom, MutNullableDom};
use crate::dom::bindings::utils::to_frozen_array;
use crate::dom::event::Event;
use crate::dom::eventtarget::EventTarget;
use crate::dom::promise::Promise;
use crate::dom::window::Window;
use crate::dom::xrboundedreferencespace::XRBoundedReferenceSpace;
use crate::dom::xrframe::XRFrame;
use crate::dom::xrhittestsource::XRHitTestSource;
use crate::dom::xrinputsourcearray::XRInputSourceArray;
use crate::dom::xrinputsourceevent::XRInputSourceEvent;
use crate::dom::xrreferencespace::XRReferenceSpace;
use crate::dom::xrreferencespaceevent::XRReferenceSpaceEvent;
use crate::dom::xrrenderstate::XRRenderState;
use crate::dom::xrrigidtransform::XRRigidTransform;
use crate::dom::xrsessionevent::XRSessionEvent;
use crate::dom::xrspace::XRSpace;

mod inline_input;
mod viewer_state;
pub(crate) use viewer_state::ViewerFrameState;
use viewer_state::ViewerCache;
pub(crate) use inline_input::{InlinePointerAction, ScreenPointerSample};
use inline_input::InlinePointer;

/// Weak references require the concrete DOM type, including bounded subtypes.
#[derive(MallocSizeOf)]
enum ReferenceSpaceWeakRef {
    Reference(WeakRef<XRReferenceSpace>),
    Bounded(WeakRef<XRBoundedReferenceSpace>),
}
impl ReferenceSpaceWeakRef {
    fn new(space: &XRReferenceSpace) -> Self {
        if let Some(bounded) = space.downcast::<XRBoundedReferenceSpace>() {
            Self::Bounded(WeakRef::new(bounded))
        } else {
            Self::Reference(WeakRef::new(space))
        }
    }
    fn root(&self) -> Option<DomRoot<XRReferenceSpace>> {
        match self {
            Self::Reference(space) => space.root(),
            Self::Bounded(space) => space.root().map(|space| DomRoot::from_ref(space.reference_space())),
        }
    }
    fn is_alive(&self) -> bool {
        match self {
            Self::Reference(space) => space.is_alive(),
            Self::Bounded(space) => space.is_alive(),
        }
    }
}

#[dom_struct]
pub(crate) struct XRSession {
    eventtarget: EventTarget,
    blend_mode: XREnvironmentBlendMode,
    mode: XRSessionMode,
    visibility_state: Cell<XRVisibilityState>,
    viewer_space: MutNullableDom<XRSpace>,
    #[no_trace]
    session: DomRefCell<Session>,
    frame_requested: Cell<bool>,
    pending_render_state: MutNullableDom<XRRenderState>,
    active_render_state: MutDom<XRRenderState>,
    /// Cached projection matrix for inline sessions
    #[no_trace]
    inline_projection_matrix: DomRefCell<Transform3D<f32, Viewer, Display>>,
    #[no_trace]
    viewer_cache: DomRefCell<ViewerCache>,

    next_raf_id: Cell<i32>,
    #[ignore_malloc_size_of = "closures are hard"]
    raf_callback_list: DomRefCell<Vec<(i32, Option<TracedCallback<XRFrameRequestCallback>>)>>,
    #[ignore_malloc_size_of = "closures are hard"]
    current_raf_callback_list:
        DomRefCell<Vec<(i32, Option<TracedCallback<XRFrameRequestCallback>>)>>,
    input_sources: Dom<XRInputSourceArray>,
    // Any promises from calling end()
    end_promises: DomRefCell<Vec<TracedPromise>>,
    /// <https://immersive-web.github.io/webxr/#ended>
    ended: Cell<bool>,
    shutdown_complete: Cell<bool>,
    #[no_trace]
    next_hit_test_id: Cell<HitTestId>,
    pending_hit_test_promises:
        DomRefCell<HashMapTracedValues<HitTestId, TracedPromise, FxBuildHasher>>,
    /// Opaque framebuffers need to know the session is "outside of a requestAnimationFrame"
    /// <https://immersive-web.github.io/webxr/#opaque-framebuffer>
    outside_raf: Cell<bool>,
    #[no_trace]
    input_frames: DomRefCell<HashMap<InputId, InputFrame>>,
    #[no_trace]
    active_input_actions: DomRefCell<HashMap<InputId, u8>>,
    framerate: Cell<Option<f32>>,
    next_framerate_request_id: Cell<u64>,
    update_framerate_promises: DomRefCell<HashMapTracedValues<u64, TracedPromise, FxBuildHasher>>,
    // Native weak references let short-lived offset spaces be collected.
    #[no_trace]
    reference_spaces: DomRefCell<Vec<ReferenceSpaceWeakRef>>,
    // Reset listeners must keep their space alive until removal/session end.
    reset_listener_spaces: DomRefCell<Vec<Dom<XRReferenceSpace>>>,
    #[no_trace]
    inline_pointers: DomRefCell<HashMap<i32, InlinePointer>>,
    inline_action_frames: DomRefCell<HashMap<i32, Dom<XRFrame>>>,
    inline_pointer_canvas: MutNullableDom<crate::dom::htmlcanvaselement::HTMLCanvasElement>,
    next_inline_pointer_id: Cell<u32>,
    #[no_trace]
    last_inline_pose: DomRefCell<Option<webxr_api::ViewerPose>>,
    #[no_trace]
    viewport_scales: DomRefCell<HashMap<u64, webxr_api::ViewportScale>>,
    active_view_ids: DomRefCell<Vec<u64>>,
}

impl XRSession {
    pub(crate) fn request_viewport_scale(&self, id: u64, scale: Option<f64>) {
        if self.is_immersive() && !self.session.borrow().supports_dynamic_viewport_scaling() { return; }
        if let Some(state) = self.viewport_scales.borrow_mut().get_mut(&id) { state.request(scale); }
    }

    pub(crate) fn commit_viewport_scale(&self, id: u64) -> f64 {
        self.viewport_scales.borrow_mut().get_mut(&id).map_or(1.0, |state| state.commit())
    }

    pub(crate) fn current_viewport_scale(&self, index: usize) -> f64 {
        self.active_view_ids.borrow().get(index).and_then(|id| self.viewport_scales.borrow().get(id).map(|state| state.current())).unwrap_or(1.0)
    }

    /// A document leaving the active browsing context must release its device,
    /// including when the document remains alive in session history.
    pub(crate) fn end_for_document_deactivation(&self, cx: &mut JSContext) {
        self.ended.set(true);
        self.clear_pending_operations(cx);
        // An earlier end() may still be waiting for a frame acknowledgement.
        // A deactivated document cannot keep that render loop progressing.
        if !self.shutdown_complete.get() {
            self.session.borrow_mut().abort_session();
        }
    }

    fn clear_pending_operations(&self, cx: &mut JSContext) {
        *self.viewer_cache.borrow_mut() = ViewerCache::default();
        self.raf_callback_list.borrow_mut().clear();
        self.pending_render_state.set(None);
        // Keep the currently executing callback list intact. Its remaining
        // callbacks and explicit cancellations still belong to this frame.
        rooted!(&in(cx) let rates = self.update_framerate_promises.borrow_mut()
            .drain().map(|(_, promise)| promise).collect::<Vec<_>>());
        rooted!(&in(cx) let hit_tests = self.pending_hit_test_promises.borrow_mut()
            .drain().map(|(_, promise)| promise).collect::<Vec<_>>());
        for promise in rates.iter().chain(hit_tests.iter()) {
            promise.reject_error(cx, Error::InvalidState(None));
        }
    }

    fn new_inherited(
        session: Session,
        render_state: &XRRenderState,
        input_sources: &XRInputSourceArray,
        mode: XRSessionMode,
    ) -> XRSession {
        let nominal_rate = if mode == XRSessionMode::Inline { None } else { session.nominal_frame_rate() };
        XRSession {
            eventtarget: EventTarget::new_inherited(),
            blend_mode: session.environment_blend_mode().convert(),
            mode,
            visibility_state: Cell::new(XRVisibilityState::Visible),
            viewer_space: Default::default(),
            session: DomRefCell::new(session),
            frame_requested: Cell::new(false),
            pending_render_state: MutNullableDom::new(None),
            active_render_state: MutDom::new(render_state),
            inline_projection_matrix: Default::default(),
            viewer_cache: DomRefCell::new(ViewerCache::default()),

            next_raf_id: Cell::new(0),
            raf_callback_list: DomRefCell::new(vec![]),
            current_raf_callback_list: DomRefCell::new(vec![]),
            input_sources: Dom::from_ref(input_sources),
            end_promises: DomRefCell::new(vec![]),
            ended: Cell::new(false),
            shutdown_complete: Cell::new(false),
            next_hit_test_id: Cell::new(HitTestId(0)),
            pending_hit_test_promises: DomRefCell::new(HashMapTracedValues::new_fx()),
            outside_raf: Cell::new(true),
            input_frames: DomRefCell::new(HashMap::new()),
            active_input_actions: DomRefCell::new(HashMap::new()),
            framerate: Cell::new(nominal_rate),
            next_framerate_request_id: Cell::new(0),
            update_framerate_promises: DomRefCell::new(HashMapTracedValues::new_fx()),
            reference_spaces: DomRefCell::new(Vec::new()),
            reset_listener_spaces: DomRefCell::new(Vec::new()),
            inline_pointers: DomRefCell::new(HashMap::new()),
            inline_action_frames: DomRefCell::new(HashMap::new()),
            inline_pointer_canvas: Default::default(),
            next_inline_pointer_id: Cell::new(u32::MAX),
            last_inline_pose: DomRefCell::new(None),
            viewport_scales: DomRefCell::new(HashMap::new()),
            active_view_ids: DomRefCell::new(Vec::new()),
        }
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        session: Session,
        mode: XRSessionMode,
    ) -> DomRoot<XRSession> {
        let ivfov = if mode == XRSessionMode::Inline {
            Some(FRAC_PI_2)
        } else {
            None
        };
        let render_state = XRRenderState::new(cx, window, 0.1, 1000.0, ivfov, None, Vec::new());
        let input_sources = XRInputSourceArray::new(cx, window);
        let ret = reflect_weak_referenceable_dom_object(
            cx,
            Rc::new(XRSession::new_inherited(
                session,
                &render_state,
                &input_sources,
                mode,
            )),
            window,
        );
        if mode == XRSessionMode::Inline && window.Document().Hidden() {
            ret.visibility_state.set(XRVisibilityState::Hidden);
        }
        ret.attach_event_handler();
        ret.setup_raf_loop();
        ret
    }

    pub(crate) fn with_session<R, F: FnOnce(&Session) -> R>(&self, with: F) -> R {
        let session = self.session.borrow();
        with(&session)
    }

    pub(crate) fn is_ended(&self) -> bool {
        self.ended.get()
    }

    pub(crate) fn viewer_frame_state(&self, frame: &Frame, advance: bool) -> ViewerFrameState {
        if self.is_ended() || !advance {
            return self.viewer_cache.borrow().clone().observe(frame);
        }
        self.viewer_cache.borrow_mut().observe(frame)
    }

    pub(crate) fn update_inline_visibility(&self, cx: &mut JSContext, hidden: bool) {
        debug_assert!(!self.is_immersive());
        self.change_visibility(cx, if hidden {
            XRVisibilityState::Hidden
        } else {
            XRVisibilityState::Visible
        });
    }

    fn change_visibility(&self, cx: &mut JSContext, visibility: XRVisibilityState) {
        if self.is_ended() || self.visibility_state.get() == visibility {
            return;
        }
        self.visibility_state.set(visibility);
        if visibility == XRVisibilityState::Hidden {
            self.cancel_inline_pointers(cx);
        }
        let event = XRSessionEvent::new(cx, self.global().as_window(),
            atom!("visibilitychange"), false, false, self);
        event.upcast::<Event>().fire(cx, self.upcast());
        self.dirty_layers();
    }

    pub(crate) fn poses_visible(&self) -> bool {
        self.visibility_state.get() != XRVisibilityState::Hidden
    }

    pub(crate) fn input_poses_visible(&self) -> bool {
        self.visibility_state.get() == XRVisibilityState::Visible
    }

    pub(crate) fn is_immersive(&self) -> bool {
        self.mode != XRSessionMode::Inline
    }

    // https://immersive-web.github.io/layers/#feature-descriptor-layers
    pub(crate) fn has_layers_feature(&self) -> bool {
        // We do not support creating layers other than projection layers
        // https://github.com/servo/servo/issues/27493
        false
    }

    fn setup_raf_loop(&self) {
        let this = Trusted::new(self);
        let global = self.global();
        let task_source = global
            .task_manager()
            .dom_manipulation_task_source()
            .to_sendable();
        let callback = ProfileGenericCallback::new(move |message| {
            let frame: Frame = message.unwrap();
            let this = this.clone();
            task_source.queue(task!(xr_raf_callback: move |cx| {
                this.root().raf_callback(cx, frame, CrossProcessInstant::now());
            }));
        })
        .expect("Could not create callback");
        self.session.borrow_mut().set_frame_dest(callback);

        self.session.borrow_mut().start_render_loop();
    }

    pub(crate) fn is_outside_raf(&self) -> bool {
        self.outside_raf.get()
    }

    fn attach_event_handler(&self) {
        let this = Trusted::new(self);
        let global = self.global();
        let task_source = global
            .task_manager()
            .dom_manipulation_task_source()
            .to_sendable();
        let callback = ProfileGenericCallback::new(move |message| {
            let this = this.clone();
            task_source.queue(task!(xr_event_callback: move |cx| {
                this.root().event_callback(cx, message.unwrap());
            }))
        })
        .expect("Could not create callback");

        // request animation frame
        self.session.borrow_mut().set_event_dest(callback);
    }

    // Must be called after the promise for session creation is resolved
    // https://github.com/immersive-web/webxr/issues/961
    //
    // This enables content that assumes all input sources are accompanied
    // by an inputsourceschange event to work properly. Without
    pub(crate) fn setup_initial_inputs(&self) {
        let initial_inputs = self.session.borrow().initial_inputs().to_owned();

        if initial_inputs.is_empty() {
            // do not fire an empty event
            return;
        }

        let this = Trusted::new(self);
        // Queue a task so that it runs after resolve()'s microtasks complete
        // so that content has a chance to attach a listener for inputsourceschange
        self.global()
            .task_manager()
            .dom_manipulation_task_source()
            .queue(task!(session_initial_inputs: move |cx| {
                let this = this.root();
                if this.is_ended() {
                    return;
                }
                this.input_sources.add_input_sources(cx, &this, &initial_inputs);
            }));
    }

    fn event_callback(&self, cx: &mut JSContext, event: XREvent) {
        if self.shutdown_complete.get() {
            return;
        }
        match event {
            XREvent::SessionEnd => {
                // https://immersive-web.github.io/webxr/#shut-down-the-session
                // Step 2
                self.ended.set(true);
                self.shutdown_complete.set(true);
                self.clear_pending_operations(cx);
                self.cancel_inline_pointers(cx);
                self.input_sources.clear(cx);
                self.input_frames.borrow_mut().clear();
                self.active_input_actions.borrow_mut().clear();
                self.reference_spaces.borrow_mut().clear();
                self.reset_listener_spaces.borrow_mut().clear();
                // Step 3-4
                self.global()
                    .as_window()
                    .Navigator(cx)
                    .Xr(cx)
                    .end_session(self);
                // Step 6 is happening n the XR session
                // https://immersive-web.github.io/webxr/#dom-xrsession-end step 3
                rooted!(&in(cx) let promises = mem::take(&mut *self.end_promises.borrow_mut()));
                for promise in promises.iter() {
                    promise.resolve_native(cx, &());
                }
                // Step 7
                let event = XRSessionEvent::new(
                    cx,
                    self.global().as_window(),
                    atom!("end"),
                    false,
                    false,
                    self,
                );
                event.upcast::<Event>().fire(cx, self.upcast());
            },
            XREvent::Select(input, kind, ty, frame) => {
                use stylo_atoms::Atom;
                const START_ATOMS: [Atom; 2] = [atom!("selectstart"), atom!("squeezestart")];
                const EVENT_ATOMS: [Atom; 2] = [atom!("select"), atom!("squeeze")];
                const END_ATOMS: [Atom; 2] = [atom!("selectend"), atom!("squeezeend")];

                // https://immersive-web.github.io/webxr/#primary-action
                let source = self.input_sources.find(input);
                let atom_index = if kind == SelectKind::Squeeze { 1 } else { 0 };
                if let Some(source) = source {
                    let mask = 1 << atom_index;
                    if ty == SelectEvent::Start {
                        if self.is_ended() {
                            return;
                        }
                        let mut actions = self.active_input_actions.borrow_mut();
                        let active = actions.entry(input).or_default();
                        if *active & mask != 0 {
                            return;
                        }
                        *active |= mask;
                    } else {
                        let mut actions = self.active_input_actions.borrow_mut();
                        let Some(active) = actions.get_mut(&input) else { return; };
                        if *active & mask == 0 {
                            return;
                        }
                        *active &= !mask;
                        if *active == 0 {
                            actions.remove(&input);
                        }
                    }
                    let frame = XRFrame::new(cx, self.global().as_window(), self, frame);
                    frame.set_active(true);
                    self.apply_frame_updates(cx, &frame);
                    if ty == SelectEvent::Start {
                        let event = XRInputSourceEvent::new(
                            cx,
                            self.global().as_window(),
                            START_ATOMS[atom_index].clone(),
                            false,
                            false,
                            &frame,
                            &source,
                        );
                        event.upcast::<Event>().fire(cx, self.upcast());
                    } else {
                        if ty == SelectEvent::Select && !self.is_ended() {
                            let event = XRInputSourceEvent::new(
                                cx,
                                self.global().as_window(),
                                EVENT_ATOMS[atom_index].clone(),
                                false,
                                false,
                                &frame,
                                &source,
                            );
                            event.upcast::<Event>().fire(cx, self.upcast());
                        }
                        let event = XRInputSourceEvent::new(
                            cx,
                            self.global().as_window(),
                            END_ATOMS[atom_index].clone(),
                            false,
                            false,
                            &frame,
                            &source,
                        );
                        event.upcast::<Event>().fire(cx, self.upcast());
                    }
                    frame.set_active(false);
                }
            },
            XREvent::VisibilityChange(v) => {
                let v = match v {
                    Visibility::Visible => XRVisibilityState::Visible,
                    Visibility::VisibleBlurred => XRVisibilityState::Visible_blurred,
                    Visibility::Hidden => XRVisibilityState::Hidden,
                };
                self.change_visibility(cx, v);
            },
            XREvent::AddInput(info) => {
                if !self.is_ended() {
                    self.input_sources.add_input_sources(cx, self, &[info]);
                }
            },
            XREvent::RemoveInput(id) => {
                self.active_input_actions.borrow_mut().remove(&id);
                self.input_sources.remove_input_source(cx, self, id);
            },
            XREvent::UpdateInput(id, source) => {
                self.active_input_actions.borrow_mut().remove(&id);
                if !self.is_ended() {
                    self.input_sources.add_remove_input_source(cx, self, id, source);
                }
            },
            XREvent::InputChanged(id, frame) => {
                self.input_frames.borrow_mut().insert(id, frame);
            },
            XREvent::ReferenceSpaceChanged(base_space, transform) => {
                self.fire_reference_space_reset(cx, base_space, transform);
            },
        }
    }

    /// <https://immersive-web.github.io/webxr/#xr-animation-frame>
    fn raf_callback(&self, cx: &mut JSContext, mut frame: Frame, time: CrossProcessInstant) {
        debug!("WebXR RAF callback {:?}", frame);
        if self.is_ended() {
            // Acknowledge already queued device frames so orderly teardown can
            // finish, without applying pending state or retaining new callbacks.
            self.session.borrow_mut().render_animation_frame();
            return;
        }
        // Inline frames share the exact callback timestamp. Backends without
        // explicit device-clock conversion use the observed frame time as an
        // estimate, never an opaque runtime epoch in a DOM timestamp.
        if !self.is_immersive() || frame.predicted_display_instant.is_none() {
            frame.predicted_display_instant = Some(time);
        }


        // Step 1-2 happen in the xebxr device thread

        // Step 3
        if let Some(pending) = self.pending_render_state.take() {
            // https://immersive-web.github.io/webxr/#apply-the-pending-render-state
            // (Steps 1-4 are implicit)
            // Step 5
            self.active_render_state.set(&pending);
        }

        // The output canvas can resize independently of updateRenderState.
        // Derive the inline projection from its current drawing-buffer size.
        if !self.is_immersive() {
            self.update_inline_projection_matrix();
            *self.last_inline_pose.borrow_mut() = frame.pose.clone();
            self.validate_inline_pointers(cx);
            frame.inputs.extend(self.inline_pointer_frames(frame.pose.as_ref()));
        }

        // TODO: how does this fit the webxr spec?
        for event in frame.events.drain(..) {
            self.handle_frame_event(cx, event);
        }

        // Hidden sessions do not run animation callbacks. Inline callbacks also
        // pause while this XRSystem owns an immersive session. Keep consuming
        // device frames so end() and native events remain responsive.
        if self.is_ended() || !self.poses_visible() ||
            (!self.is_immersive() && self.global().as_window().Navigator(cx).Xr(cx).has_active_immersive_session())
        {
            self.session.borrow_mut().render_animation_frame();
            return;
        }

        // Pending state has already been applied, even when no layer can render.
        // Keep queued callbacks until an output layer becomes available; frames
        // still need acknowledgement for native events and orderly shutdown.
        if !self.active_render_state.get().should_render(self.is_immersive()) {
            self.session.borrow_mut().render_animation_frame();
            return;
        }

        // Step 4
        // TODO: what should this check be?
        // This is checking that the new render state has the same
        // layers as the frame.
        // Related to https://github.com/immersive-web/webxr/issues/1051
        if !self
            .active_render_state
            .get()
            .has_sub_images(&frame.sub_images[..])
        {
            // If the frame has different layers than the render state,
            // we just return early, drawing a blank frame.
            // This can result in flickering when the render state is changed.
            // TODO: it would be better to not render anything until the next frame.
            warn!("Rendering blank XR frame");
            self.session.borrow_mut().render_animation_frame();
            return;
        }

        // Step 5: XXXManishearth handle inline session

        // Step 6-7
        {
            let mut current = self.current_raf_callback_list.borrow_mut();
            assert!(current.is_empty());
            mem::swap(&mut *self.raf_callback_list.borrow_mut(), &mut current);
        }

        let time = self
            .global()
            .performance(cx)
            .to_dom_high_res_time_stamp(time);
        let frame = XRFrame::new_animation(cx, self.global().as_window(), self, frame);

        if let Some(ids) = frame.view_ids() {
            let mut scales = self.viewport_scales.borrow_mut();
            scales.retain(|id, _| ids.contains(id));
            for id in &ids { scales.entry(*id).or_default().begin_frame(); }
            *self.active_view_ids.borrow_mut() = ids;
        }

        // Step 8-9
        frame.set_active(true);
        frame.set_animation_frame(true);

        // Step 10
        self.apply_frame_updates(cx, &frame);

        // TODO: how does this fit with the webxr and xr layers specs?
        self.layers_begin_frame(cx, &frame);

        // Step 11-12
        self.outside_raf.set(false);
        let len = self.current_raf_callback_list.borrow().len();
        for i in 0..len {
            rooted!(&in(cx) let callback = self.current_raf_callback_list.borrow()[i].1.clone());
            if let Some(ref callback) = *callback {
                let _ = callback.Call__(cx, time, &frame, ExceptionHandling::Report);
            }
        }
        self.outside_raf.set(true);
        *self.current_raf_callback_list.borrow_mut() = vec![];

        // TODO: how does this fit with the webxr and xr layers specs?
        self.layers_end_frame(&frame);

        // Step 13
        frame.set_active(false);

        // TODO: how does this fit the webxr spec?
        self.session.borrow_mut().render_animation_frame();
    }

    fn update_inline_projection_matrix(&self) {
        debug_assert!(!self.is_immersive());
        let render_state = self.active_render_state.get();
        let size = if let Some(base) = render_state.GetBaseLayer() {
            base.size()
        } else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        let mut clip_planes = util::ClipPlanes::default();
        let near = *render_state.DepthNear() as f32;
        let far = *render_state.DepthFar() as f32;
        clip_planes.update(near, far);
        let top = *render_state
            .GetInlineVerticalFieldOfView()
            .expect("IVFOV should be non null for inline sessions") /
            2.;
        let top = near * top.tan() as f32;
        let bottom = -top;
        let right = top * size.width as f32 / size.height as f32;
        let left = -right;
        let matrix = util::frustum_to_projection_matrix(left, right, top, bottom, clip_planes);
        *self.inline_projection_matrix.borrow_mut() = matrix;
    }

    /// Constructs a View suitable for inline sessions using the inlineVerticalFieldOfView and canvas size
    pub(crate) fn inline_view(&self) -> View<Viewer> {
        debug_assert!(!self.is_immersive());
        View {
            // Inline views have no offset
            transform: RigidTransform3D::identity(),
            projection: *self.inline_projection_matrix.borrow(),
        }
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.session.borrow().id()
    }

    pub(crate) fn dirty_layers(&self) {
        if let Some(layer) = self.RenderState().GetBaseLayer() {
            layer.context().mark_as_dirty();
        }
    }

    // TODO: how does this align with the layers spec?
    fn layers_begin_frame(&self, cx: &mut JSContext, frame: &XRFrame) {
        if let Some(layer) = self.active_render_state.get().GetBaseLayer() {
            layer.begin_frame(cx, frame);
        }
        self.active_render_state.get().with_layers(|layers| {
            for layer in layers {
                layer.begin_frame(cx, frame);
            }
        });
    }

    // TODO: how does this align with the layers spec?
    fn layers_end_frame(&self, frame: &XRFrame) {
        if let Some(layer) = self.active_render_state.get().GetBaseLayer() {
            layer.end_frame(frame);
        }
        self.active_render_state.get().with_layers(|layers| {
            for layer in layers {
                layer.end_frame(frame);
            }
        });
    }

    /// <https://immersive-web.github.io/webxr/#xrframe-apply-frame-updates>
    fn apply_frame_updates(&self, cx: &mut JSContext, frame: &XRFrame) {
        let timestamp = *self.global().performance(cx).to_dom_high_res_time_stamp(CrossProcessInstant::now());
        // <https://www.w3.org/TR/webxr-gamepads-module-1/#xrframe-apply-gamepad-frame-updates>
        for (id, frame) in self.input_frames.borrow_mut().drain() {
            let source = self.input_sources.find(id);
            if let Some(source) = source {
                source.update_gamepad_state(&frame, timestamp);
            }
        }
        // Frame data is authoritative, even when the backend didn't emit a
        // separate InputChanged event (or several events ran before this RAF).
        for input in frame.inputs() {
            if let Some(source) = self.input_sources.find(input.id) {
                source.update_gamepad_state(input, timestamp);
            }
        }
    }

    pub(crate) fn register_reference_space(&self, space: &XRReferenceSpace) {
        let mut spaces = self.reference_spaces.borrow_mut();
        spaces.retain(ReferenceSpaceWeakRef::is_alive);
        spaces.push(ReferenceSpaceWeakRef::new(space));
    }

    pub(crate) fn retain_reset_listener_space(&self, space: &XRReferenceSpace) {
        if !self.is_ended() {
            self.reset_listener_spaces.borrow_mut().push(Dom::from_ref(space));
        }
    }

    pub(crate) fn release_reset_listener_space(&self, space: &XRReferenceSpace) {
        self.reset_listener_spaces.borrow_mut().retain(|entry| !std::ptr::eq(&**entry, space));
    }

    fn fire_reference_space_reset(&self, cx: &mut JSContext, base: webxr_api::BaseSpace,
        transform: Option<ApiRigidTransform>) {
        if self.is_ended() { return; }
        self.viewer_cache.borrow_mut().reset(base, transform);
        // Author handlers may create offset spaces, request more spaces or end
        // the session. Snapshot roots and release all list borrows before JS.
        let spaces: Vec<_> = self.reference_spaces.borrow().iter().filter_map(ReferenceSpaceWeakRef::root)
            .filter(|space| space.space().base == base).collect();
        for space in spaces {
            if self.is_ended() { break; }
            let transform = transform.map(|delta| {
                XRRigidTransform::new(cx, self.global().as_window(), space.reset_transform(delta))
            });
            let event = XRReferenceSpaceEvent::new(cx, self.global().as_window(), atom!("reset"),
                false, false, &space, transform.as_deref());
            event.upcast::<Event>().fire(cx, space.upcast());
        }
        self.reference_spaces.borrow_mut().retain(ReferenceSpaceWeakRef::is_alive);
    }

    fn handle_frame_event(&self, cx: &mut JSContext, event: FrameUpdateEvent) {
        match event {
            FrameUpdateEvent::NominalFrameRateChanged(rate) => {
                self.apply_nominal_framerate(cx, rate);
            },
            FrameUpdateEvent::ReferenceSpaceChanged(base, transform) => {
                self.fire_reference_space_reset(cx, base, transform);
            },
            FrameUpdateEvent::HitTestSourceAdded(id) => {
                rooted!(&in(cx) let promise = self.pending_hit_test_promises.borrow_mut().remove(&id));
                if let Some(ref promise) = *promise {
                    let hit_test_source =
                        XRHitTestSource::new(cx, self.global().as_window(), id, self);
                    promise.resolve_native(cx, &hit_test_source);
                } else {
                    warn!(
                        "received hit test add request for unknown hit test {:?}",
                        id
                    )
                }
            },
            _ => self.session.borrow_mut().apply_event(event),
        }
    }

    /// <https://www.w3.org/TR/webxr/#apply-the-nominal-frame-rate>
    fn apply_nominal_framerate(&self, cx: &mut JSContext, rate: f32) {
        if self.mode == XRSessionMode::Inline || self.ended.get() ||
            !webxr_api::util::valid_frame_rate(rate) || self.framerate.get() == Some(rate)
        {
            return;
        }

        self.session.borrow_mut().apply_event(FrameUpdateEvent::NominalFrameRateChanged(rate));
        self.framerate.set(Some(rate));

        let event = XRSessionEvent::new(
            cx,
            self.global().as_window(),
            Atom::from("frameratechange"),
            false,
            false,
            self,
        );
        event.upcast::<Event>().fire(cx, self.upcast());
    }
}

impl XRSessionMethods<crate::DomTypeHolder> for XRSession {
    // https://immersive-web.github.io/webxr/#eventdef-xrsession-end
    event_handler!(end, GetOnend, SetOnend);

    // https://immersive-web.github.io/webxr/#eventdef-xrsession-select
    event_handler!(select, GetOnselect, SetOnselect);

    // https://immersive-web.github.io/webxr/#eventdef-xrsession-selectstart
    event_handler!(selectstart, GetOnselectstart, SetOnselectstart);

    // https://immersive-web.github.io/webxr/#eventdef-xrsession-selectend
    event_handler!(selectend, GetOnselectend, SetOnselectend);

    // https://immersive-web.github.io/webxr/#eventdef-xrsession-squeeze
    event_handler!(squeeze, GetOnsqueeze, SetOnsqueeze);

    // https://immersive-web.github.io/webxr/#eventdef-xrsession-squeezestart
    event_handler!(squeezestart, GetOnsqueezestart, SetOnsqueezestart);

    // https://immersive-web.github.io/webxr/#eventdef-xrsession-squeezeend
    event_handler!(squeezeend, GetOnsqueezeend, SetOnsqueezeend);

    // https://immersive-web.github.io/webxr/#eventdef-xrsession-visibilitychange
    event_handler!(
        visibilitychange,
        GetOnvisibilitychange,
        SetOnvisibilitychange
    );

    // https://immersive-web.github.io/webxr/#eventdef-xrsession-inputsourceschange
    event_handler!(
        inputsourceschange,
        GetOninputsourceschange,
        SetOninputsourceschange
    );

    // https://www.w3.org/TR/webxr/#dom-xrsession-onframeratechange
    event_handler!(frameratechange, GetOnframeratechange, SetOnframeratechange);

    /// <https://immersive-web.github.io/webxr/#dom-xrsession-renderstate>
    fn RenderState(&self) -> DomRoot<XRRenderState> {
        self.active_render_state.get()
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrsession-updaterenderstate>
    fn UpdateRenderState(&self, cx: &mut JSContext, init: &XRRenderStateInit) -> ErrorResult {
        // Step 2
        if self.ended.get() {
            return Err(Error::InvalidState(None));
        }
        // Step 3:
        if let Some(Some(ref layer)) = init.baseLayer &&
            Dom::from_ref(layer.session()) != Dom::from_ref(self)
        {
            return Err(Error::InvalidState(None));
        }

        // Step 4:
        if init.inlineVerticalFieldOfView.is_some() && self.is_immersive() {
            return Err(Error::InvalidState(None));
        }

        // https://immersive-web.github.io/layers/#updaterenderstatechanges
        // Step 1.
        if init.baseLayer.is_some() && (self.has_layers_feature() || init.layers.is_some()) {
            return Err(Error::NotSupported(None));
        }

        if let Some(Some(ref layers)) = init.layers {
            // Step 2
            for layer in layers {
                let count = layers
                    .iter()
                    .filter(|other| other.layer_id() == layer.layer_id())
                    .count();
                if count > 1 {
                    return Err(Error::Type(c"Duplicate entry in WebXR layers".to_owned()));
                }
            }

            // Step 3
            for layer in layers {
                if layer.session() != self {
                    return Err(Error::Type(
                        c"Layer from different session in WebXR layers".to_owned(),
                    ));
                }
            }
        }

        // Step 4-5
        let pending = self
            .pending_render_state
            .or_init(|| self.active_render_state.get().clone_object(cx));

        // Step 6
        if let Some(ref layers) = init.layers {
            let layers = layers.as_deref().unwrap_or_default();
            pending.set_base_layer(None);
            pending.set_layers(layers.iter().map(|x| &**x).collect());
            let layers = layers
                .iter()
                .filter_map(|layer| {
                    let context_id = WebXRContextId::from(layer.context_id());
                    let layer_id = layer.layer_id()?;
                    Some((context_id, layer_id))
                })
                .collect();
            self.session.borrow_mut().set_layers(layers);
        }

        // End of https://immersive-web.github.io/layers/#updaterenderstatechanges

        if let Some(near) = init.depthNear {
            let mut near = *near;
            // Step 8 from #apply-the-pending-render-state
            // this may need to be changed if backends wish to impose
            // further constraints
            if near < 0. {
                near = 0.;
            }
            pending.set_depth_near(near);
        }
        if let Some(far) = init.depthFar {
            let mut far = *far;
            // Step 9 from #apply-the-pending-render-state
            // this may need to be changed if backends wish to impose
            // further constraints
            // currently the maximum is infinity, so just check that
            // the value is non-negative
            if far < 0. {
                far = 0.;
            }
            pending.set_depth_far(far);
        }
        if let Some(fov) = init.inlineVerticalFieldOfView {
            let mut fov = *fov;
            // Step 10 from #apply-the-pending-render-state
            // this may need to be changed if backends wish to impose
            // further constraints
            if fov <= 0. {
                fov = 0.0001;
            } else if fov >= PI {
                fov = PI - 0.0001;
            }
            pending.set_inline_vertical_fov(fov);
        }
        if let Some(ref layer) = init.baseLayer {
            pending.set_base_layer(layer.as_deref());
            pending.set_layers(Vec::new());
            let layers = layer
                .iter()
                .filter_map(|layer| {
                    let context_id = WebXRContextId::from(layer.context_id());
                    let layer_id = layer.layer_id()?;
                    Some((context_id, layer_id))
                })
                .collect();
            self.session.borrow_mut().set_layers(layers);
        }

        if init.depthFar.is_some() || init.depthNear.is_some() {
            self.session
                .borrow_mut()
                .update_clip_planes(*pending.DepthNear() as f32, *pending.DepthFar() as f32);
        }

        Ok(())
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrsession-requestanimationframe>
    fn RequestAnimationFrame(&self, callback: RootedCallback<XRFrameRequestCallback>) -> i32 {
        if self.is_ended() {
            return 0;
        }
        // queue up RAF callback, obtain ID
        let mut raf_id = self.next_raf_id.get().wrapping_add(1);
        if raf_id == 0 {
            raf_id = 1;
        }
        self.next_raf_id.set(raf_id);
        self.raf_callback_list
            .borrow_mut()
            .push((raf_id, Some(callback.to_traced())));

        raf_id
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrsession-cancelanimationframe>
    fn CancelAnimationFrame(&self, frame: i32) {
        self.raf_callback_list.borrow_mut().retain(|pair| pair.0 != frame);

        let mut list = self.current_raf_callback_list.borrow_mut();
        if let Some(pair) = list.iter_mut().find(|pair| pair.0 == frame) {
            pair.1 = None;
        }
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrsession-environmentblendmode>
    fn EnvironmentBlendMode(&self) -> XREnvironmentBlendMode {
        self.blend_mode
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrsession-visibilitystate>
    fn VisibilityState(&self) -> XRVisibilityState {
        self.visibility_state.get()
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrsession-requestreferencespace>
    fn RequestReferenceSpace(
        &self,
        cx: &mut CurrentRealm,
        ty: XRReferenceSpaceType,
    ) -> RootedPromise {
        let p = Promise::new_in_realm(cx);

        // https://immersive-web.github.io/webxr/#create-a-reference-space

        // XXXManishearth reject based on session type
        // https://github.com/immersive-web/webxr/blob/master/spatial-tracking-explainer.md#practical-usage-guidelines

        if !self.is_immersive() &&
            (ty == XRReferenceSpaceType::Bounded_floor || ty == XRReferenceSpaceType::Unbounded)
        {
            p.reject_error(cx, Error::NotSupported(None));
            return p;
        }

        match ty {
            XRReferenceSpaceType::Unbounded => {
                // XXXmsub2 figure out how to support this
                p.reject_error(cx, Error::NotSupported(None))
            },
            ty => {
                if ty != XRReferenceSpaceType::Viewer &&
                    (!self.is_immersive() || ty != XRReferenceSpaceType::Local)
                {
                    let s = ty.as_str();
                    if !self
                        .session
                        .borrow()
                        .granted_features()
                        .iter()
                        .any(|f| *f == s)
                    {
                        p.reject_error(cx, Error::NotSupported(None));
                        return p;
                    }
                }
                if ty == XRReferenceSpaceType::Bounded_floor {
                    let space = XRBoundedReferenceSpace::new(cx, self.global().as_window(), self);
                    p.resolve_native(cx, &space);
                } else {
                    let space = XRReferenceSpace::new(cx, self.global().as_window(), self, ty);
                    p.resolve_native(cx, &space);
                }
            },
        }
        p
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrsession-inputsources>
    fn InputSources(&self) -> DomRoot<XRInputSourceArray> {
        DomRoot::from_ref(&*self.input_sources)
    }

    /// <https://immersive-web.github.io/webxr/#dom-xrsession-end>
    fn End(&self, cx: &mut CurrentRealm) -> RootedPromise {
        let p = Promise::new_in_realm(cx);
        if self.ended.get() {
            p.reject_error(cx, Error::InvalidState(None));
            return p;
        }
        self.end_promises.borrow_mut().push(p.to_traced());
        // This is duplicated in event_callback since this should
        // happen ASAP for end() but can happen later if the device
        // shuts itself down
        self.ended.set(true);
        self.clear_pending_operations(cx);
        self.cancel_inline_pointers(cx);
        self.global()
            .as_window()
            .Navigator(cx)
            .Xr(cx)
            .end_session(self);
        self.session.borrow_mut().end_session();
        // IDs are device identifiers, not array indices. Keep sources reachable
        // until the device's cancellation events and SessionEnd have arrived.
        self.input_sources.disconnect_all();
        p
    }

    /// <https://immersive-web.github.io/hit-test/#dom-xrsession-requesthittestsource>
    fn RequestHitTestSource(
        &self,
        cx: &mut CurrentRealm,
        options: &XRHitTestOptionsInit,
    ) -> RootedPromise {
        let p = Promise::new_in_realm(cx);

        if !self
            .session
            .borrow()
            .granted_features()
            .iter()
            .any(|f| f == "hit-test")
        {
            p.reject_error(cx, Error::NotSupported(None));
            return p;
        }

        if self.is_ended() {
            p.reject_error(cx, Error::InvalidState(None));
            return p;
        }

        let id = self.next_hit_test_id.get();
        self.next_hit_test_id.set(HitTestId(id.0 + 1));

        let space = options.space.space();
        let ray = if let Some(ref ray) = options.offsetRay {
            ray.ray()
        } else {
            Ray {
                origin: Vector3D::new(0., 0., 0.),
                direction: Vector3D::new(0., 0., -1.),
            }
        };

        let mut types = EntityTypes::default();

        if let Some(ref tys) = options.entityTypes {
            for ty in tys {
                match ty {
                    XRHitTestTrackableType::Point => types.point = true,
                    XRHitTestTrackableType::Plane => types.plane = true,
                    XRHitTestTrackableType::Mesh => types.mesh = true,
                }
            }
        } else {
            types.plane = true;
        }

        let source = HitTestSource {
            id,
            space,
            ray,
            types,
        };
        self.pending_hit_test_promises
            .borrow_mut()
            .insert(id, p.to_traced());

        self.session.borrow().request_hit_test(source);

        p
    }

    /// <https://www.w3.org/TR/webxr-ar-module-1/#dom-xrsession-interactionmode>
    fn InteractionMode(&self) -> XRInteractionMode {
        // Until Servo supports WebXR sessions on mobile phones or similar non-XR devices,
        // this should always be world space
        XRInteractionMode::World_space
    }

    /// <https://www.w3.org/TR/webxr/#dom-xrsession-framerate>
    fn GetFrameRate(&self) -> Option<Finite<f32>> {
        if self.mode == XRSessionMode::Inline {
            None
        } else {
            self.framerate.get().and_then(Finite::new)
        }
    }

    /// <https://www.w3.org/TR/webxr/#dom-xrsession-supportedframerates>
    fn GetSupportedFrameRates(
        &self,
        cx: &mut JSContext,
    ) -> Option<RootedTraceableBox<HeapFloat32Array>> {
        let session = self.session.borrow();
        if self.mode == XRSessionMode::Inline || session.supported_frame_rates().is_empty() {
            None
        } else {
            let framerates = session.supported_frame_rates();
            rooted!(&in(cx) let mut array = ptr::null_mut::<JSObject>());
            Some(
                create_buffer_source(cx, framerates, array.handle_mut())
                    .expect("Failed to construct supported frame rates array"),
            )
        }
    }

    /// <https://www.w3.org/TR/webxr/#dom-xrsession-enabledfeatures>
    fn EnabledFeatures(&self, cx: &mut JSContext, retval: MutableHandleValue) {
        let session = self.session.borrow();
        let features = session.granted_features();
        to_frozen_array(cx, features, retval)
    }

    /// <https://www.w3.org/TR/webxr/#dom-xrsession-issystemkeyboardsupported>
    fn IsSystemKeyboardSupported(&self) -> bool {
        // Support for this only exists on Meta headsets (no desktop support)
        // so this will always be false until that changes
        false
    }

    /// <https://www.w3.org/TR/webxr/#dom-xrsession-updatetargetframerate>
    fn UpdateTargetFrameRate(&self, cx: &mut CurrentRealm, rate: Finite<f32>) -> RootedPromise {
        let promise = Promise::new_in_realm(cx);
        {
            let session = self.session.borrow();
            let supported_frame_rates = session.supported_frame_rates();

            if self.mode == XRSessionMode::Inline ||
                supported_frame_rates.is_empty() ||
                self.ended.get()
            {
                promise.reject_error(cx, Error::InvalidState(None));
                return promise;
            }

            if !supported_frame_rates.contains(&*rate) {
                promise.reject_error(cx, Error::Type(c"Provided framerate not supported".into()));
                return promise;
            }
        }

        let request_id = self.next_framerate_request_id.get();
        self.next_framerate_request_id.set(request_id.wrapping_add(1));
        self.update_framerate_promises.borrow_mut().insert(request_id, promise.to_traced());

        let this = Trusted::new(self);
        let global = self.global();
        let task_source = global
            .task_manager()
            .dom_manipulation_task_source()
            .to_sendable();

        let callback = ProfileGenericCallback::new(move |message| {
            let this = this.clone();
            task_source.queue(task!(update_session_framerate: move |cx| {
                let session = this.root();
                if !session.update_framerate_promises.borrow().contains_key(&request_id) {
                    return;
                }
                let rate = message.ok().filter(|rate| webxr_api::util::valid_frame_rate(*rate));
                if let Some(rate) = rate {
                    // A rate-change listener can end the session and reject
                    // this operation. Keep it registered until that event ends.
                    session.apply_nominal_framerate(cx, rate);
                }
                rooted!(&in(cx) let promise = session.update_framerate_promises.borrow_mut().remove(&request_id));
                if let Some(ref promise) = *promise {
                    if rate.is_some() {
                        promise.resolve_native(cx, &());
                    } else {
                        promise.reject_error(cx, Error::InvalidState(None));
                    }
                };
            }));
        })
        .expect("Could not create callback");

        self.session.borrow_mut().update_frame_rate(*rate, callback);

        promise
    }
}

// The pose of an object in native-space. Should never be exposed.
pub(crate) type ApiPose = RigidTransform3D<f32, ApiSpace, webxr_api::Native>;
// A transform between objects in some API-space
pub(crate) type ApiRigidTransform = RigidTransform3D<f32, ApiSpace, ApiSpace>;

#[derive(Clone, Copy)]
pub(crate) struct BaseSpace;

pub(crate) type BaseTransform = RigidTransform3D<f32, webxr_api::Native, BaseSpace>;

pub(crate) fn cast_transform<T, U, V, W>(
    transform: RigidTransform3D<f32, T, U>,
) -> RigidTransform3D<f32, V, W> {
    transform.cast_unit()
}

impl Convert<XREnvironmentBlendMode> for EnvironmentBlendMode {
    fn convert(self) -> XREnvironmentBlendMode {
        match self {
            EnvironmentBlendMode::Opaque => XREnvironmentBlendMode::Opaque,
            EnvironmentBlendMode::AlphaBlend => XREnvironmentBlendMode::Alpha_blend,
            EnvironmentBlendMode::Additive => XREnvironmentBlendMode::Additive,
        }
    }
}
