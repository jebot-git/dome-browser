/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use js::context::JSContext;
use js::jsval::UndefinedValue;
use js::rust::HandleObject;
use net_traits::request::RequestBuilder;
use script_bindings::cformat;
use script_bindings::reflector::{Reflector, reflect_dom_object};
use servo_base::generic_channel::{GenericCallback, GenericSend};
use servo_config::pref;
use storage_traits::cache_storage::{
    CacheFailure, CacheOperation, CacheResult, CacheStorageThreadMessage,
};
use storage_traits::client_storage::{StorageIdentifier, StorageProxyMap, StorageType};

use crate::dom::bindings::codegen::Bindings::CacheStorageBinding::{
    CacheStorageMethods, MultiCacheQueryOptions,
};
use crate::dom::bindings::codegen::Bindings::RequestBinding::{RequestInfo, RequestInit};
use crate::dom::bindings::codegen::UnionTypes::RequestOrUSVString;
use crate::dom::bindings::error::{Error, Fallible};
use crate::dom::bindings::refcounted::{Trusted, TrustedPromise};
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::bindings::utils::to_frozen_array;
use crate::dom::fetch::request::Request;
use crate::dom::fetch::response::Response;
use crate::dom::globalscope::GlobalScope;
use crate::dom::serviceworker::cache::{Cache, query_options};
use crate::dom::{Promise, RootedPromise};
use crate::realms::enter_auto_realm;

#[dom_struct]
pub(crate) struct CacheStorage {
    reflector_: Reflector,
}

#[derive(Clone, Copy)]
pub(crate) enum CacheReply {
    Open,
    Boolean,
    Names,
    Match,
    MatchAll,
    Keys,
    Done,
}

impl CacheStorage {
    pub(crate) fn new(cx: &mut JSContext, global: &GlobalScope) -> DomRoot<Self> {
        reflect_dom_object(
            cx,
            Box::new(Self {
                reflector_: Reflector::new(),
            }),
            global,
        )
    }
}

pub(crate) fn cache_enabled() -> bool {
    pref!(dom_cache_enabled) || pref!(dom_serviceworker_enabled)
}

pub(crate) fn request_object(
    cx: &mut JSContext,
    global: &GlobalScope,
    input: RequestOrUSVString,
) -> Fallible<DomRoot<Request>> {
    Ok(match input {
        RequestOrUSVString::Request(request) => request,
        RequestOrUSVString::USVString(value) => Request::constructor(
            cx,
            global,
            None,
            RequestInfo::USVString(value),
            &RequestInit::empty(),
            None,
        )?,
    })
}

pub(crate) fn request_record(
    cx: &mut JSContext,
    global: &GlobalScope,
    input: RequestOrUSVString,
) -> Fallible<RequestBuilder> {
    Ok(request_object(cx, global, input)?.cache_record(cx))
}

fn storage_proxy(global: &GlobalScope) -> Fallible<StorageProxyMap> {
    let origin = global.origin().immutable().clone();
    if matches!(origin, servo_url::ImmutableOrigin::Opaque(_)) {
        return Err(Error::Security(None));
    }
    let receiver = global
        .storage_threads()
        .client_storage_handle()
        .obtain_a_storage_bottle_map(
            StorageType::Local,
            global.webview_id(),
            StorageIdentifier::Caches,
            origin,
        );
    receiver
        .recv()
        .map_err(|_| Error::Operation(Some("Storage service unavailable".into())))?
        .map_err(|message| Error::Operation(Some(message)))
}

fn cache_error(error: CacheFailure) -> Error {
    match error {
        CacheFailure::Storage(message) => Error::Operation(Some(message)),
        CacheFailure::Type(message) => Error::Type(cformat!("{message}")),
        CacheFailure::InvalidState => Error::InvalidState(None),
        CacheFailure::QuotaExceeded => Error::QuotaExceeded {
            quota: None,
            requested: None,
        },
    }
}

fn finish_operation(
    cx: &mut JSContext,
    global: &GlobalScope,
    promise: &RootedPromise,
    reply: CacheReply,
    result: Result<CacheResult, CacheFailure>,
) -> Fallible<()> {
    let result = result.map_err(cache_error)?;
    match (reply, result) {
        (CacheReply::Open, CacheResult::Opened(cache)) => {
            let cache = Cache::new(cx, global, cache);
            promise.resolve_native(cx, &*cache);
        },
        (CacheReply::Boolean, CacheResult::Boolean(value)) => promise.resolve_native(cx, &value),
        (CacheReply::Names, CacheResult::Names(names)) => {
            promise.resolve_native(
                cx,
                &names
                    .into_iter()
                    .map(DOMString::from_code_units)
                    .collect::<Vec<_>>(),
            );
        },
        (CacheReply::Done, CacheResult::Done) => promise.resolve_native(cx, &()),
        (CacheReply::Match, CacheResult::Entries(entries)) => {
            if let Some(entry) = entries.into_iter().next() {
                let response = Response::from_cache_record(cx, global, entry.response)?;
                promise.resolve_native(cx, &*response);
            } else {
                promise.resolve_native(cx, &());
            }
        },
        (CacheReply::MatchAll, CacheResult::Entries(entries)) => {
            let responses = entries
                .into_iter()
                .map(|entry| Response::from_cache_record(cx, global, entry.response))
                .collect::<Fallible<Vec<_>>>()?;
            let mut realm = enter_auto_realm(cx, &**promise);
            let cx = &mut realm.current_realm();
            rooted!(&in(cx) let mut array = UndefinedValue());
            to_frozen_array(cx, &responses, array.handle_mut());
            promise.resolve(cx, array.handle());
        },
        (CacheReply::Keys, CacheResult::Entries(entries)) => {
            let requests = entries
                .into_iter()
                .map(|entry| Request::from_cache_record(cx, global, entry.request))
                .collect::<Fallible<Vec<_>>>()?;
            let mut realm = enter_auto_realm(cx, &**promise);
            let cx = &mut realm.current_realm();
            rooted!(&in(cx) let mut array = UndefinedValue());
            to_frozen_array(cx, &requests, array.handle_mut());
            promise.resolve(cx, array.handle());
        },
        _ => {
            return Err(Error::Operation(Some(
                "Unexpected cache storage reply".into(),
            )));
        },
    }
    Ok(())
}

