/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dom_struct::dom_struct;
use js::context::JSContext;
use js::jsval::UndefinedValue;
use js::realm::CurrentRealm;
use js::rust::{HandleObject, HandleValue};
use net_traits::request::RequestBuilder;
use script_bindings::reflector::{Reflector, reflect_dom_object};
use storage_traits::cache_storage::{CacheEntry, CacheIdentifier, CacheOperation, CacheQuery};

use crate::dom::abortcontroller::AbortController;
use crate::dom::abortsignal::AbortSignal;
use crate::dom::bindings::codegen::Bindings::CacheBinding::{CacheMethods, CacheQueryOptions};
use crate::dom::bindings::codegen::Bindings::RequestBinding::{
    RequestInfo, RequestInit, RequestMethods,
};
use crate::dom::bindings::codegen::Bindings::ResponseBinding::{ResponseMethods, ResponseType};
use crate::dom::bindings::codegen::UnionTypes::RequestOrUSVString;
use crate::dom::bindings::conversions::root_from_handleobject;
use crate::dom::bindings::error::{Error, Fallible};
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::DomRoot;
use crate::dom::fetch::request::Request;
use crate::dom::fetch::response::Response;
use crate::dom::globalscope::GlobalScope;
use crate::dom::promisenativehandler::{Callback, PromiseNativeHandler};
use crate::dom::serviceworker::cachestorage::{
    CacheReply, cache_enabled, request_object, request_record, send_operation,
};
use crate::dom::{Promise, RootedPromise};
use crate::fetch::fetch::Fetch;
use crate::realms::enter_auto_realm;

#[dom_struct]
pub(crate) struct Cache {
    reflector_: Reflector,
    #[no_trace]
    identity: CacheIdentifier,
}

pub(crate) fn query_options(options: &CacheQueryOptions) -> CacheQuery {
    CacheQuery {
        ignore_search: options.ignoreSearch,
        ignore_method: options.ignoreMethod,
        ignore_vary: options.ignoreVary,
    }
}

fn validate_request(request: &RequestBuilder) -> Fallible<()> {
    if !matches!(request.url.url().scheme(), "http" | "https") || request.method.as_str() != "GET" {
        return Err(Error::Type(
            c"Cache writes require an HTTP(S) GET request".to_owned(),
        ));
    }
    Ok(())
}

type Ready = Rc<dyn Fn(&mut JSContext, CacheEntry)>;
/// Claim a failure before canceling sibling fetches. Only the claimant may
/// reject: cancellation can synchronously reenter another body reader.
type Failed = Rc<dyn Fn(&mut JSContext) -> bool>;

/// Read through Fetch's native stream reader, preserving stream rejection values.
/// No successful body read or transaction means no cache write.
fn read_entry(
    cx: &mut JSContext,
    response: &Response,
    request: RequestBuilder,
    promise: &RootedPromise,
    ready: Ready,
    failed: Failed,
) {
    let record = response.cache_record(cx);
    if record.metadata.status == 206
        || record.metadata.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("vary")
                && value
                    .split(|byte| *byte == b',')
                    .any(|field| field.trim_ascii() == b"*")
        })
    {
        if failed(cx) {
            promise.reject_error(
                cx,
                Error::Type(c"Partial responses and Vary:* cannot be cached".to_owned()),
            );
        }
        return;
    }
    let body = match response.body_for_cache(cx) {
        Ok(body) => body,
        Err(error) => {
            if failed(cx) {
                promise.reject_error(cx, error);
            }
            return;
        },
    };
    let entry = CacheEntry {
        request,
        response: record,
    };
    let Some(body) = body else {
        ready(cx, entry);
        return;
    };
    let reader = match body.acquire_default_reader(cx) {
        Ok(reader) => reader,
        Err(error) => {
            if failed(cx) {
                promise.reject_error(cx, error);
            }
            return;
        },
    };
    let saved = RefCell::new(Some(entry));
    let promise = promise.clone();
    reader.read_all_bytes(
        cx,
        Rc::new(move |cx, bytes| {
            if let Some(mut entry) = saved.borrow_mut().take() {
                entry.response.body = Some(bytes);
                ready(cx, entry);
            }
        }),
        Rc::new(move |cx, error| {
            if failed(cx) {
                promise.reject(cx, error);
            }
        }),
    );
}

