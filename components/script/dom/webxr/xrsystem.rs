/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;

use dom_struct::dom_struct;
use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowMethods;
use js::context::JSContext;
use js::realm::CurrentRealm;
use profile_traits::generic_callback::GenericCallback as ProfileGenericCallback;
use script_bindings::cell::DomRefCell;
use script_bindings::reflector::reflect_dom_object;
use servo_base::id::PipelineId;
use servo_config::pref;
use webxr_api::{Error as XRError, PendingSession, Session, SessionInit, SessionMode};
use stylo_atoms::Atom;

use crate::conversions::Convert;
use crate::dom::bindings::codegen::Bindings::XRSystemBinding::{
    XRSessionInit, XRSessionMode, XRSystemMethods,
};
use crate::dom::bindings::conversions::{ConversionResult, FromJSValConvertible};
use crate::dom::bindings::error::Error;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::refcounted::{Trusted, TrustedPromise};
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::{AsHandleValue, Dom, DomRoot, MutNullableDom};
use crate::dom::eventtarget::EventTarget;
use crate::dom::gamepad::Gamepad;
use crate::dom::promise::{Promise, RootedPromise};
use crate::dom::window::Window;
use crate::dom::xrsession::{InlinePointerAction, ScreenPointerSample, XRSession};
use crate::dom::xrtest::XRTest;
use crate::event_loop::script_thread::ScriptThread;

#[dom_struct]
pub(crate) struct XRSystem {
    eventtarget: EventTarget,
    gamepads: DomRefCell<Vec<Dom<Gamepad>>>,
    pending_immersive_session: Cell<bool>,
    session_request_generation: Cell<u64>,
    device_change_listener_count: Cell<u32>,
    active_immersive_session: MutNullableDom<XRSession>,
    active_inline_sessions: DomRefCell<Vec<Dom<XRSession>>>,
    test: MutNullableDom<XRTest>,
    #[no_trace]
    pipeline: PipelineId,
}

impl XRSystem {
    pub(crate) fn device_change_listener_added(&self) {
        self.device_change_listener_count
            .set(self.device_change_listener_count.get().saturating_add(1));
        self.global().as_window().update_xr_device_observation();
    }

    pub(crate) fn device_change_listener_removed(&self) {
        self.device_change_listener_count
            .set(self.device_change_listener_count.get().saturating_sub(1));
        self.global().as_window().update_xr_device_observation();
    }

    pub(crate) fn clear_device_change_listeners(&self) {
        self.device_change_listener_count.set(0);
        self.global().as_window().update_xr_device_observation();
    }

    pub(crate) fn wants_device_changes(&self) -> bool {
        self.device_change_listener_count.get() != 0
    }

    pub(crate) fn device_changed(&self, cx: &mut JSContext) {
        if self.wants_device_changes() {
            self.upcast::<EventTarget>()
                .fire_event(cx, Atom::from("devicechange"));
        }
    }

    pub(crate) fn has_inline_sessions(&self) -> bool {
        !self.active_inline_sessions.borrow().is_empty()
    }

    pub(crate) fn has_inline_pointer(&self, pointer: i32) -> bool {
        self.active_inline_sessions.borrow().iter().any(|session| session.has_inline_pointer(pointer))
    }

    pub(crate) fn inline_pointer_event(&self, cx: &mut JSContext, pointer: i32,
        action: InlinePointerAction, sample: Option<&ScreenPointerSample>) {
        let sessions: Vec<_> = self.active_inline_sessions.borrow().iter()
            .map(|session| DomRoot::from_ref(&**session)).collect();
        for session in sessions { session.inline_pointer_event(cx, pointer, action, sample); }
    }

    pub(crate) fn cancel_inline_pointers(&self, cx: &mut JSContext) {
        let sessions: Vec<_> = self.active_inline_sessions.borrow().iter()
            .map(|session| DomRoot::from_ref(&**session)).collect();
        for session in sessions { session.cancel_inline_pointers(cx); }
    }

