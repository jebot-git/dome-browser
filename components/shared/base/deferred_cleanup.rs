// SPDX-License-Identifier: MPL-2.0
//! Thread-local native owners. Timer threads only wake the event loop; they
//! never receive these callbacks, contexts or !Send texture wrappers.
use crate::cleanup_wakeup::WakeTimer;
use std::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
/// Owner-thread completion tracking. Child retirement work created while an
/// owner is polled or dropped inherits its scope.
#[derive(Clone, Default)]
pub struct CleanupScope(Rc<ScopeState>);
#[derive(Default)]
struct ScopeState {
    pending: Cell<usize>,
    retained: Cell<bool>,
}
impl CleanupScope {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn pending(&self) -> bool {
        self.0.pending.get() != 0
    }
    /// Retry only this owner's due work, without polling another embedder's
    /// native callbacks. Returns true when this scope has completed.
    pub fn poll_scheduled(&self) -> bool {
        poll_inner(false, Some(self));
        !self.pending()
    }
    /// Last-resort shutdown policy: retain incomplete native owners inert until
    /// process exit. Call only after stopping new work and a bounded cleanup
    /// wait. This does not mark unfinished work complete. Future work in this
    /// scope is also retained, so it cannot run after the native host is gone.
    pub fn retain_unfinished(&self) {
        self.0.retained.set(true);
        let _ = PENDING.try_with(|pending| {
            pending.borrow_mut().owners.retain(|owner| {
                !owner
                    .scopes
                    .iter()
                    .any(|scope| Rc::ptr_eq(&scope.0, &self.0))
            });
        });
        schedule_next();
    }
    pub fn run<R>(&self, work: impl FnOnce() -> R) -> R {
        let mut scopes = current_scopes();
        if !scopes.iter().any(|scope| Rc::ptr_eq(&scope.0, &self.0)) {
            scopes.push(self.clone());
        }
        in_scopes(scopes, work)
    }
    pub fn defer(&self, cleanup: impl FnMut() -> bool + 'static) {
        let mut scopes = current_scopes();
        if !scopes.iter().any(|scope| Rc::ptr_eq(&scope.0, &self.0)) {
            scopes.push(self.clone());
        }
        enqueue(cleanup, scopes);
    }
}
thread_local! { static CURRENT_SCOPES: RefCell<Vec<CleanupScope>> = const { RefCell::new(Vec::new()) }; }
struct ScopeGuard(Vec<CleanupScope>);
impl Drop for ScopeGuard {
    fn drop(&mut self) {
        let _ = CURRENT_SCOPES.try_with(|scopes| {
            *scopes.borrow_mut() = std::mem::take(&mut self.0);
        });
    }
}
fn in_scopes<R>(scopes: Vec<CleanupScope>, work: impl FnOnce() -> R) -> R {
    let previous = CURRENT_SCOPES.try_with(|current| current.replace(scopes));
    let _guard = previous.ok().map(ScopeGuard);
    work()
}
fn current_scopes() -> Vec<CleanupScope> {
    CURRENT_SCOPES
        .try_with(|current| current.borrow().clone())
        .unwrap_or_default()
}
struct Owner {
    complete: bool,
    cleanup: Option<Box<dyn FnMut() -> bool>>,
    scopes: Vec<CleanupScope>,
    due: Instant,
    delay: Duration,
}
impl Drop for Owner {
    fn drop(&mut self) {
        if !self.complete {
            // The owner thread exited or polling unwound before retirement
            // completed. These !Send objects cannot be moved to another thread
            // or safely destroyed after their native host disappeared. Retain
            // them inert until process exit; normal shutdown must drain first.
            std::mem::forget(self.cleanup.take());
            return;
        }
        // Dropping the callback can itself retire another native owner. Keep
        // the parent outstanding until those child tasks have been registered.
        let cleanup = self.cleanup.take();
        in_scopes(self.scopes.clone(), || drop(cleanup));
        for scope in &self.scopes {
            scope.0.pending.set(scope.0.pending.get() - 1);
        }
    }
}
#[derive(Default)]
struct State {
    owners: Vec<Owner>,
    timers: Vec<Weak<WakeTimer>>,
}
thread_local! { static PENDING: RefCell<State> = RefCell::new(State::default()); }
/// Keep this registration on the native-owner thread. Dropping it cancels the
/// timer without waiting for callbacks or transferring native objects.
pub struct WakeRegistration {
    _timer: Arc<WakeTimer>,
    _local: PhantomData<Rc<()>>,
}
pub fn register_waker(wake: impl Fn() + Send + 'static) -> std::io::Result<WakeRegistration> {
    let timer = Arc::new(WakeTimer::new(wake)?);
    PENDING.with(|pending| pending.borrow_mut().timers.push(Arc::downgrade(&timer)));
    schedule_next();
    Ok(WakeRegistration {
        _timer: timer,
        _local: PhantomData,
    })
}
fn schedule_next() {
    let Ok((due, timers)) = PENDING.try_with(|pending| {
        let mut state = pending.borrow_mut();
        state.timers.retain(|timer| timer.strong_count() != 0);
        (
            state.owners.iter().map(|owner| owner.due).min(),
            state
                .timers
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>(),
        )
    }) else {
        return;
    };
    for timer in timers {
        timer.schedule(due);
    }
}
pub fn defer(cleanup: impl FnMut() -> bool + 'static) {
    enqueue(cleanup, current_scopes());
}
fn enqueue(cleanup: impl FnMut() -> bool + 'static, scopes: Vec<CleanupScope>) {
    for scope in &scopes {
        scope.0.pending.set(scope.0.pending.get() + 1);
    }
    let mut owner = Some(Owner {
        complete: false,
        cleanup: Some(Box::new(cleanup)),
        scopes,
        due: Instant::now(),
        delay: Duration::from_millis(10),
    });
    // Another TLS destructor can retire a native owner after this queue has
    // already been destroyed. Retain it through Owner's incomplete-drop path;
    // never destroy !Send native handles or panic during TLS teardown.
    if !owner
        .as_ref()
        .unwrap()
        .scopes
        .iter()
        .any(|scope| scope.0.retained.get())
    {
        let _ = PENDING.try_with(|pending| {
            pending.borrow_mut().owners.push(owner.take().unwrap());
        });
    }
    drop(owner);
    schedule_next();
}

