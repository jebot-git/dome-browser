// SPDX-License-Identifier: GPL-3.0-or-later
//! Return unused native heap pages without changing system swap policy or
//! discarding live documents. SpiderMonkey's GC configuration stays unchanged.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod platform {
    use std::sync::mpsc::{self, SyncSender};
    use std::time::{Duration, Instant};

    const INTERVAL: Duration = Duration::from_secs(30);
    const MIN_FREE: usize = 32 * 1024 * 1024;
    const RETAIN: usize = 128 * 1024;

    fn enabled() -> bool {
        std::env::var_os("DOME_BROWSER_MEMORY_RECLAIM").is_none_or(|value| value != "0")
    }

    pub fn configure_allocator() {
        if !enabled() { return; }
        // Native rendering, networking and script threads share glibc's heap.
        // Bound arena proliferation before creating those threads. Preserve
        // explicit allocator settings supplied by the application launcher.
        let explicit = std::env::var_os("MALLOC_ARENA_MAX").is_some()
            || std::env::var("GLIBC_TUNABLES").ok().is_some_and(|value|
                value.split(':').any(|item| item.starts_with("glibc.malloc.arena_max=")));
        if !explicit {
            // mallopt is glibc's synchronized allocator configuration API.
            unsafe { libc::mallopt(libc::M_ARENA_MAX, 8); }
        }
    }

    pub struct Reclaimer { requests: SyncSender<()> }

    impl Reclaimer {
        pub fn start() -> Option<Self> {
            if !enabled() { return None; }
            let (requests, receiver) = mpsc::sync_channel(1);
            let trace = std::env::var_os("DOME_BROWSER_MEMORY_TRACE").is_some_and(|value| value == "1");
            std::thread::Builder::new().name("Native heap reclaim".into()).spawn(move || {
                let mut last_check = Instant::now();
                loop {
                    let wait = INTERVAL.saturating_sub(last_check.elapsed());
                    match receiver.recv_timeout(wait) {
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        _ if last_check.elapsed() < INTERVAL => continue,
                        _ => (),
                    }
                    last_check = Instant::now();
                    // Only allocator-owned free blocks can be purged. This
                    // neither scans the DOM/JS heap nor forces a collection.
                    let before = unsafe { libc::mallinfo2() };
                    if before.fordblks >= MIN_FREE {
                        let start = Instant::now();
                        let released = unsafe { libc::malloc_trim(RETAIN) };
                        let elapsed = start.elapsed();
                        if trace || elapsed >= Duration::from_millis(250) {
                            eprintln!("MEMORY_RECLAIM free_bytes={} allocated_bytes={} released={} elapsed_ms={}",
                                before.fordblks, before.uordblks, released, elapsed.as_millis());
                        }
                    } else if trace {
                        eprintln!("MEMORY_RECLAIM free_bytes={} allocated_bytes={} skipped=below-threshold",
                            before.fordblks, before.uordblks);
                    }
                }
            }).ok()?;
            // The detached thread owns no browser/engine handles. Dropping the
            // last sender wakes it immediately; shutdown never waits on a purge.
            Some(Self { requests })
        }

        pub fn request(&self) { let _ = self.requests.try_send(()); }
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
mod platform {
    pub fn configure_allocator() {}
    pub struct Reclaimer;
    impl Reclaimer {
        pub fn start() -> Option<Self> { None }
        pub fn request(&self) {}
    }
}

pub use platform::{Reclaimer, configure_allocator};