    pub(crate) fn update_inline_visibility(&self, cx: &mut JSContext, hidden: bool) {
        // Event handlers can end sessions; don't retain the list borrow while
        // firing visibility events into script.
        let sessions: Vec<_> = self.active_inline_sessions.borrow().iter()
            .map(|session| DomRoot::from_ref(&**session)).collect();
        for session in sessions {
            session.update_inline_visibility(cx, hidden);
        }
    }

    pub(crate) fn end_sessions_for_document_deactivation(&self) {
        let generation = self.session_request_generation.get();
        self.session_request_generation
            .set(generation.wrapping_add(1));
        self.pending_immersive_session.set(false);
        if let Some(registry) = self.global().as_window().webxr_registry() {
            registry.cancel_document_sessions(self.pipeline, generation);
        }
        if let Some(session) = self.active_immersive_session.get() {
            session.end_for_document_deactivation();
            session.dirty_layers();
        }
        self.active_immersive_session.set(None);
        for session in self.active_inline_sessions.borrow().iter() {
            session.end_for_document_deactivation();
        }
        self.active_inline_sessions.borrow_mut().clear();
    }

    fn new_inherited(pipeline: PipelineId) -> XRSystem {
        XRSystem {
            eventtarget: EventTarget::new_inherited(),
            gamepads: DomRefCell::new(Vec::new()),
            pending_immersive_session: Cell::new(false),
            session_request_generation: Cell::new(1),
            device_change_listener_count: Cell::new(0),
            active_immersive_session: Default::default(),
            active_inline_sessions: DomRefCell::new(Vec::new()),
            test: Default::default(),
            pipeline,
        }
    }

    pub(crate) fn new(cx: &mut JSContext, window: &Window) -> DomRoot<XRSystem> {
        reflect_dom_object(
            cx,
            Box::new(XRSystem::new_inherited(window.pipeline_id())),
            window,
        )
    }

    pub(crate) fn pending_or_active_session(&self) -> bool {
        self.pending_immersive_session.get() || self.active_immersive_session.get().is_some()
    }

    pub(crate) fn has_active_immersive_session(&self) -> bool {
        self.active_immersive_session.get().is_some()
    }

    pub(crate) fn set_pending(&self) {
        self.pending_immersive_session.set(true)
    }

    pub(crate) fn set_active_immersive_session(&self, cx: &mut JSContext, session: &XRSession) {
        // XXXManishearth when we support non-immersive (inline) sessions we should
        // ensure they never reach these codepaths
        self.pending_immersive_session.set(false);
        self.active_immersive_session.set(Some(session));
        self.cancel_inline_pointers(cx);
    }

    /// <https://immersive-web.github.io/webxr/#ref-for-eventdef-xrsession-end>
    pub(crate) fn end_session(&self, session: &XRSession) {
        // Step 3
        if let Some(active) = self.active_immersive_session.get() &&
            Dom::from_ref(&*active) == Dom::from_ref(session)
        {
            self.active_immersive_session.set(None);
            // Dirty the canvas, since it has been skipping this step whilst in immersive
            // mode
            session.dirty_layers();
        }
        self.active_inline_sessions
            .borrow_mut()
            .retain(|sess| Dom::from_ref(&**sess) != Dom::from_ref(session));
    }
}

impl Convert<SessionMode> for XRSessionMode {
    fn convert(self) -> SessionMode {
        match self {
            XRSessionMode::Immersive_vr => SessionMode::ImmersiveVR,
            XRSessionMode::Immersive_ar => SessionMode::ImmersiveAR,
            XRSessionMode::Inline => SessionMode::Inline,
        }
    }
}

impl XRSystemMethods<crate::DomTypeHolder> for XRSystem {
    event_handler!(devicechange, GetOndevicechange, SetOndevicechange);

