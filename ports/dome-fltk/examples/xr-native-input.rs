// SPDX-License-Identifier: GPL-3.0-or-later
//! Test-only embedder for trusted native touch/pen events. Not an installed browser interface.
#![allow(dead_code)]
// Reuse the shipping inline backend; this test has no immersive device or UI.
#[path = "../src/xr/inline.rs"]
mod inline;
use euclid::RigidTransform3D;
use webxr::SurfmanGL;
use webxr_api::{
    ContextId, DeviceAPI, DiscoveryAPI, Error, Event, EventBuffer, Floor, Frame, InputSource,
    LayerId, LayerInfo, LayerInit, Native, Quitter, Session, SessionBuilder, SessionInit, SessionMode,
    ViewerPose, Viewports, Views,
};
fn backend(error: impl std::fmt::Debug) -> Error {
    Error::BackendSpecific(format!("{error:?}"))
}
struct Registry;
impl servo::webxr::WebXrRegistry for Registry {
    fn register(&self, registry: &mut servo::webxr::MainThreadRegistry) {
        registry.register(inline::Discovery);
        if std::env::var_os("XR_INPUT_MOCK_XR").is_some() {
            registry.register_mock(servo::webxr::HeadlessMockDiscovery::new(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true))));
        }
    }
}
use serde_json::{Value, json};
use servo::{
    EventLoopWaker, InputEvent, InputEventId, InputEventResult, RenderingContext, ServoBuilder,
    SoftwareRenderingContext, TouchEvent, TouchEventType, TouchId, TouchPointerType, WebView,
    WebViewBuilder, WebViewDelegate,
};
use std::{
    cell::{Cell, RefCell},
    io::{self, BufRead, Write},
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

fn reply(value: Value) {
    println!("XR_INPUT {}", value);
    io::stdout().flush().unwrap();
}
#[derive(Clone)]
struct Waker;
impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {}
}
#[derive(Default)]
struct State {
    dirty: Cell<bool>,
    pending: RefCell<Vec<(InputEventId, u64)>>,
}
impl WebViewDelegate for State {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.dirty.set(true);
    }
    fn notify_input_event_handled(&self, _: WebView, id: InputEventId, result: InputEventResult) {
        let mut pending = self.pending.borrow_mut();
        let index = pending.iter().position(|(expected, _)| *expected == id)
            .expect("Every external event must complete exactly once; internal events stay private");
        let (_, token) = pending.remove(index);
        reply(json!({"token":token,"handled":true,"failed":result.contains(InputEventResult::DispatchFailed)}));
    }
    fn notify_crashed(&self, _: WebView, reason: String, trace: Option<String>) {
        eprintln!("Test webview crashed: {reason} {trace:?}");
        std::process::exit(2);
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .unwrap();
    let url = url::Url::parse(&std::env::args().nth(1).ok_or("fixture URL required")?)?;
    let size = dpi::PhysicalSize::new(900, 650);
    let context = Rc::new(SoftwareRenderingContext::new(size).map_err(|e| format!("{e:?}"))?);
    context.make_current().map_err(|e| format!("{e:?}"))?;
    let mut opts = servo::Opts::default();
    opts.multiprocess = false;
    opts.config_dir = Some(std::path::PathBuf::from(std::env::var("XR_INPUT_PROFILE")?));
    std::fs::create_dir_all(opts.config_dir.as_ref().unwrap())?;
    let mut prefs = servo::Preferences::default();
    prefs.dom_webxr_glwindow_enabled = false;
    prefs.dom_webxr_openxr_enabled = false;
    prefs.dom_webxr_hands_enabled = false;
    prefs.dom_webxr_layers_enabled = false;
    prefs.dom_webgl2_enabled = true;
    if std::env::var_os("XR_INPUT_POLICY_TEST").is_some() {
        prefs.dom_permissions_enabled = true;
    }
    // Only this separate test embedder can expose mock devices; the browser
    // keeps its normal consent and device-discovery configuration.
    if std::env::var_os("XR_INPUT_MOCK_XR").is_some() {
        prefs.dom_webxr_test = true;
        prefs.dom_webxr_unsafe_assume_user_intent = true;
    }
    let servo = ServoBuilder::default()
        .opts(opts)
        .preferences(prefs)
        .event_loop_waker(Box::new(Waker))
        .build();
    servo.register_webxr_registry(Box::new(Registry));
    let state = Rc::new(State::default());
    let view = WebViewBuilder::new(&servo, context.clone())
        .url(url)
        .delegate(state.clone())
        .build();
    view.show();
    view.set_focused(true);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut quit = false;
    while !quit && Instant::now() < deadline {
        context.make_current().map_err(|e| format!("{e:?}"))?;
        servo.spin_event_loop();
        if state.dirty.replace(false) {
            view.paint();
            context.present();
        }
        if let Ok(line) = rx.try_recv() {
            let command: Value = serde_json::from_str(&line?)?;
            let token = command["token"].as_u64().ok_or("token required")?;
            match command["op"].as_str().ok_or("op required")? {
                "touch" => {
                    assert!(
                        state.pending.borrow().is_empty(),
                        "Only one native event may be pending"
                    );
                    let phase = match command["phase"].as_str() {
                        Some("down") => TouchEventType::Down,
                        Some("move") => TouchEventType::Move,
                        Some("up") => TouchEventType::Up,
                        Some("cancel") => TouchEventType::Cancel,
                        _ => return Err("Unknown touch phase".into()),
                    };
                    let kind = match command["kind"].as_str() {
                        Some("touch") => TouchPointerType::Touch,
                        Some("pen") => TouchPointerType::Pen,
                        _ => return Err("Unknown pointer kind".into()),
                    };
                    let mut event = TouchEvent::new(
                        phase,
                        TouchId(command["id"].as_i64().ok_or("id required")?.try_into()?),
                        servo::DevicePoint::new(
                            command["x"].as_f64().ok_or("x required")? as f32,
                            command["y"].as_f64().ok_or("y required")? as f32,
                        )
                        .into(),
                        kind,
                    );
                    event.can_synthesize_mouse_events =
                        command["compat"].as_bool().unwrap_or(false);
                    let id = view.notify_input_event(InputEvent::Touch(event));
                    state.pending.borrow_mut().push((id, token));
                },
                "pen-batch" => {
                    assert!(state.pending.borrow().is_empty());
                    for sample in command["samples"].as_array().ok_or("samples required")? {
                        let phase = match sample["phase"].as_str() {
                            Some("sample") => servo::PenEventType::Sample,
                            Some("leave") => servo::PenEventType::Leave,
                            Some("cancel") => servo::PenEventType::Cancel,
                            _ => return Err("Unknown pen phase".into()),
                        };
                        let mut event = servo::PenEvent::new(phase,
                            sample["id"].as_i64().unwrap_or(1).try_into()?,
                            servo::DevicePoint::new(sample["x"].as_f64().unwrap_or(80.) as f32,
                                sample["y"].as_f64().unwrap_or(100.) as f32).into());
                        event.buttons = sample["buttons"].as_u64().unwrap_or(0).try_into()?;
                        event.pressure = if event.buttons != 0 { 0.5 } else { 0. };
                        let id = view.notify_input_event(InputEvent::Pen(event));
                        state.pending.borrow_mut().push((id, sample["token"].as_u64().ok_or("sample token required")?));
                    }
                    reply(json!({"token":token,"queued":true}));
                },
                "eval" => {
                    let script = command["script"].as_str().ok_or("script required")?;
                    view.evaluate_javascript(script, move |result| match result {
                        Ok(servo::JSValue::String(text)) => {
                            reply(json!({"token":token,"value":text}))
                        },
                        other => reply(json!({"token":token,"error":format!("{other:?}")})),
                    });
                },
                "hide" => {
                    view.hide();
                    reply(json!({"token":token,"hidden":true}));
                },
                "show" => {
                    view.show();
                    reply(json!({"token":token,"shown":true}));
                },
                "quit" => {
                    quit = true;
                    reply(json!({"token":token,"quit":true}));
                },
                _ => return Err("Unknown operation".into()),
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if !quit {
        return Err("Test harness deadline expired".into());
    }
    drop(view);
    drop(servo);
    Ok(())
}
