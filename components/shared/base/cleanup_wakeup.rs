// SPDX-License-Identifier: MPL-2.0
//! A cancellable timer that owns only a thread-safe event-loop wake callback.
use std::{
    sync::{Arc, Condvar, Mutex},
    time::Instant,
};
#[derive(Default)]
struct Schedule {
    deadline: Option<Instant>,
    stopped: bool,
}
pub(crate) struct WakeTimer {
    shared: Arc<(Mutex<Schedule>, Condvar)>,
}
impl WakeTimer {
    pub fn new(wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let shared = Arc::new((Mutex::new(Schedule::default()), Condvar::new()));
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("XR cleanup wake".into())
            .spawn(move || {
                let (lock, signal) = &*worker;
                let mut schedule = lock.lock().unwrap();
                loop {
                    if schedule.stopped {
                        break;
                    }
                    let Some(deadline) = schedule.deadline else {
                        schedule = signal.wait(schedule).unwrap();
                        continue;
                    };
                    let now = Instant::now();
                    if deadline > now {
                        schedule = signal.wait_timeout(schedule, deadline - now).unwrap().0;
                        continue;
                    }
                    schedule.deadline = None;
                    drop(schedule);
                    wake();
                    schedule = lock.lock().unwrap();
                }
            })?;
        Ok(Self { shared })
    }
    pub fn schedule(&self, deadline: Option<Instant>) {
        let (lock, signal) = &*self.shared;
        lock.lock().unwrap().deadline = deadline;
        signal.notify_one();
    }
}
impl Drop for WakeTimer {
    fn drop(&mut self) {
        let (lock, signal) = &*self.shared;
        lock.lock().unwrap().stopped = true;
        signal.notify_one();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};
    #[test]
    fn dropping_registration_stops_waiting_timer() {
        let (send, recv) = mpsc::channel();
        let timer = WakeTimer::new(move || {
            let _ = send.send(());
        })
        .unwrap();
        timer.schedule(Some(Instant::now() + Duration::from_secs(10)));
        drop(timer);
        assert_eq!(
            recv.recv_timeout(Duration::from_secs(2)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }
    #[test]
    fn rescheduling_coalesces_and_does_not_repeat_wakeup() {
        let (send, recv) = mpsc::channel();
        let timer = WakeTimer::new(move || {
            let _ = send.send(());
        })
        .unwrap();
        timer.schedule(Some(Instant::now() + Duration::from_secs(10)));
        timer.schedule(Some(Instant::now()));
        recv.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            recv.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
    }
}