    /// <https://immersive-web.github.io/webxr/#dom-xr-issessionsupported>
    fn IsSessionSupported(&self, cx: &mut CurrentRealm, mode: XRSessionMode) -> RootedPromise {
        // XXXManishearth this should select an XR device first
        let promise = Promise::new_in_realm(cx);
        if mode == XRSessionMode::Inline {
            promise.resolve_native(cx, &true);
            return promise;
        }
        if !self
            .global()
            .as_window()
            .Document()
            .allowed_to_use_policy_feature(
                net_traits::permissions_policy::PolicyFeature::XrSpatialTracking,
            )
        {
            promise.reject_error(cx, Error::Security(None));
            return promise;
        }
        let mut trusted = Some(TrustedPromise::from(&promise));
        let global = self.global();
        let task_source = global
            .task_manager()
            .dom_manipulation_task_source()
            .to_sendable();

        let callback = ProfileGenericCallback::new(move |message| {
            // router doesn't know this is only called once
            let trusted = if let Some(trusted) = trusted.take() {
                trusted
            } else {
                error!("supportsSession callback called twice!");
                return;
            };
            let message: Result<(), webxr_api::Error> = if let Ok(message) = message {
                message
            } else {
                error!("supportsSession callback given incorrect payload");
                return;
            };
            if let Ok(()) = message {
                task_source.queue(trusted.resolve_task(true));
            } else {
                task_source.queue(trusted.resolve_task(false));
            };
        })
        .expect("Could not create callback");

        if let Some(mut r) = global.as_window().webxr_registry() {
            r.supports_session(mode.convert(), callback);
        } else {
            promise.resolve_native(cx, &false);
        }

        promise
    }

    /// <https://immersive-web.github.io/webxr/#dom-xr-requestsession>
    fn RequestSession(
        &self,
        realm: &mut CurrentRealm,
        mode: XRSessionMode,
        init: &XRSessionInit,
    ) -> RootedPromise {
        let global = self.global();
        let window = global.as_window();
        let promise = Promise::new_in_realm(realm);
        if !window.Document().is_fully_active() {
            promise.reject_error(realm, Error::InvalidState(None));
            return promise;
        }

        let generation = self.session_request_generation.get();
        if mode != XRSessionMode::Inline {
            if !ScriptThread::is_user_interacting() {
                if pref!(dom_webxr_unsafe_assume_user_intent) {
                    warn!(
                        "The dom.webxr.unsafe-assume-user-intent preference assumes user intent to enter WebXR."
                    );
                } else {
                    promise.reject_error(realm, Error::Security(None));
                    return promise;
                }
            }

            if self.pending_or_active_session() {
                promise.reject_error(realm, Error::InvalidState(None));
                return promise;
            }

            self.set_pending();
        }

        let mut required_features = vec![];
        let mut optional_features = vec![];

        if let Some(ref r) = init.requiredFeatures {
            for feature in r {
                if let Ok(ConversionResult::Success(s)) =
                    String::from_jsval(realm, feature.as_handle_value(), ())
                {
                    required_features.push(s)
                } else {
                    warn!("Unable to convert required feature to string");
                    if mode != XRSessionMode::Inline {
                        self.pending_immersive_session.set(false);
                    }
                    promise.reject_error(realm, Error::NotSupported(None));
                    return promise;
                }
            }
        }

        if let Some(ref o) = init.optionalFeatures {
            for feature in o {
                if let Ok(ConversionResult::Success(s)) =
                    String::from_jsval(realm, feature.as_handle_value(), ())
                {
                    optional_features.push(s)
                } else {
                    warn!("Unable to convert optional feature to string");
                }
            }
        }

        if !required_features.contains(&"viewer".to_string()) {
            required_features.push("viewer".to_string());
        }

        if !required_features.contains(&"local".to_string()) && mode != XRSessionMode::Inline {
            required_features.push("local".to_string());
        }

        if !window.Document().allowed_to_use_policy_feature(
            net_traits::permissions_policy::PolicyFeature::XrSpatialTracking,
        ) {
            let spatial = |feature: &str| {
                matches!(
                    feature,
                    "local"
                        | "local-floor"
                        | "bounded-floor"
                        | "unbounded"
                        | "hit-test"
                        | "anchors"
                        | "plane-detection"
                        | "mesh-detection"
                        | "hand-tracking"
                )
            };
            if required_features.iter().any(|feature| spatial(feature)) {
                if mode != XRSessionMode::Inline {
                    self.pending_immersive_session.set(false);
                }
                promise.reject_error(realm, Error::NotSupported(None));
                return promise;
            }
            optional_features.retain(|feature| !spatial(feature));
        }

        if generation != self.session_request_generation.get()
            || !window.Document().is_fully_active()
        {
            if generation == self.session_request_generation.get() && mode != XRSessionMode::Inline
            {
                self.pending_immersive_session.set(false);
            }
            promise.reject_error(realm, Error::InvalidState(None));
            return promise;
        }

        let init = SessionInit {
            webview_id: Some(global.as_window().webview_id()),
            origin: global
                .origin()
                .immutable()
                .ascii_serialization()
                .into_owned(),
            required_features,
            optional_features,
            first_person_observer_view: pref!(dom_webxr_first_person_observer_view),
        };

        let mut trusted = Some((Trusted::new(self), TrustedPromise::from(&promise)));
        let task_source = global
            .task_manager()
            .dom_manipulation_task_source()
            .to_sendable();
        let callback = ProfileGenericCallback::new(move |message| {
            let Some((this, promise)) = trusted.take() else {
                return;
            };
            let message: Result<Session, XRError> =
                message.unwrap_or(Err(XRError::CommunicationError));
            let response = message.map(PendingSession::new);
            task_source.queue(task!(request_session: move |cx| {
                this.root().session_obtained(cx, response, &promise.root(cx), mode, generation);
            }));
        });
        match (callback, window.webxr_registry()) {
            (Ok(callback), Some(mut registry)) => {
                registry.request_session_for_document(
                    self.pipeline,
                    generation,
                    mode.convert(),
                    init,
                    callback,
                );
            },
            _ => {
                if mode != XRSessionMode::Inline {
                    self.pending_immersive_session.set(false);
                }
                promise.reject_error(realm, Error::NotSupported(None));
            },
        }
        promise
    }

