// SPDX-License-Identifier: GPL-3.0-or-later
//! One lazy worker per native discovery. Availability never blocks the UI and
//! no polling occurs while there are no authorized document observers.
use servo::profile_traits::generic_callback::GenericCallback;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub(super) type Key = (u64, u64, i64, i64, u64, String);
const INTERVAL: Duration = Duration::from_millis(500);
struct State {
    callback: Option<GenericCallback<()>>,
    revision: u64,
    stopped: bool,
}
pub(super) struct Monitor {
    state: Arc<(Mutex<State>, Condvar)>,
    probe: Arc<dyn Fn() -> Option<Key> + Send + Sync>,
    worker: Option<JoinHandle<()>>,
}
impl Monitor {
    pub(super) fn new(probe: impl Fn() -> Option<Key> + Send + Sync + 'static) -> Self {
        Self {
            state: Arc::new((
                Mutex::new(State {
                    callback: None,
                    revision: 0,
                    stopped: false,
                }),
                Condvar::new(),
            )),
            probe: Arc::new(probe),
            worker: None,
        }
    }
    pub(super) fn observe(&mut self, callback: Option<GenericCallback<()>>) {
        let enabled = callback.is_some();
        {
            let mut state = self.state.0.lock().unwrap();
            state.callback = callback;
            state.revision = state.revision.wrapping_add(1);
        }
        self.state.1.notify_all();
        if enabled && self.worker.is_none() {
            let state = self.state.clone();
            let probe = self.probe.clone();
            match std::thread::Builder::new()
                .name("Dome XR availability".into())
                .spawn(move || run(state, probe))
            {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => eprintln!("WebXR availability worker could not start: {error}"),
            }
        }
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        let mut state = self.state.0.lock().unwrap();
        state.stopped = true;
        state.callback = None;
        drop(state);
        self.state.1.notify_all();
        // The bounded probe may still be finishing. Do not join it on the UI
        // thread; it exits without another probe or callback after cancellation.
    }
}
fn run(shared: Arc<(Mutex<State>, Condvar)>, probe: Arc<dyn Fn() -> Option<Key> + Send + Sync>) {
    let (lock, changed) = &*shared;
    let mut last_revision = 0;
    let mut last: Option<Option<Key>> = None;
    loop {
        let mut state = lock.lock().unwrap();
        while !state.stopped && state.callback.is_none() {
            state = changed.wait(state).unwrap();
        }
        if state.stopped {
            return;
        }
        let revision = state.revision;
        let callback = state.callback.clone().unwrap();
        drop(state);
        let current = probe();
        let state = lock.lock().unwrap();
        if state.stopped {
            return;
        }
        if state.revision != revision {
            continue;
        }
        if last_revision != revision {
            last_revision = revision;
            last = None;
        }
        let notify = last.as_ref().is_some_and(|last| *last != current);
        last = Some(current);
        drop(state);
        if notify {
            let _ = callback.send(());
        }
        let state = lock.lock().unwrap();
        if state.stopped {
            return;
        }
        if state.revision == revision {
            drop(changed.wait_timeout(state, INTERVAL).unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Instant;
    fn wait(mut condition: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("Availability worker did not reach the expected state");
    }
    #[test]
    fn worker_is_lazy_and_stops_polling_without_observers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let mut monitor = Monitor::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            None
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let callback = GenericCallback::new(|_| {}).unwrap();
        monitor.observe(Some(callback));
        wait(|| calls.load(Ordering::SeqCst) > 0);
        monitor.observe(None);
        std::thread::sleep(Duration::from_millis(40));
        let stopped = calls.load(Ordering::SeqCst);
        std::thread::sleep(INTERVAL + Duration::from_millis(50));
        assert_eq!(calls.load(Ordering::SeqCst), stopped);
    }
    #[test]
    fn unchanged_samples_do_not_emit_and_changes_are_reported() {
        let revision = Arc::new(AtomicUsize::new(0));
        let value = revision.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let mut monitor = Monitor::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            let v = value.load(Ordering::SeqCst);
            if v == 0 {
                None
            } else {
                Some((0, 0, 0, 0, v as u64, "test".into()))
            }
        });
        let (tx, rx) = mpsc::channel();
        monitor.observe(Some(
            GenericCallback::new(move |_| {
                let _ = tx.send(());
            })
            .unwrap(),
        ));
        wait(|| calls.load(Ordering::SeqCst) >= 2);
        assert!(rx.try_recv().is_err());
        revision.store(1, Ordering::SeqCst);
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        revision.store(2, Ordering::SeqCst);
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        revision.store(0, Ordering::SeqCst);
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        std::thread::sleep(INTERVAL + Duration::from_millis(50));
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn cancelled_inflight_sample_cannot_notify_a_replacement_observer() {
        let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
        let worker_gate = gate.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let mut monitor = Monitor::new(move || {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == 1 {
                let mut state = worker_gate.0.lock().unwrap();
                state.0 = true;
                worker_gate.1.notify_all();
                while !state.1 {
                    state = worker_gate.1.wait(state).unwrap();
                }
            }
            if n == 0 {
                None
            } else {
                Some((0, 0, 0, 0, 1, "test".into()))
            }
        });
        let (tx, rx) = mpsc::channel();
        monitor.observe(Some(
            GenericCallback::new(move |_| {
                let _ = tx.send(());
            })
            .unwrap(),
        ));
        wait(|| gate.0.lock().unwrap().0);
        monitor.observe(None);
        let (new_tx, new_rx) = mpsc::channel();
        monitor.observe(Some(
            GenericCallback::new(move |_| {
                let _ = new_tx.send(());
            })
            .unwrap(),
        ));
        {
            gate.0.lock().unwrap().1 = true;
            gate.1.notify_all();
        }
        wait(|| calls.load(Ordering::SeqCst) >= 4);
        assert!(rx.try_recv().is_err());
        assert!(new_rx.try_recv().is_err());
    }
}