pub fn pending() -> bool {
    PENDING.with(|pending| !pending.borrow().owners.is_empty())
}
/// Explicit cleanup callers may retry immediately. The idle event loop should
/// use poll_scheduled to observe per-owner backoff.
pub fn poll() -> bool {
    poll_inner(true, None)
}
pub fn poll_scheduled() -> bool {
    poll_inner(false, None)
}
fn poll_inner(force: bool, scope: Option<&CleanupScope>) -> bool {
    let owners = PENDING.with(|pending| std::mem::take(&mut pending.borrow_mut().owners));
    let mut retained = Vec::new();
    for mut owner in owners {
        let selected =
            scope.is_none_or(|scope| owner.scopes.iter().any(|tag| Rc::ptr_eq(&tag.0, &scope.0)));
        if !selected || (!force && Instant::now() < owner.due) {
            retained.push(owner);
            continue;
        }
        let complete = in_scopes(owner.scopes.clone(), || owner.cleanup.as_mut().unwrap()());
        if complete {
            owner.complete = true;
        }
        if !complete {
            owner.due = Instant::now() + owner.delay;
            owner.delay = (owner.delay * 2).min(Duration::from_millis(500));
            retained.push(owner);
        }
    }
    PENDING.with(|pending| pending.borrow_mut().owners.extend(retained));
    schedule_next();
    !pending()
}
/// For dedicated XR threads only, after the caller has received its result.
pub fn drain() {
    let mut delay = Duration::from_millis(10);
    while !poll() {
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(500));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    #[test]
    fn failed_callback_retains_thread_local_owner_until_success() {
        let resource = Rc::new(());
        let weak = Rc::downgrade(&resource);
        let polls = Rc::new(Cell::new(0));
        let count = polls.clone();
        defer(move || {
            let _ = &resource;
            count.set(count.get() + 1);
            count.get() == 3
        });
        assert!(!poll());
        assert!(weak.upgrade().is_some());
        assert!(!poll());
        assert!(weak.upgrade().is_some());
        assert!(poll());
        assert!(weak.upgrade().is_none());
        assert_eq!(polls.get(), 3);
    }
    #[test]
    fn callback_can_enqueue_another_owner_without_borrowing_conflict() {
        defer(|| {
            defer(|| true);
            true
        });
        assert!(!poll());
        assert!(poll());
    }
    #[test]
    fn busy_polls_respect_backoff_and_new_owner_runs_independently() {
        let attempts = Rc::new(Cell::new(0));
        let count = attempts.clone();
        let ready = Rc::new(Cell::new(false));
        let cleanup_ready = ready.clone();
        defer(move || {
            count.set(count.get() + 1);
            cleanup_ready.get()
        });
        assert!(!poll_scheduled());
        PENDING.with(|pending| {
            pending.borrow_mut().owners[0].due = Instant::now() + Duration::from_secs(10)
        });
        for _ in 0..20 {
            assert!(!poll_scheduled());
        }
        let new_owner = Rc::new(Cell::new(false));
        let completed = new_owner.clone();
        defer(move || {
            completed.set(true);
            true
        });
        assert!(!poll_scheduled());
        let paced = attempts.get() == 1 && new_owner.get();
        ready.set(true);
        assert!(poll());
        assert!(paced);
    }
    #[test]
    fn late_registration_wakes_already_queued_owner() {
        let (send, recv) = std::sync::mpsc::channel();
        let owner = Rc::new(Cell::new(false));
        let completed = owner.clone();
        defer(move || {
            completed.set(true);
            true
        });
        let registration = register_waker(move || {
            let _ = send.send(());
        })
        .unwrap();
        let woke = recv.recv_timeout(Duration::from_secs(2)).is_ok();
        let retained = !owner.get();
        assert!(poll_scheduled());
        drop(registration);
        assert!(woke && retained && owner.get());
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn scopes_track_only_their_own_pending_native_work() {
        let first = CleanupScope::new();
        let second = CleanupScope::new();
        let ready = Rc::new(Cell::new(false));
        let wait = ready.clone();
        first.defer(|| true);
        second.defer(move || wait.get());
        let both_pending = first.pending() && second.pending();
        assert!(!poll());
        let first_complete = !first.pending() && second.pending();
        ready.set(true);
        assert!(poll());
        assert!(both_pending && first_complete && !second.pending());
    }
    #[test]
    fn native_drop_handoff_keeps_scope_pending_until_child_finishes() {
        struct Resource;
        impl Drop for Resource {
            fn drop(&mut self) {
                defer(|| true);
            }
        }
        let scope = CleanupScope::new();
        let owner = Resource;
        scope.defer(move || {
            let _ = &owner;
            true
        });
        let first_pending = !poll() && scope.pending();
        let complete = poll() && !scope.pending();
        assert!(first_pending && complete);
    }
    #[test]
    fn scoped_callback_does_not_adopt_unrelated_unscoped_children() {
        let scope = CleanupScope::new();
        defer(|| {
            defer(|| true);
            true
        });
        scope.run(|| {
            assert!(!poll());
        });
        let independent = !scope.pending();
        assert!(poll());
        assert!(independent);
    }
    #[test]
    fn nested_scope_and_unwinding_restore_the_previous_owner() {
        let first = CleanupScope::new();
        let second = CleanupScope::new();
        first.run(|| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                second.run(|| panic!("scope probe"))
            }));
            defer(|| true);
        });
        let first_only = first.pending() && !second.pending();
        assert!(poll());
        assert!(first_only && !first.pending());
    }
}