    /// <https://github.com/immersive-web/webxr-test-api/blob/master/explainer.md>
    fn Test(&self, cx: &mut JSContext) -> DomRoot<XRTest> {
        self.test.or_init(|| XRTest::new(cx, &self.global()))
    }
}

impl XRSystem {
    fn session_obtained(
        &self,
        cx: &mut JSContext,
        response: Result<PendingSession, XRError>,
        promise: &RootedPromise,
        mode: XRSessionMode,
        generation: u64,
    ) {
        if generation != self.session_request_generation.get()
            || !self.global().as_window().Document().is_fully_active()
        {
            if generation == self.session_request_generation.get() && mode != XRSessionMode::Inline
            {
                self.pending_immersive_session.set(false);
            }
            promise.reject_error(cx, Error::InvalidState(None));
            return;
        }
        let session = match response {
            Ok(session) => session.into_session(),
            Err(e) => {
                warn!("Error requesting XR session: {:?}", e);
                if mode != XRSessionMode::Inline {
                    self.pending_immersive_session.set(false);
                }
                promise.reject_error(cx, Error::NotSupported(None));
                return;
            },
        };
        let session = XRSession::new(cx, self.global().as_window(), session, mode);
        if mode == XRSessionMode::Inline {
            self.active_inline_sessions
                .borrow_mut()
                .push(Dom::from_ref(&*session));
        } else {
            self.set_active_immersive_session(cx, &session);
        }
        promise.resolve_native(cx, &session);
        // https://github.com/immersive-web/webxr/issues/961
        // This must be called _after_ the promise is resolved
        session.setup_initial_inputs();
    }

    // https://github.com/immersive-web/navigation/issues/10
    pub(crate) fn dispatch_sessionavailable(&self) {
        let xr = Trusted::new(self);
        self.global()
            .task_manager()
            .dom_manipulation_task_source()
            .queue(task!(fire_sessionavailable_event: move |cx| {
                // The sessionavailable event indicates user intent to enter an XR session
                let xr = xr.root();
                    let _guard = ScriptThread::user_interacting_guard();
                    xr.upcast::<EventTarget>().fire_bubbling_event(cx, atom!("sessionavailable"));
            }));
    }
}
