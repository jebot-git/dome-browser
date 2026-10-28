// SPDX-License-Identifier: MPL-2.0
//! Thread-local native owners. Timer threads only wake the event loop; they
//! never receive these callbacks, contexts or !Send texture wrappers.
use crate::cleanup_wakeup::WakeTimer;
use std::{
    cell::RefCell,
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
struct Owner {
    cleanup: Box<dyn FnMut() -> bool>,
    due: Instant,
    delay: Duration,
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
    let (due, timers) = PENDING.with(|pending| {
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
    });
    for timer in timers {
        timer.schedule(due);
    }
}
pub fn defer(cleanup: impl FnMut() -> bool + 'static) {
    PENDING.with(|pending| {
        pending.borrow_mut().owners.push(Owner {
            cleanup: Box::new(cleanup),
            due: Instant::now(),
            delay: Duration::from_millis(10),
        })
    });
    schedule_next();
}
pub fn pending() -> bool {
    PENDING.with(|pending| !pending.borrow().owners.is_empty())
}
/// Explicit cleanup callers may retry immediately. The idle event loop should
/// use poll_scheduled to observe per-owner backoff.
pub fn poll() -> bool {
    poll_inner(true)
}
pub fn poll_scheduled() -> bool {
    poll_inner(false)
}
fn poll_inner(force: bool) -> bool {
    let owners = PENDING.with(|pending| std::mem::take(&mut pending.borrow_mut().owners));
    let mut retained = Vec::new();
    for mut owner in owners {
        if !force && Instant::now() < owner.due {
            retained.push(owner);
            continue;
        }
        if !(owner.cleanup)() {
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
