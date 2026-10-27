/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use js::context::JSContext;
use script_bindings::callback::{RootedCallback, TracedCallback};
use script_bindings::cell::DomRefCell;

use crate::dom::bindings::callback::ExceptionHandling;
use crate::dom::bindings::codegen::Bindings::IdleDeadlineBinding::{
    IdleRequestCallback, IdleRequestOptions,
};
use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowMethods;
use crate::dom::idledeadline::IdleDeadline;
use crate::dom::window::Window;
use crate::event_loop::timers::{OneshotTimerCallback, OneshotTimerHandle};
use crate::realms::enter_auto_realm;

#[derive(JSTraceable, MallocSizeOf)]
struct IdleRequest {
    handle: u32,
    callback: TracedCallback<IdleRequestCallback>,
    timeout: Option<OneshotTimerHandle>,
}

#[derive(Default, JSTraceable, MallocSizeOf)]
pub(crate) struct IdleCallbacks {
    next_handle: Cell<u32>,
    pending: DomRefCell<VecDeque<IdleRequest>>,
    runnable: DomRefCell<VecDeque<IdleRequest>>,
    period_deadline: Cell<Option<Instant>>,
    period_generation: Cell<u64>,
    period_wake: Cell<Option<OneshotTimerHandle>>,
}

impl Window {
    pub(crate) fn request_idle_callback(
        &self,
        callback: RootedCallback<IdleRequestCallback>,
        options: &IdleRequestOptions,
    ) -> u32 {
        let handle = loop {
            let handle = self.idle_callbacks().next_handle.get().wrapping_add(1);
            self.idle_callbacks().next_handle.set(handle);
            if handle != 0 &&
                !self.idle_callbacks().pending.borrow().iter().any(|entry| entry.handle == handle) &&
                !self.idle_callbacks().runnable.borrow().iter().any(|entry| entry.handle == handle)
            {
                break handle;
            }
        };
        let timeout = options.timeout.filter(|timeout| *timeout > 0).map(|timeout| {
            self.as_global_scope().schedule_callback(
                OneshotTimerCallback::IdleCallbackTimeout(handle),
                Duration::from_millis(u64::from(timeout)),
            )
        });
        self.idle_callbacks().pending.borrow_mut().push_back(IdleRequest {
            handle,
            callback: callback.to_traced(),
            timeout,
        });
        handle
    }

    fn take_idle_callback(&self, handle: u32) -> Option<IdleRequest> {
        for queue in [&self.idle_callbacks().pending, &self.idle_callbacks().runnable] {
            let mut queue = queue.borrow_mut();
            if let Some(index) = queue.iter().position(|entry| entry.handle == handle) {
                return queue.remove(index);
            }
        }
        None
    }

    pub(crate) fn cancel_idle_callback(&self, handle: u32) {
        if let Some(timer) = self.take_idle_callback(handle).and_then(|entry| entry.timeout) {
            self.as_global_scope().unschedule_callback(timer);
        }
    }

    pub(crate) fn invoke_idle_callback(&self, cx: &mut JSContext, estimate: Instant) -> bool {
        let now = Instant::now();
        if !self.Document().is_fully_active() || estimate <= now { return false; }
        let idle = self.idle_callbacks();
        if idle.period_deadline.get().is_none_or(|deadline| now >= deadline) {
            if idle.pending.borrow().is_empty() && idle.runnable.borrow().is_empty() {
                return false;
            }
            // Snapshot registrations at the start of the period. Requests made
            // by a callback go to pending and cannot join this runnable batch.
            idle.runnable.borrow_mut().append(&mut idle.pending.borrow_mut());
            idle.period_deadline.set(Some(estimate));
            let generation = idle.period_generation.get().wrapping_add(1);
            idle.period_generation.set(generation);
            if let Some(wake) = idle.period_wake.take() {
                self.as_global_scope().unschedule_callback(wake);
            }
            let wake = self.as_global_scope().schedule_callback(
                OneshotTimerCallback::IdlePeriodWake(generation),
                estimate.saturating_duration_since(now),
            );
            idle.period_wake.set(Some(wake));
        }
        // A period can end early for higher-priority work, but the next period
        // must still wait until its deadline. The wake timer prevents waiting
        // registrations from being stranded when there is no other activity.
        let deadline = idle.period_deadline.get().unwrap().min(estimate);
        if deadline <= Instant::now() { return false; }
        let entry = idle.runnable.borrow_mut().pop_front();
        let Some(entry) = entry else { return false; };
        self.call_idle_callback(cx, entry, deadline, false);
        true
    }

    pub(crate) fn idle_period_wake(&self, generation: u64) {
        if self.idle_callbacks().period_generation.get() == generation {
            self.idle_callbacks().period_wake.set(None);
        }
    }

    pub(crate) fn invoke_idle_timeout(&self, cx: &mut JSContext, handle: u32) {
        if !self.Document().is_fully_active() { return; }
        if let Some(entry) = self.take_idle_callback(handle) {
            self.call_idle_callback(cx, entry, Instant::now(), true);
        }
    }

    fn call_idle_callback(
        &self,
        cx: &mut JSContext,
        entry: IdleRequest,
        deadline: Instant,
        timed_out: bool,
    ) {
        // Root before allocating the deadline or invoking author code. Neither
        // the callback nor the window's queue is borrowed during invocation.
        let callback = entry.callback.root(cx);
        if let Some(timer) = entry.timeout {
            self.as_global_scope().unschedule_callback(timer);
        }
        let mut realm = enter_auto_realm(cx, self);
        let cx = &mut realm.current_realm();
        let deadline = IdleDeadline::new(cx, self, deadline, timed_out);
        let _ = callback.Call__(cx, &deadline, ExceptionHandling::Report);
    }
}
