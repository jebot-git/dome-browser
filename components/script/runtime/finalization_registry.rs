/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! FinalizationRegistry host cleanup tasks. The GC hook only records rooted
//! work; a microtask checkpoint queues ordinary, lifecycle-cancellable tasks.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::{Arc, Weak};
use std::thread::{self, ThreadId};

use js::context::JSContext;
use js::jsapi::{HandleValueArray, Heap, JSFunction, JSObject, JSTracer, JS_GetFunctionObject};
use js::jsval::{ObjectValue, UndefinedValue};
use js::panic::wrap_panic;
use js::realm::AutoRealm;
use js::rust::wrappers2::{Call, SetHostCleanupFinalizationRegistryCallback};
use script_bindings::settings_stack::{run_a_callback, run_a_script};

use crate::dom::bindings::error::report_pending_exception;
use crate::dom::bindings::trace::RootedTraceableBox;
use crate::dom::globalscope::GlobalScope;
use crate::{DomTypeHolder, JSTraceable};

#[derive(JSTraceable)]
struct CleanupJob {
    callback: Heap<*mut JSObject>,
    incumbent: Heap<*mut JSObject>,
}

#[derive(Default)]
struct CleanupState {
    active: bool,
    pending: VecDeque<Box<CleanupJob>>,
    scheduled: HashMap<usize, (Box<CleanupJob>, Weak<()>)>,
}

thread_local! {
    static CLEANUP: RefCell<CleanupState> = RefCell::new(CleanupState::default());
}

// The queued task carries only an opaque identity, never a JS pointer or a
// runtime pointer. Dropping a cancelled task on another thread leaves pruning
// to its owner; no JS heap or root is touched from the foreign thread.
struct CleanupTicket {
    identity: Arc<()>,
    owner: ThreadId,
}

impl CleanupTicket {
    fn key(&self) -> usize {
        Arc::as_ptr(&self.identity) as usize
    }

    fn run(self, cx: &mut JSContext) {
        assert_eq!(self.owner, thread::current().id());
        let job = CLEANUP.with(|state| state.borrow_mut().scheduled.remove(&self.key()));
        let Some((job, _)) = job else {
            // The owning runtime was destroyed or this task was cancelled.
            return;
        };
        // No GC-capable operation occurs between removal and rooting.
        let job = RootedTraceableBox::from_box(job);
        run_cleanup(cx, &job);
    }
}

impl Drop for CleanupTicket {
    fn drop(&mut self) {
        if self.owner == thread::current().id() {
            let _ = CLEANUP.try_with(|state| {
                state.borrow_mut().scheduled.remove(&self.key());
            });
        }
    }
}

#[expect(unsafe_code)]
unsafe extern "C" fn queue_cleanup(
    callback: *mut JSFunction,
    incumbent: *mut JSObject,
    _data: *mut c_void,
) {
    wrap_panic(&mut || {
        CLEANUP.with(|state| {
            let mut state = state.borrow_mut();
            if !state.active {
                return;
            }
            // SpiderMonkey forbids GC here. Allocate only Rust bookkeeping;
            // keep Heap fields at stable addresses and trace them from the
            // runtime's extra-roots tracer, including through later GCs.
            let job = Box::new(CleanupJob {
                callback: Heap::default(),
                incumbent: Heap::default(),
            });
            job.callback.set(unsafe { JS_GetFunctionObject(callback) });
            job.incumbent.set(incumbent);
            state.pending.push_back(job);
        });
    });
}

#[expect(unsafe_code)]
pub(crate) fn initialize(cx: &JSContext) {
    CLEANUP.with(|state| {
        let mut state = state.borrow_mut();
        assert!(!state.active && state.pending.is_empty() && state.scheduled.is_empty());
        state.active = true;
    });
    unsafe { SetHostCleanupFinalizationRegistryCallback(cx, Some(queue_cleanup), std::ptr::null_mut()) };
}

#[expect(unsafe_code)]
pub(crate) fn shutdown(cx: &JSContext) {
    // Disable the hook and release all JS references before runtime destruction.
    // Surviving tickets cannot identify a future runtime's jobs: a ticket keeps
    // its Arc allocation alive until it is dropped, preventing identity reuse.
    unsafe { SetHostCleanupFinalizationRegistryCallback(cx, None, std::ptr::null_mut()) };
    CLEANUP.with(|state| *state.borrow_mut() = CleanupState::default());
}

/// Called by the owning runtime's extra-roots tracer.
#[expect(unsafe_code)]
pub(crate) unsafe fn trace(tracer: *mut JSTracer) {
    CLEANUP.with(|state| {
        let state = state.borrow();
        for job in &state.pending {
            unsafe { job.trace(tracer) };
        }
        for (job, _) in state.scheduled.values() {
            unsafe { job.trace(tracer) };
        }
    });
}

/// Called after the microtask checkpoint has finished. It never runs author JS.
#[expect(unsafe_code)]
pub(crate) fn enqueue_tasks() {
    CLEANUP.with(|state| {
        state.borrow_mut().scheduled.retain(|_, (_, ticket)| ticket.strong_count() != 0);
    });
    loop {
        let job = CLEANUP.with(|state| state.borrow_mut().pending.pop_front());
        let Some(job) = job else { break };
        let job = RootedTraceableBox::from_box(job);
        let global = unsafe { GlobalScope::from_object(job.callback.get()) };
        if !global.can_continue_running() {
            continue;
        }
        let ticket = CleanupTicket { identity: Arc::new(()), owner: thread::current().id() };
        CLEANUP.with(|state| {
            state.borrow_mut().scheduled.insert(ticket.key(), (job.into_box(), Arc::downgrade(&ticket.identity)));
        });
        global.task_manager().javascript_engine_task_source().queue(
            task!(finalization_registry_cleanup: move |cx| {
                ticket.run(cx);
            }),
        );
    }
}

#[expect(unsafe_code)]
fn run_cleanup(cx: &mut JSContext, job: &CleanupJob) {
    rooted!(&in(cx) let function = job.callback.get());
    let global = unsafe { GlobalScope::from_object(function.get()) };
    if !global.can_continue_running() {
        return;
    }
    let incumbent = (!job.incumbent.get().is_null())
        .then(|| unsafe { GlobalScope::from_object(job.incumbent.get()) });
    let mut realm = AutoRealm::new_from_handle(cx, function.handle());
    let cx = &mut realm.current_realm();
    run_a_script::<DomTypeHolder, _, _>(cx, &global, |cx| {
        rooted!(&in(cx) let function = ObjectValue(function.get()));
        rooted!(&in(cx) let this = UndefinedValue());
        rooted!(&in(cx) let mut result = UndefinedValue());
        let mut call = || unsafe {
            Call(cx, this.handle(), function.handle(), &HandleValueArray::empty(), result.handle_mut())
        };
        let ok = if let Some(incumbent) = incumbent.as_ref() {
            run_a_callback::<DomTypeHolder, _>(incumbent, call)
        } else {
            call()
        };
        if !ok {
            report_pending_exception(cx);
        }
    });
}
