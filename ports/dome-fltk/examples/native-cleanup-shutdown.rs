// SPDX-License-Identifier: GPL-3.0-or-later
//! Isolated embedder regression for owner-thread retirement at final shutdown.
//! Uses private offscreen contexts; never resets the GPU or controls a desktop.
use servo::{
    EventLoopWaker, RenderingContext, ServoBuilder, SoftwareRenderingContext, WebViewBuilder,
};
use servo_base::deferred_cleanup::{self, CleanupScope};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use surfman::{Connection, Error};

#[derive(Clone)]
struct Waker;
impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {}
}
struct Context {
    inner: SoftwareRenderingContext,
    fail: Arc<AtomicBool>,
    attempts: Rc<Cell<usize>>,
    host_alive: Rc<Cell<bool>>,
    expose_scope: bool,
}
impl RenderingContext for Context {
    fn read_to_image(&self, rect: servo::DeviceIntRect) -> Option<image::RgbaImage> {
        self.inner.read_to_image(rect)
    }
    fn size(&self) -> dpi::PhysicalSize<u32> {
        self.inner.size()
    }
    fn resize(&self, size: dpi::PhysicalSize<u32>) {
        self.inner.resize(size);
    }
    fn present(&self) {
        self.inner.present();
    }
    fn make_current(&self) -> Result<(), Error> {
        assert!(
            self.host_alive.get(),
            "retirement called after native host release"
        );
        self.attempts.set(self.attempts.get() + 1);
        if self.fail.load(Ordering::Acquire) {
            return Err(Error::Failed);
        }
        self.inner.make_current()
    }
    fn native_context_is_lost(&self) -> bool {
        self.inner.native_context_is_lost()
    }
    fn gleam_gl_api(&self) -> Rc<dyn gleam::gl::Gl> {
        self.inner.gleam_gl_api()
    }
    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.inner.glow_gl_api()
    }
    fn connection(&self) -> Option<Connection> {
        self.inner.connection()
    }
    fn cleanup_scope(&self) -> Option<CleanupScope> {
        if self.expose_scope {
            self.inner.cleanup_scope()
        } else {
            None
        }
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .unwrap();
    let scenario = std::env::args()
        .nth(1)
        .ok_or("recover, retain or fallback required")?;
    assert!(["recover", "retain", "fallback"].contains(&scenario.as_str()));
    let size = dpi::PhysicalSize::new(64, 64);
    let inner = if std::env::var_os("DOME_CREATION_SOFTWARE").is_some() {
        SoftwareRenderingContext::new(size)
    } else {
        SoftwareRenderingContext::new_accelerated(size)
    }
    .map_err(|e| format!("{e:?}"))?;
    inner.make_current().map_err(|e| format!("{e:?}"))?;
    let scope = inner.cleanup_scope().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let attempts = Rc::new(Cell::new(0));
    let host_alive = Rc::new(Cell::new(true));
    let context = Rc::new(Context {
        inner,
        fail: fail.clone(),
        attempts: attempts.clone(),
        host_alive: host_alive.clone(),
        expose_scope: scenario != "fallback",
    });
    let weak = Rc::downgrade(&context);
    let mut opts = servo::Opts::default();
    opts.multiprocess = false;
    opts.config_dir = Some(std::path::PathBuf::from(std::env::var(
        "DOME_SHUTDOWN_PROFILE",
    )?));
    std::fs::create_dir_all(opts.config_dir.as_ref().unwrap())?;
    let servo = ServoBuilder::default()
        .opts(opts)
        .event_loop_waker(Box::new(Waker))
        .build();
    let view = WebViewBuilder::new(&servo, context.clone()).build();
    fail.store(true, Ordering::Release);
    drop(view);
    assert!(weak.upgrade().is_some());
    drop(context);
    assert!(
        weak.upgrade().is_some(),
        "failed retirement must own its context"
    );
    if scenario != "retain" {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(750));
            fail.store(false, Ordering::Release);
        });
    }
    let start = Instant::now();
    drop(servo);
    let elapsed = start.elapsed();
    host_alive.set(false);
    let final_attempts = attempts.get();
    if scenario == "retain" {
        assert!(
            elapsed >= Duration::from_secs(2),
            "final cleanup grace period was skipped"
        );
        assert!(
            elapsed < Duration::from_secs(8),
            "cleanup did not remain bounded"
        );
        assert!(weak.upgrade().is_some() && scope.pending());
        assert!(
            deferred_cleanup::poll(),
            "timed-out work must no longer be scheduled"
        );
        assert_eq!(attempts.get(), final_attempts);
    } else {
        assert!(
            weak.upgrade().is_none(),
            "Servo returned before retiring its renderer"
        );
        assert!(
            !scope.pending(),
            "final native-context retirement did not complete"
        );
        assert!(elapsed < Duration::from_secs(8));
        assert!(deferred_cleanup::poll());
    }
    println!(
        "NATIVE_SHUTDOWN {}",
        serde_json::json!({
            "scenario":scenario,"passed":true,"binding_attempts":final_attempts,
            "shutdown_ms":elapsed.as_millis(),"retained":scenario=="retain"
        })
    );
    Ok(())
}