impl Cache {
    pub(crate) fn new(
        cx: &mut JSContext,
        global: &GlobalScope,
        identity: CacheIdentifier,
    ) -> DomRoot<Self> {
        reflect_dom_object(
            cx,
            Box::new(Self {
                reflector_: Reflector::new(),
                identity,
            }),
            global,
        )
    }

    fn query(
        &self,
        cx: &mut JSContext,
        input: Option<RequestOrUSVString>,
        options: &CacheQueryOptions,
        reply: CacheReply,
    ) -> RootedPromise {
        let global = self.global();
        let promise = Promise::new(cx, &global);
        let request = match input
            .map(|input| request_record(cx, &global, input))
            .transpose()
        {
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
            CacheOperation::Query {
                cache: self.identity,
                request,
                options: query_options(options),
            },
            reply,
        );
        promise
    }

    fn add_all(&self, cx: &mut JSContext, inputs: Vec<RequestOrUSVString>) -> RootedPromise {
        let global = self.global();
        let promise = Promise::new(cx, &global);
        let mut requests = Vec::with_capacity(inputs.len());
        let mut records = Vec::with_capacity(inputs.len());
        for input in inputs {
            let request = match request_object(cx, &global, input) {
                Ok(request) => request,
                Err(error) => {
                    promise.reject_error(cx, error);
                    return promise;
                },
            };
            let record = request.cache_record(cx);
            if let Err(error) = validate_request(&record) {
                promise.reject_error(cx, error);
                return promise;
            }
            requests.push(request);
            records.push(record);
        }
        if requests.is_empty() {
            promise.resolve_native(cx, &());
            return promise;
        }
        let controller = AbortController::new_with_proto(cx, &global, None);
        let state = Rc::new(Batch {
            // These strong GC roots outlive native promise handlers and stream
            // callbacks. No untraced Dom pointers are kept in this Rust state.
            global: global.clone(),
            promise: promise.clone(),
            controller,
            identity: self.identity,
            remaining: Cell::new(requests.len()),
            failed: Cell::new(false),
            entries: RefCell::new(vec![None; requests.len()]),
            requests: records,
        });
        for (index, request) in requests.into_iter().enumerate() {
            let signal = AbortSignal::create_dependent_abort_signal(
                cx,
                vec![request.Signal(), state.controller.signal()],
                &global,
            );
            let mut init = RequestInit::empty();
            init.signal = Some(Some(signal));
            let mut realm = enter_auto_realm(cx, &*global);
            let cx = &mut realm.current_realm();
            let fetched = Fetch(&global, RequestInfo::Request(request), &init, cx);
            let handler = PromiseNativeHandler::new(
                cx,
                &global,
                Some(Box::new(Fetched {
                    state: state.clone(),
                    index,
                })),
                Some(Box::new(FetchFailed {
                    state: state.clone(),
                })),
            );
            fetched.append_native_handler(cx, &handler);
        }
        promise
    }
}

/// The members containing DOM objects are explicit long-lived roots. This state
/// can therefore also be retained by stream callbacks, which are Rust closures.
struct Batch {
    global: DomRoot<GlobalScope>,
    promise: RootedPromise,
    controller: DomRoot<AbortController>,
    identity: CacheIdentifier,
    remaining: Cell<usize>,
    failed: Cell<bool>,
    entries: RefCell<Vec<Option<CacheEntry>>>,
    requests: Vec<RequestBuilder>,
}
impl Batch {
    fn abort(&self, cx: &mut JSContext) -> bool {
        if self.failed.replace(true) {
            return false;
        }
        let mut realm = enter_auto_realm(cx, &*self.global);
        let cx = &mut realm.current_realm();
        rooted!(&in(cx) let reason = UndefinedValue());
        self.controller.signal_abort(cx, reason.handle());
        true
    }
    fn complete(&self, cx: &mut JSContext, index: usize, entry: CacheEntry) {
        if self.failed.get() {
            return;
        }
        self.entries.borrow_mut()[index] = Some(entry);
        self.remaining.set(self.remaining.get() - 1);
        if self.remaining.get() == 0 {
            let entries = self
                .entries
                .borrow_mut()
                .iter_mut()
                .map(|entry| entry.take().unwrap())
                .collect();
            send_operation(
                cx,
                &self.global,
                &self.promise,
                CacheOperation::Put {
                    cache: self.identity,
                    entries,
                },
                CacheReply::Done,
            );
        }
    }
}

