// SPDX-License-Identifier: GPL-3.0-or-later
mod ui;
mod xr;
use servo::{
    Code, EmbedderControl, InputEvent, Key, KeyState, KeyboardEvent, LoadStatus, Location,
    Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, NamedKey,
    PermissionFeature, PermissionRequest, RenderingContext, ServoBuilder, SimpleDialog,
    SoftwareRenderingContext, Theme, WebView, WebViewBuilder, WebViewDelegate, WheelDelta,
    WheelEvent, WheelMode,
};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;

#[derive(Clone)]
struct Waker(Arc<AtomicBool>);
struct Clipboard;
impl servo::ClipboardDelegate for Clipboard {
    fn clear(&self, _: WebView) {
        if std::env::var_os("DOME_BROWSER_TRACE").is_some() {
            eprintln!("CLIPBOARD_CLEAR");
        }
        unsafe { ui::browser_ui_copy(ui::c("").as_ptr()) }
    }
    fn set_text(&self, _: WebView, value: String) {
        if std::env::var_os("DOME_BROWSER_TRACE").is_some() {
            eprintln!("CLIPBOARD_SET {}", value.len());
        }
        unsafe { ui::browser_ui_copy(ui::c(&value).as_ptr()) }
    }
    fn get_text(&self, _: WebView, request: servo::StringRequest) {
        if let Some(value) = ui::string(unsafe { ui::browser_ui_paste() }) {
            if std::env::var_os("DOME_BROWSER_TRACE").is_some() {
                eprintln!("CLIPBOARD_GET {}", value.len());
            }
            request.success(value);
        } else {
            request.failure("The system clipboard has no text".into());
        }
    }
}
impl servo::EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn servo::EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        self.0.store(true, Ordering::Release);
        unsafe { ui::browser_ui_wake() }
    }
}
struct PageState {
    dirty: Cell<bool>,
    title: RefCell<String>,
    url: RefCell<String>,
    history: Cell<(usize, usize)>,
    changed: Cell<bool>,
    loaded: Cell<bool>,
    closed: Cell<bool>,
    pending: RefCell<Vec<servo::CreateNewWebViewRequest>>,
    tab: usize,
    permissions: Rc<RefCell<std::collections::HashSet<String>>>,
}
impl WebViewDelegate for PageState {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.dirty.set(true)
    }
    fn notify_page_title_changed(&self, _: WebView, title: Option<String>) {
        *self.title.borrow_mut() = title.unwrap_or_else(|| "Untitled".into());
        self.changed.set(true)
    }
    fn notify_url_changed(&self, _: WebView, url: Url) {
        *self.url.borrow_mut() = url.to_string();
        self.changed.set(true);
    }
    fn notify_history_changed(&self, _: WebView, entries: Vec<Url>, current: usize) {
        self.history.set((current, entries.len()));
        self.changed.set(true)
    }
    fn notify_load_status_changed(&self, _: WebView, status: LoadStatus) {
        self.loaded.set(status == LoadStatus::Complete);
        ui::status(if self.loaded.get() {
            "Ready"
        } else {
            "Loading…"
        });
    }
    fn notify_closed(&self, _: WebView) {
        self.closed.set(true)
    }
    fn notify_cursor_changed(&self, _: WebView, cursor: servo::Cursor) {
        let s = format!("{cursor:?}");
        unsafe {
            ui::browser_ui_cursor(if s == "Pointer" {
                1
            } else if s == "Text" {
                2
            } else {
                0
            })
        }
    }
    fn notify_fullscreen_state_changed(&self, _: WebView, enabled: bool) {
        unsafe { ui::browser_ui_fullscreen(enabled as i32) }
    }
    fn show_console_message(&self, _: WebView, level: servo::ConsoleLogLevel, message: String) {
        eprintln!("PAGE {level:?} {message}");
    }
    fn notify_crashed(&self, _: WebView, reason: String, _: Option<String>) {
        ui::status(&format!("Page stopped: {reason}"));
        eprintln!("PAGE_CRASH {reason}");
    }
    fn request_create_new(&self, _: WebView, request: servo::CreateNewWebViewRequest) {
        self.pending.borrow_mut().push(request);
    }
    fn request_permission(&self, view: WebView, request: PermissionRequest) {
        let origin = view
            .url()
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_else(|| "null".into());
        let feature = format!("{:?}", request.feature());
        if matches!(request.feature(), PermissionFeature::Notifications)
            && self.permissions.borrow().contains(&origin)
        {
            request.allow();
            return;
        }
        if matches!(request.feature(), PermissionFeature::Notifications)
            && ui::ask(
                "Site permission",
                &format!("{origin}\n\nAllow this site to display desktop notifications?"),
            )
        {
            self.permissions.borrow_mut().insert(origin);
            request.allow();
        } else {
            ui::status(&format!("{feature} permission denied"));
            request.deny();
        }
    }
    fn request_authentication(&self, _: WebView, request: servo::AuthenticationRequest) {
        let site = request.url().origin().ascii_serialization();
        let username = ui::string(unsafe {
            ui::browser_ui_prompt(
                ui::c(&format!("{site}\nHTTP authentication: username")).as_ptr(),
                ui::c("").as_ptr(),
            )
        });
        let Some(username) = username else {
            return;
        };
        let password = ui::string(unsafe {
            ui::browser_ui_password(
                ui::c(&format!("{site}\nHTTP authentication: password")).as_ptr(),
            )
        });
        if let Some(password) = password {
            request.authenticate(username, password);
        }
    }
    fn show_notification(&self, view: WebView, notice: servo::Notification) {
        let origin = view
            .url()
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_else(|| "null".into());
        if !self.permissions.borrow().contains(&origin) {
            return;
        }
        let key = format!(
            "{}:{}:{}",
            self.tab,
            origin,
            if notice.tag.is_empty() {
                format!("{:?}", Instant::now())
            } else {
                notice.tag
            }
        );
        let payload = serde_json::json!({"key":key,"title":notice.title,"body":format!("{origin}\n{}",notice.body),"tab":self.tab,"kind":"browser","actions":[["default","Open tab"]]});
        unsafe {
            ui::browser_ui_notification(ui::c(&payload.to_string()).as_ptr());
        }
    }
    fn show_embedder_control(&self, view: WebView, control: EmbedderControl) {
        match control {
            EmbedderControl::ColorPicker(mut picker) => {
                let mut color = picker.current_color().unwrap_or(servo::RgbColor {
                    red: 0,
                    green: 0,
                    blue: 0,
                });
                if unsafe {
                    ui::browser_ui_color(&mut color.red, &mut color.green, &mut color.blue)
                } != 0
                {
                    picker.select(Some(color));
                    picker.submit();
                }
            },
            EmbedderControl::SimpleDialog(dialog) => {
                let origin = view
                    .url()
                    .map(|u| u.origin().ascii_serialization())
                    .unwrap_or_else(|| "null".into());
                match dialog {
                    SimpleDialog::Alert(d) => {
                        ui::ask("Message from page", &format!("{origin}\n\n{}", d.message()));
                        d.confirm();
                    },
                    SimpleDialog::Confirm(d) => {
                        if ui::ask(
                            "Question from page",
                            &format!("{origin}\n\n{}", d.message()),
                        ) {
                            d.confirm()
                        } else {
                            d.dismiss()
                        }
                    },
                    SimpleDialog::Prompt(mut d) => {
                        let value = unsafe {
                            ui::browser_ui_prompt(
                                ui::c(&format!("{origin}\n\n{}", d.message())).as_ptr(),
                                ui::c(d.current_value()).as_ptr(),
                            )
                        };
                        if let Some(answer) = ui::string(value) {
                            d.set_current_value(&answer);
                            d.confirm()
                        } else {
                            d.dismiss()
                        }
                    },
                }
            },
            EmbedderControl::FilePicker(mut picker) => {
                let paths = ui::string(unsafe {
                    ui::browser_ui_file(picker.allow_select_multiple() as i32)
                });
                if let Some(paths) =
                    paths.and_then(|s| serde_json::from_str::<Vec<PathBuf>>(&s).ok())
                {
                    picker.select(&paths);
                    picker.submit()
                } else {
                    picker.dismiss()
                }
            },
            EmbedderControl::ContextMenu(menu) => {
                let choices: Vec<_> = menu
                    .items()
                    .iter()
                    .filter_map(|item| {
                        if let servo::ContextMenuItem::Item {
                            label,
                            action,
                            enabled: true,
                        } = item
                        {
                            Some((label.clone(), *action))
                        } else {
                            None
                        }
                    })
                    .collect();
                let labels: Vec<_> = choices.iter().map(|x| &x.0).collect();
                let rect = menu.position();
                let selected = unsafe {
                    ui::browser_ui_select(
                        ui::c(&serde_json::to_string(&labels).unwrap()).as_ptr(),
                        rect.min.x,
                        rect.min.y,
                    )
                };
                if let Some((_, action)) =
                    usize::try_from(selected).ok().and_then(|i| choices.get(i))
                {
                    menu.select(*action)
                } else {
                    menu.dismiss()
                }
            },
            EmbedderControl::SelectElement(mut select) => {
                let mut options = Vec::new();
                for entry in select.options() {
                    match entry {
                        servo::SelectElementOptionOrOptgroup::Option(o) => options.push(o.clone()),
                        servo::SelectElementOptionOrOptgroup::Optgroup { options: o, .. } => {
                            options.extend(o.clone())
                        },
                    }
                }
                let options: Vec<_> = options.into_iter().filter(|o| !o.is_disabled).collect();
                let labels: Vec<_> = options.iter().map(|o| &o.label).collect();
                let rect = select.position();
                let chosen = unsafe {
                    ui::browser_ui_select(
                        ui::c(&serde_json::to_string(&labels).unwrap()).as_ptr(),
                        rect.min.x,
                        rect.min.y,
                    )
                };
                if let Some(option) = usize::try_from(chosen).ok().and_then(|i| options.get(i)) {
                    select.select(vec![option.id]);
                    select.submit();
                }
            },
            _ => {},
        }
    }
}
struct Tab {
    view: WebView,
    context: Rc<dyn RenderingContext>,
    state: Rc<PageState>,
    zoom: f32,
}
fn context(software: bool) -> Result<Rc<dyn RenderingContext>, Box<dyn std::error::Error>> {
    let size = ui::dimensions();
    let context = if software {
        SoftwareRenderingContext::new(size)
    } else {
        SoftwareRenderingContext::new_accelerated(size)
    }
    .map_err(|e| format!("Cannot create rendering context: {e:?}"))?;
    let context = Rc::new(context);
    context
        .make_current()
        .map_err(|e| format!("Cannot make context current: {e:?}"))?;
    Ok(context)
}
fn state(id: usize, permissions: Rc<RefCell<std::collections::HashSet<String>>>) -> Rc<PageState> {
    Rc::new(PageState {
        dirty: Cell::new(true),
        title: RefCell::new("New tab".into()),
        url: RefCell::new(String::new()),
        history: Cell::new((0, 0)),
        changed: Cell::new(true),
        loaded: Cell::new(false),
        closed: Cell::new(false),
        pending: RefCell::new(Vec::new()),
        tab: id,
        permissions,
    })
}
fn parse_url(text: &str) -> Result<Url, url::ParseError> {
    let text = text.trim();
    if text.is_empty() {
        return Url::parse("about:blank");
    }
    if std::path::Path::new(text).is_absolute() {
        return Ok(Url::from_file_path(text).unwrap());
    }
    if let Ok(url) = Url::parse(text) {
        return Ok(url);
    }
    if !text.contains(' ') && (text.contains('.') || text.starts_with("localhost")) {
        return Url::parse(&format!("https://{text}"));
    }
    let mut url = Url::parse("https://duckduckgo.com/")?;
    url.query_pairs_mut().append_pair("q", text);
    Ok(url)
}
fn activate(tabs: &[Tab], index: usize) {
    for (i, tab) in tabs.iter().enumerate() {
        if i == index {
            tab.view.show();
            tab.view
                .notify_theme_change(if unsafe { ui::browser_ui_light() } != 0 {
                    Theme::Light
                } else {
                    Theme::Dark
                });
            tab.view.set_focused(true);
            tab.view.resize(ui::dimensions());
            tab.state.dirty.set(true);
            tab.state.changed.set(true);
        } else {
            tab.view.hide();
            tab.view.set_focused(false);
        }
    }
}
fn key(event: &ui::Event) -> InputEvent {
    let named = match event.b {
        0xffe1 | 0xffe2 => Some(NamedKey::Shift),
        0xffe3 | 0xffe4 => Some(NamedKey::Control),
        0xffe7 | 0xffe8 | 0xffeb | 0xffec => Some(NamedKey::Meta),
        0xffe9 | 0xffea => Some(NamedKey::Alt),
        0xfe03 => Some(NamedKey::AltGraph),
        0xffe5 => Some(NamedKey::CapsLock),
        0xff7f => Some(NamedKey::NumLock),
        0xff14 => Some(NamedKey::ScrollLock),
        0xff0d => Some(NamedKey::Enter),
        0xff1b => Some(NamedKey::Escape),
        0xff08 => Some(NamedKey::Backspace),
        0xffff => Some(NamedKey::Delete),
        0xff09 => Some(NamedKey::Tab),
        0xff50 => Some(NamedKey::Home),
        0xff57 => Some(NamedKey::End),
        0xff51 => Some(NamedKey::ArrowLeft),
        0xff53 => Some(NamedKey::ArrowRight),
        0xff52 => Some(NamedKey::ArrowUp),
        0xff54 => Some(NamedKey::ArrowDown),
        0xff55 => Some(NamedKey::PageUp),
        0xff56 => Some(NamedKey::PageDown),
        _ => None,
    };
    let text = if event.c & (0x40000 | 0x80000) != 0 && (32..127).contains(&event.b) {
        char::from_u32(event.b as u32).unwrap().to_string()
    } else {
        event.text()
    };
    let key = named.map(Key::Named).unwrap_or_else(|| {
        Key::Character(if text.is_empty() && (32..127).contains(&event.b) {
            char::from_u32(event.b as u32).unwrap().to_string()
        } else {
            if text.is_empty() {
                return Key::Named(NamedKey::Unidentified);
            }
            text
        })
    });
    let mut modifiers = Modifiers::empty();
    if event.c & 0x10000 != 0 {
        modifiers |= Modifiers::SHIFT;
    }
    if event.c & 0x40000 != 0 {
        modifiers |= Modifiers::CONTROL;
    }
    if event.c & 0x80000 != 0 {
        modifiers |= Modifiers::ALT;
    }
    if event.c & 0x400000 != 0 {
        modifiers |= Modifiers::META;
    }
    InputEvent::Keyboard(KeyboardEvent::new_without_event(
        if event.a != 0 {
            KeyState::Down
        } else {
            KeyState::Up
        },
        key,
        Code::Unidentified,
        Location::Standard,
        modifiers,
        false,
        false,
    ))
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help") {
        println!(
            "dome-browser [--software] [--profile DIRECTORY] [URL]\n  --smoke-test FILE.ppm  Capture after load and exit\n  --xr-probe            Query DomeD's XR endpoint without acquiring a session\n  --version             Show engine and toolkit versions"
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--version") {
        println!("Dome Browser 0.1.0 / Servo 0.7.0 / FLTK 1.4.5");
        return Ok(());
    }
    if args.iter().any(|a| a == "--xr-probe") {
        println!("{}", xr::probe()?);
        return Ok(());
    }
    let software = args.iter().any(|a| a == "--software");
    let mut target = "about:blank".to_string();
    let mut screenshot = None;
    let mut profile = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--software" => {},
            "--smoke-test" => {
                i += 1;
                screenshot = Some(args.get(i).ok_or("--smoke-test needs a path")?.clone());
            },
            "--profile" => {
                i += 1;
                profile = Some(PathBuf::from(args.get(i).ok_or("--profile needs a path")?));
            },
            s if s.starts_with('-') => return Err(format!("Unknown option: {s}").into()),
            s => target = s.into(),
        }
        i += 1;
    }
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    if unsafe { ui::browser_ui_open(ui::c(parse_url(&target)?.as_str()).as_ptr()) } == 0 {
        return Ok(());
    }
    let initial = context(software)?;
    let mut prefs = servo::Preferences::default();
    prefs.dom_notification_enabled = true;
    prefs.dom_webxr_glwindow_enabled = false;
    prefs.dom_webxr_openxr_enabled = false;
    prefs.dom_webxr_hands_enabled = false;
    prefs.dom_webxr_layers_enabled = false;
    prefs.dom_webxr_unsafe_assume_user_intent = false;
    prefs.dom_webgl2_enabled = true;
    let mut opts = servo::Opts::default();
    opts.config_dir = profile.or_else(|| {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|d| d.join("dome-browser"))
    });
    opts.multiprocess = false;
    if let Some(directory) = &opts.config_dir {
        std::fs::create_dir_all(directory)?;
    }
    let wake = Arc::new(AtomicBool::new(true));
    let servo = ServoBuilder::default()
        .opts(opts)
        .preferences(prefs)
        .event_loop_waker(Box::new(Waker(wake.clone())))
        .build();
    servo.setup_logging();
    servo.register_webxr_registry(Box::new(xr::Registry::new(initial.connection())));
    let permissions = Rc::new(RefCell::new(std::collections::HashSet::new()));
    let first = state(0, permissions.clone());
    let view = WebViewBuilder::new(&servo, initial.clone())
        .clipboard_delegate(Rc::new(Clipboard))
        .url(parse_url(&target)?)
        .delegate(first.clone())
        .build();
    let mut tabs = vec![Tab {
        view,
        context: initial,
        state: first,
        zoom: 1.0,
    }];
    let mut active = 0;
    let mut next_id = 1;
    activate(&tabs, active);
    let start = Instant::now();
    let mut captures = 0;
    let mut running = true;
    while running {
        unsafe {
            ui::browser_ui_wait(if wake.load(Ordering::Acquire) {
                0.0
            } else {
                0.03
            });
        }
        let mut event = ui::Event::default();
        while unsafe { ui::browser_ui_poll(&mut event) } != 0 {
            if std::env::var_os("DOME_BROWSER_TRACE").is_some()
                && [5, 6, 7, 15, 16].contains(&event.kind)
            {
                eprintln!(
                    "BROWSER_ACTION {} {} active={} tabs={}",
                    event.kind,
                    event.a,
                    active,
                    tabs.len()
                );
            }
            match event.kind {
                1 => match parse_url(&event.text()) {
                    Ok(url) => tabs[active].view.load(url),
                    Err(e) => ui::status(&e.to_string()),
                },
                2 => {
                    tabs[active].view.go_back(1);
                },
                3 => {
                    tabs[active].view.go_forward(1);
                },
                4 => tabs[active].view.reload(),
                5 => {
                    let ctx = context(software)?;
                    let s = state(next_id, permissions.clone());
                    next_id += 1;
                    let view = WebViewBuilder::new(&servo, ctx.clone())
                        .clipboard_delegate(Rc::new(Clipboard))
                        .delegate(s.clone())
                        .url(parse_url(&event.text())?)
                        .build();
                    tabs.push(Tab {
                        view,
                        context: ctx,
                        state: s,
                        zoom: 1.,
                    });
                    active = tabs.len() - 1;
                    activate(&tabs, active);
                },
                6 => {
                    tabs.remove(active);
                    if tabs.is_empty() {
                        running = false;
                        break;
                    }
                    active = active.min(tabs.len() - 1);
                    activate(&tabs, active);
                },
                7 => {
                    active = if event.a < 0 {
                        (active + 1) % tabs.len()
                    } else {
                        (event.a as usize).min(tabs.len() - 1)
                    };
                    activate(&tabs, active);
                },
                8 => {
                    running = false;
                    break;
                },
                9 => tabs[active].view.set_focused(event.a != 0),
                10 => {
                    tabs[active].view.resize(ui::dimensions());
                    tabs[active].state.dirty.set(true);
                },
                11 => {
                    let button = match event.b {
                        1 => MouseButton::Primary,
                        2 => MouseButton::Auxiliary,
                        3 => MouseButton::Secondary,
                        _ => continue,
                    };
                    tabs[active]
                        .view
                        .notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                            if event.a != 0 {
                                MouseButtonAction::Down
                            } else {
                                MouseButtonAction::Up
                            },
                            button,
                            servo::DevicePoint::new(event.c as f32, event.d as f32).into(),
                        )));
                },
                12 => {
                    tabs[active].view.notify_input_event(InputEvent::MouseMove(
                        MouseMoveEvent::new(
                            servo::DevicePoint::new(event.a as f32, event.b as f32).into(),
                        ),
                    ));
                },
                13 => {
                    tabs[active]
                        .view
                        .notify_input_event(InputEvent::Wheel(WheelEvent::new(
                            WheelDelta {
                                x: event.a as f64 * 48.,
                                y: event.b as f64 * 48.,
                                z: 0.,
                                mode: WheelMode::DeltaPixel,
                            },
                            servo::DevicePoint::new(event.c as f32, event.d as f32).into(),
                        )));
                },
                14 => {
                    tabs[active].view.notify_input_event(key(&event));
                },
                15 => {
                    for t in &tabs {
                        t.view.notify_theme_change(if event.a != 0 {
                            Theme::Light
                        } else {
                            Theme::Dark
                        });
                    }
                },
                16 => {
                    unsafe {
                        ui::browser_ui_fullscreen(0);
                    }
                    tabs[active].view.evaluate_javascript(
                        "if(document.fullscreenElement)document.exitFullscreen()",
                        |_| {},
                    );
                },
                17 => {
                    let tab = &mut tabs[active];
                    tab.zoom = if event.a == 0 {
                        1.0
                    } else {
                        (tab.zoom * if event.a > 0 { 1.1 } else { 1. / 1.1 }).clamp(0.25, 5.)
                    };
                    tab.view.set_page_zoom(tab.zoom);
                },
                19 => {
                    let character = match event.a {
                        0 => "x",
                        1 => "c",
                        2 => "v",
                        _ => "a",
                    };
                    let mut k = ui::Event {
                        kind: 14,
                        a: 1,
                        b: character.as_bytes()[0] as i32,
                        c: 0x40000,
                        ..Default::default()
                    };
                    tabs[active].view.notify_input_event(key(&k));
                    k.a = 0;
                    tabs[active].view.notify_input_event(key(&k));
                },
                21 => tabs[active].view.load(Url::parse(
                    "https://immersive-web.github.io/webxr-samples/immersive-vr-session.html",
                )?),
                22 => {
                    if let Some(index) = tabs.iter().position(|t| t.state.tab == event.a as usize) {
                        active = index;
                        activate(&tabs, active);
                    }
                },
                _ => {},
            }
        }
        if !running {
            break;
        }
        tabs[active]
            .context
            .make_current()
            .map_err(|e| format!("Graphics context lost: {e:?}"))?;
        wake.store(false, Ordering::Release);
        servo.spin_event_loop();
        if tabs.iter().any(|t| t.state.closed.get()) {
            let current = tabs[active].state.tab;
            tabs.retain(|t| !t.state.closed.get());
            if tabs.is_empty() {
                break;
            }
            active = tabs
                .iter()
                .position(|t| t.state.tab == current)
                .unwrap_or_else(|| active.min(tabs.len() - 1));
            activate(&tabs, active);
        }
        let requests: Vec<_> = tabs.iter().flat_map(|t| t.state.pending.take()).collect();
        for request in requests {
            let ctx = context(software)?;
            let s = state(next_id, permissions.clone());
            next_id += 1;
            let view = request
                .builder(ctx.clone())
                .clipboard_delegate(Rc::new(Clipboard))
                .delegate(s.clone())
                .build();
            tabs.push(Tab {
                view,
                context: ctx,
                state: s,
                zoom: 1.,
            });
            active = tabs.len() - 1;
            activate(&tabs, active);
        }
        if tabs.iter().any(|t| t.state.changed.replace(false)) {
            let tab = &tabs[active];
            let (current, total) = tab.state.history.get();
            unsafe {
                ui::browser_ui_title(
                    ui::c(&tab.state.title.borrow()).as_ptr(),
                    ui::c(&tab.state.url.borrow()).as_ptr(),
                    (current > 0) as i32,
                    (current + 1 < total) as i32,
                );
                ui::browser_ui_tabs(
                    ui::c(&serde_json::to_string(
                        &tabs
                            .iter()
                            .map(|t| t.state.title.borrow().clone())
                            .collect::<Vec<_>>(),
                    )?)
                    .as_ptr(),
                    active as i32,
                );
            }
        }
        if tabs[active].state.dirty.replace(false) {
            let tab = &tabs[active];
            tab.context.make_current().map_err(|e| format!("{e:?}"))?;
            tab.view.paint();
            let size = tab.context.size();
            if let Some(image) =
                tab.context
                    .read_to_image(servo::DeviceIntRect::from_size(euclid::Size2D::new(
                        size.width as i32,
                        size.height as i32,
                    )))
            {
                if let Some(path) = std::env::var_os("DOME_BROWSER_FRAME_CAPTURE") {
                    image.save(path)?;
                }
                unsafe { ui::browser_ui_frame(image.as_ptr(), size.width, size.height) };
                captures += 1;
            }
        }
        if let Some(path) = screenshot.as_ref() {
            if tabs[active].state.loaded.get()
                && captures > 0
                && start.elapsed() > Duration::from_secs(2)
            {
                let result = unsafe { ui::browser_ui_screenshot(ui::c(path).as_ptr()) };
                if result == 0 {
                    return Err("Screenshot failed".into());
                }
                println!("{{\"engine\":\"Servo\",\"toolkit\":\"FLTK\",\"frames\":{captures}}}");
                break;
            }
            if start.elapsed() > Duration::from_secs(60) {
                return Err("Page smoke test timed out".into());
            }
        }
    }
    drop(tabs);
    drop(servo);
    unsafe {
        ui::browser_ui_close();
    }
    Ok(())
}