/// Each operation owns its callback and promise, so body reads finishing in a
/// different order cannot settle another operation's promise.
pub(crate) fn send_operation(
    cx: &mut JSContext,
    global: &GlobalScope,
    promise: &RootedPromise,
    operation: CacheOperation,
    reply: CacheReply,
) {
    let proxy = match storage_proxy(global) {
        Ok(proxy) => proxy,
        Err(error) => {
            promise.reject_error(cx, error);
            return;
        },
    };
    let mut roots = Some((Trusted::new(global), TrustedPromise::new(promise)));
    let task_source = global
        .task_manager()
        .dom_manipulation_task_source()
        .to_sendable();
    let callback = match GenericCallback::new(move |result| {
        let Some((trusted_global, trusted_promise)) = roots.take() else {
            return;
        };
        let result = result
            .unwrap_or_else(|_| Err(CacheFailure::Storage("Cache service disconnected".into())));
        task_source.queue(task!(cache_storage_reply: move |cx| {
            let global = trusted_global.root();
            let promise = trusted_promise.root(cx);
            if let Err(error) = finish_operation(cx, &global, &promise, reply, result) { promise.reject_error(cx, error); }
        }));
    }) {
        Ok(callback) => callback,
        Err(_) => {
            promise.reject_error(
                cx,
                Error::Operation(Some("Cache callback unavailable".into())),
            );
            return;
        },
    };
    if global
        .storage_threads()
        .send(CacheStorageThreadMessage::Run {
            origin: global.origin().immutable().clone(),
            proxy,
            operation,
            callback,
        })
        .is_err()
    {
        promise.reject_error(
            cx,
            Error::Operation(Some("Cache service unavailable".into())),
        );
    }
}

impl CacheStorageMethods<crate::DomTypeHolder> for CacheStorage {
    fn Open(&self, cx: &mut JSContext, name: DOMString) -> RootedPromise {
        let global = self.global();
        let promise = Promise::new(cx, &global);
        send_operation(
            cx,
            &global,
            &promise,
            CacheOperation::Open(name.to_code_units()),
            CacheReply::Open,
        );
        promise
    }
    fn Has(&self, cx: &mut JSContext, name: DOMString) -> RootedPromise {
        let global = self.global();
        let promise = Promise::new(cx, &global);
        send_operation(
            cx,
            &global,
            &promise,
            CacheOperation::Has(name.to_code_units()),
            CacheReply::Boolean,
        );
        promise
    }
    fn Delete(&self, cx: &mut JSContext, name: DOMString) -> RootedPromise {
        let global = self.global();
        let promise = Promise::new(cx, &global);
        send_operation(
            cx,
            &global,
            &promise,
            CacheOperation::DeleteCache(name.to_code_units()),
            CacheReply::Boolean,
        );
        promise
    }
    fn Keys(&self, cx: &mut JSContext) -> RootedPromise {
        let global = self.global();
        let promise = Promise::new(cx, &global);
        send_operation(
            cx,
            &global,
            &promise,
            CacheOperation::StorageKeys,
            CacheReply::Names,
        );
        promise
    }
    fn Match(
        &self,
        cx: &mut JSContext,
        input: RequestOrUSVString,
        options: &MultiCacheQueryOptions,
    ) -> RootedPromise {
        let global = self.global();
        let promise = Promise::new(cx, &global);
        let request = match request_record(cx, &global, input) {
            Ok(request) => request,
            Err(error) => {
                promise.reject_error(cx, error);
                return promise;
            },
        };
        send_operation(
            cx,
            &global,
            &promise,
            CacheOperation::StorageMatch {
                request,
                options: query_options(&options.parent),
                cache_name: options.cacheName.as_ref().map(|name| name.to_code_units()),
            },
            CacheReply::Match,
        );
        promise
    }
}

impl script_bindings::interfaces::CacheApiHelpers for CacheStorage {
    fn is_enabled(_cx: &mut JSContext, _global: HandleObject) -> bool {
        cache_enabled()
    }
}
