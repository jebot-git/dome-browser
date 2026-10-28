// SPDX-License-Identifier: GPL-3.0-or-later
//! Isolated embedder regression for owner-thread retirement at final shutdown.
//! Uses private offscreen contexts; never resets the GPU or controls a desktop.
#[path = "../src/graphics.rs"]
mod graphics;
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
    fn prepare_for_rendering(&self) {
        if !self.fail.load(Ordering::Acquire) {
            self.inner.prepare_for_rendering();
        }
    }
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
        .ok_or("recover, retain, fallback, render, application or create required")?;
    assert!(
        [
            "recover",
            "retain",
            "fallback",
            "render",
            "application",
            "create"
        ]
        .contains(&scenario.as_str())
    );
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
    let mut prefs = servo::Preferences::default();
    prefs.shell_background_color_rgba = [1., 0., 0., 1.];
    let servo = ServoBuilder::default()
        .opts(opts)
        .preferences(prefs)
        .event_loop_waker(Box::new(Waker))
        .build();
    let view = WebViewBuilder::new(&servo, context.clone()).build();
    if ["render", "application", "create"].contains(&scenario.as_str()) {
        let peer = if std::env::var_os("DOME_CREATION_SOFTWARE").is_some() {
            SoftwareRenderingContext::new(size)
        } else {
            SoftwareRenderingContext::new_accelerated(size)
        }
        .map_err(|error| format!("{error:?}"))?;
        peer.make_current().map_err(|error| format!("{error:?}"))?;
        peer.prepare_for_rendering();
        let gl = peer.gleam_gl_api();
        gl.clear_color(0., 0., 1., 1.);
        gl.clear(gleam::gl::COLOR_BUFFER_BIT);
        let rect = servo::DeviceIntRect::new(
            servo::DeviceIntPoint::new(0, 0),
            servo::DeviceIntPoint::new(64, 64),
        );
        if scenario == "create" {
            let inner = if std::env::var_os("DOME_CREATION_SOFTWARE").is_some() {
                SoftwareRenderingContext::new(size)
            } else {
                SoftwareRenderingContext::new_accelerated(size)
            }
            .map_err(|error| format!("{error:?}"))?;
            let unavailable = Arc::new(AtomicBool::new(true));
            let candidate = Rc::new(Context {
                inner,
                fail: unavailable.clone(),
                attempts: Rc::new(Cell::new(0)),
                host_alive: host_alive.clone(),
                expose_scope: true,
            });
            peer.make_current().map_err(|error| format!("{error:?}"))?;
            peer.prepare_for_rendering();
            for _ in 0..3 {
                assert!(
                    WebViewBuilder::new(&servo, candidate.clone())
                        .try_build()
                        .is_err()
                );
                assert!(
                    peer.read_to_image(rect)
                        .unwrap()
                        .pixels()
                        .all(|pixel| pixel.0 == [0, 0, 255, 255])
                );
            }
            unavailable.store(false, Ordering::Release);
            let recovered = WebViewBuilder::new(&servo, candidate.clone())
                .try_build()
                .map_err(|error| format!("{error:?}"))?;
            recovered.paint();
            assert!(
                candidate
                    .read_to_image(rect)
                    .unwrap()
                    .pixels()
                    .all(|pixel| pixel.0 == [255, 0, 0, 255])
            );
            drop(recovered);
            drop(candidate);
        } else if scenario == "application" {
            let retry = graphics::RetryState::default();
            let now = Instant::now();
            fail.store(true, Ordering::Release);
            assert!(graphics::capture(&*context, || view.paint()).is_err());
            assert!(retry.failure(now) && !retry.due(now));
            assert!(
                peer.read_to_image(rect)
                    .unwrap()
                    .pixels()
                    .all(|pixel| pixel.0 == [0, 0, 255, 255])
            );
            fail.store(false, Ordering::Release);
            assert!(retry.due(now + Duration::from_secs(1)));
            let image = graphics::capture(&*context, || view.paint())
                .map_err(|error| format!("{error:?}"))?;
            assert!(image.pixels().all(|pixel| pixel.0 == [255, 0, 0, 255]));
            assert!(retry.recover());
        } else {
            fail.store(true, Ordering::Release);
            view.paint();
            assert!(
                peer.read_to_image(rect)
                    .unwrap()
                    .pixels()
                    .all(|pixel| pixel.0 == [0, 0, 255, 255]),
                "unavailable frame must preserve the peer target"
            );
            fail.store(false, Ordering::Release);
            view.paint();
            assert!(
                context
                    .read_to_image(rect)
                    .unwrap()
                    .pixels()
                    .all(|pixel| pixel.0 == [255, 0, 0, 255]),
                "a later available frame must render normally"
            );
        }
        drop(view);
        drop(context);
        drop(servo);
        assert!(
            peer.read_to_image(rect)
                .unwrap()
                .pixels()
                .all(|pixel| pixel.0 == [0, 0, 255, 255])
        );
        assert!(!scope.pending());
        host_alive.set(false);
        println!(
            "NATIVE_RENDER {}",
            serde_json::json!({"scenario":scenario,"passed":true,"failed_frame_skipped":true,"recovered":true})
        );
        return Ok(());
    }
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