#[derive(JSTraceable, MallocSizeOf)]
struct Fetched {
    #[no_trace]
    #[ignore_malloc_size_of = "Explicit GC roots in shared native batch state"]
    state: Rc<Batch>,
    index: usize,
}
impl Callback for Fetched {
    fn callback(&self, cx: &mut CurrentRealm, value: HandleValue) {
        if self.state.failed.get() {
            return;
        }
        let response = if value.is_object() {
            rooted!(&in(cx) let object = value.to_object());
            root_from_handleobject::<Response>(cx, object.handle()).ok()
        } else {
            None
        };
        let Some(response) = response else {
            self.state.abort(cx);
            self.state.promise.reject_error(
                cx,
                Error::Type(c"Fetch did not return a Response".to_owned()),
            );
            return;
        };
        if response.Type() == ResponseType::Error || !response.Ok() || response.Status() == 206 {
            self.state.abort(cx);
            self.state.promise.reject_error(
                cx,
                Error::Type(c"Cache.addAll requires successful non-partial responses".to_owned()),
            );
            return;
        }
        let ready_state = self.state.clone();
        let failure_state = self.state.clone();
        let index = self.index;
        read_entry(
            cx,
            &response,
            self.state.requests[index].clone(),
            &self.state.promise,
            Rc::new(move |cx, entry| ready_state.complete(cx, index, entry)),
            Rc::new(move |cx| failure_state.abort(cx)),
        );
    }
}

#[derive(JSTraceable, MallocSizeOf)]
struct FetchFailed {
    #[no_trace]
    #[ignore_malloc_size_of = "Explicit GC roots in shared native batch state"]
    state: Rc<Batch>,
}
impl Callback for FetchFailed {
    fn callback(&self, cx: &mut CurrentRealm, value: HandleValue) {
        if !self.state.failed.get() {
            self.state.abort(cx);
            self.state.promise.reject(cx, value);
        }
    }
}

impl CacheMethods<crate::DomTypeHolder> for Cache {
    fn Match(
        &self,
        cx: &mut JSContext,
        input: RequestOrUSVString,
        options: &CacheQueryOptions,
    ) -> RootedPromise {
        self.query(cx, Some(input), options, CacheReply::Match)
    }
    fn MatchAll(
        &self,
        cx: &mut JSContext,
        input: Option<RequestOrUSVString>,
        options: &CacheQueryOptions,
    ) -> RootedPromise {
        self.query(cx, input, options, CacheReply::MatchAll)
    }
    fn Keys(
        &self,
        cx: &mut JSContext,
        input: Option<RequestOrUSVString>,
        options: &CacheQueryOptions,
    ) -> RootedPromise {
        self.query(cx, input, options, CacheReply::Keys)
    }
    fn Delete(
        &self,
        cx: &mut JSContext,
        input: RequestOrUSVString,
        options: &CacheQueryOptions,
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
            CacheOperation::DeleteEntries {
                cache: self.identity,
                request,
                options: query_options(options),
            },
            CacheReply::Boolean,
        );
        promise
    }
    fn Put(
        &self,
        cx: &mut JSContext,
        input: RequestOrUSVString,
        response: &Response,
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
        if let Err(error) = validate_request(&request) {
            promise.reject_error(cx, error);
            return promise;
        }
        let ready_global = global.clone();
        let ready_promise = promise.clone();
        let identity = self.identity;
        read_entry(
            cx,
            response,
            request,
            &promise,
            Rc::new(move |cx, entry| {
                send_operation(
                    cx,
                    &ready_global,
                    &ready_promise,
                    CacheOperation::Put {
                        cache: identity,
                        entries: vec![entry],
                    },
                    CacheReply::Done,
                );
            }),
            Rc::new(|_| true),
        );
        promise
    }
    fn Add(&self, cx: &mut JSContext, input: RequestOrUSVString) -> RootedPromise {
        self.add_all(cx, vec![input])
    }
    fn AddAll(&self, cx: &mut JSContext, inputs: Vec<RequestOrUSVString>) -> RootedPromise {
        self.add_all(cx, inputs)
    }
}

impl script_bindings::interfaces::CacheApiHelpers for Cache {
    fn is_enabled(_cx: &mut JSContext, _global: HandleObject) -> bool {
        cache_enabled()
    }
}
