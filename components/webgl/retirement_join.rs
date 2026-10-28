// SPDX-License-Identifier: MPL-2.0
use std::{
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
/// Wait briefly for cleanup, then detach its owning thread. Native resources
/// remain on that thread; no context or !Send wrapper crosses thread boundaries.
pub fn join_retiring_thread(handle: JoinHandle<()>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        thread::sleep(remaining.min(Duration::from_millis(5)));
    }
    handle.join().is_ok()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_cleanup_detaches_without_dropping_its_local_owner() {
        let (started, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let (done, finished) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            let owner = std::rc::Rc::new(23);
            started.send(()).unwrap();
            wait.recv().unwrap();
            done.send(*owner).unwrap();
        });
        ready.recv().unwrap();
        assert!(!join_retiring_thread(handle, Duration::from_millis(10)));
        assert!(finished.try_recv().is_err());
        release.send(()).unwrap();
        assert_eq!(finished.recv_timeout(Duration::from_secs(2)).unwrap(), 23);
    }
    #[test]
    fn completed_cleanup_is_joined() {
        assert!(join_retiring_thread(
            thread::spawn(|| {}),
            Duration::from_secs(1)
        ));
    }
}