#[cfg(test)]
mod inherited_scope_tests {
    use super::*;
    #[test]
    fn explicit_child_scope_keeps_parent_pending_and_deduplicates_tags() {
        let parent = CleanupScope::new();
        let child = CleanupScope::new();
        let next = child.clone();
        parent.defer(move || {
            next.run(|| next.defer(|| true));
            true
        });
        let linked = !poll() && parent.pending() && child.pending();
        assert!(poll());
        assert!(linked && !parent.pending() && !child.pending());
    }
}

#[cfg(test)]
mod thread_exit_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn thread_exit_does_not_drop_unretired_native_owners_after_tls_teardown() {
        struct Resource(Arc<AtomicUsize>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let probe = dropped.clone();
        let result = std::thread::spawn(move || {
            // Register first so the scope TLS is destroyed before the queue.
            let _registration = register_waker(|| {}).unwrap();
            let resource = Resource(probe);
            let scope = CleanupScope::new();
            scope.defer(move || {
                let _ = &resource;
                false
            });
        })
        .join();
        assert!(result.is_ok());
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }
}

#[cfg(test)]
mod late_enqueue_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Resource(Arc<AtomicUsize>);
    impl Drop for Resource {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    struct LateOwner(RefCell<Option<(CleanupScope, Resource)>>);
    impl Drop for LateOwner {
        fn drop(&mut self) {
            if let Some((scope, resource)) = self.0.get_mut().take() {
                scope.defer(move || {
                    let _ = &resource;
                    false
                });
            }
        }
    }
    thread_local! {
        static LATE: LateOwner = const { LateOwner(RefCell::new(None)) };
    }
    #[test]
    fn new_retirement_after_queue_tls_exit_keeps_native_owner_inert() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let probe = dropped.clone();
        let result = std::thread::spawn(move || {
            // TLS destructors run in reverse initialization order. This owner
            // retires last, after both the queue and ambient tags are gone.
            LATE.with(|_| {});
            let _registration = register_waker(|| {}).unwrap();
            let scope = CleanupScope::new();
            scope.run(|| {});
            LATE.with(|late| *late.0.borrow_mut() = Some((scope, Resource(probe))));
        })
        .join();
        assert!(result.is_ok());
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }
}

#[cfg(test)]
mod final_drain_tests {
    use super::*;
    #[test]
    fn scoped_shutdown_does_not_poll_unrelated_native_owners() {
        let first = CleanupScope::new();
        let second = CleanupScope::new();
        let attempts = Rc::new(Cell::new(0));
        let count = attempts.clone();
        second.defer(move || {
            count.set(count.get() + 1);
            true
        });
        first.defer(|| true);
        assert!(first.poll_scheduled());
        let independent = attempts.get() == 0 && second.pending();
        assert!(second.poll_scheduled());
        assert!(independent && attempts.get() == 1);
    }
    #[test]
    fn final_retention_disarms_current_and_future_work_but_keeps_peer_live() {
        let stopped = CleanupScope::new();
        let peer = CleanupScope::new();
        let attempts = Rc::new(Cell::new(0));
        let count = attempts.clone();
        stopped.defer(move || {
            count.set(count.get() + 1);
            false
        });
        stopped.retain_unfinished();
        let count = attempts.clone();
        stopped.defer(move || {
            count.set(count.get() + 1);
            false
        });
        peer.defer(|| true);
        assert!(peer.poll_scheduled());
        assert!(poll());
        assert!(stopped.pending());
        assert_eq!(attempts.get(), 0);
    }
}
