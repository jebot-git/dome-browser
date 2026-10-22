# Dome Browser

An experimental FLTK 1.4.5 browser embedding Servo directly through its Rust
API. The controls are native FLTK widgets. Servo renders HTML, CSS, JavaScript
and WebGL into an offscreen GL surface; the shell copies completed RGBA images
into its page widget. It does not launch servoshell or embed a second browser
window.

The source baseline is Servo `2190554aaa6990416dd92851fcd8240ac63ee750`, fetched
from <https://github.com/servo/servo> on 2026-10-22. The port is in
`ports/dome-fltk`; the remaining source changes expose an accelerated offscreen
constructor and carry the requesting document's origin into the WebXR device
interface for consent. A further engine fix delivers text-only notifications
without waiting for a nonexistent image-resource callback. Active XR sessions
are also ended when a document is suspended in history or its window is torn
down, so navigation and tab closure release the device promptly.

## Build and run

Use the patched DomeD FLTK prefix, a recent Rust toolchain, a C++17 compiler,
libclang with its resource headers, and the development files for Fontconfig,
FreeType, Cairo, GIO, X11, Wayland and EGL/OpenGL. The helper uses the local
toolchain from the adjacent canonical `dome-compositor` checkout when present.
It disables Rust debug information and defaults to one job to limit memory.

```sh
python3 ports/dome-fltk/build.py
ports/dome-fltk/run https://servo.org
```

`DOME_FLTK_PREFIX` overrides the toolkit prefix. `SERVO_RUST_BIN` overrides the
directory containing `cargo` and `rustc`. The final link uses Rust's bundled
lld when available. The checked-in Cargo lockfile pins dependencies.

On the current machine libclang 21 was installed without its resource headers.
The Ubuntu `libclang-common-21-dev` package was extracted under
`../_deps/servo-clang`, and the helper sets `BINDGEN_EXTRA_CLANG_ARGS` to that
resource directory. This required no system package changes.

```sh
ports/dome-fltk/run --software https://servo.org
ports/dome-fltk/run --profile /path/to/private/profile https://example.com
ports/dome-fltk/run --smoke-test /tmp/servo.ppm http://127.0.0.1:8000/
ports/dome-fltk/run --xr-probe
```

The default profile is `$XDG_DATA_HOME/dome-browser`, or
`~/.local/share/dome-browser`. Separate profiles still share one browser
instance per session bus. Run under a separate `dbus-run-session` for isolated
tests.

## DomeD integration

| Interface | Implementation |
| --- | --- |
| Universal menu | Exports the real FLTK commands through DBusMenu and associates them with the window through KDE's Wayland appmenu protocol or X11 properties. The local menu collapses only while a DomeD menu host exists. Shortcuts remain active, and losing the host restores the local menu. |
| Appearance | Applies DomeD's palette, accent, BDF family and native strike to controls and native dialogs. Web content receives the current color-scheme preference while retaining its own CSS fonts. The Fontconfig export permits web fonts alongside the native BDF faces. |
| Keybindings | Ctrl+L location, Ctrl+T new tab, Ctrl+W close tab, Ctrl+Q quit, Ctrl+R reload, Alt+Left/Right history, Ctrl+Tab next tab, Ctrl+plus/minus/0 zoom and F11 fullscreen. Page input is forwarded to Servo. |
| Tray | Publishes a StatusNotifierItem with a themed globe pixmap, the page title and the same command menu. Activation raises the browser. |
| Notifications | Uses the session's `org.freedesktop.Notifications` service after native, origin-specific site consent. Returned IDs and notification actions are scoped to the service owner; activation focuses the originating tab. |
| Activation | Exposes `org.freedesktop.Application` on `org.domed.Browser`. Subsequent launches open their URLs in the existing instance. |
| Dialogs | Uses FLTK file, select, color, JavaScript and HTTP authentication dialogs. Unsupported device permissions are denied. |
| WebXR | Uses DomeD protocol v1 to share the compositor's existing XR session, described below. |

## WebXR through DomeD

The port registers a `DiscoveryAPI<SurfmanGL>` backend with Servo. It does not
create an OpenXR instance or use the WiVRn runtime directly. It queries
`$DOME_WEBXR_SOCKET`, or the `dome-webxr-PORT.sock` endpoint in
`XDG_RUNTIME_DIR`. The port comes from the Wayland session record,
`DOME_CONTROL_PORT`, or 49640. Each RPC checks the peer UID and bounds the JSON
response before parsing it.

### Compatibility with the WPE hook

The existing DomeD **host** hook covers this build unchanged. The Linux adapter
in `src/xr.rs` implements the same protocol-v1 `hello`, `begin_session`,
`wait_frame`, `end_frame` and `end_session` exchange as the preserved WPE client.
The compositor still owns the physical OpenXR session and accepts the browser's
RGBA stereo projection frames through its authenticated Unix socket.

The WPE `dome-openxr/runtime.cpp` library is an OpenXR client facade accepting
`XR_MNDX_egl_enable` and GLES graphics bindings. It cannot simply be selected
with `XR_RUNTIME_JSON` for upstream Servo on Linux: Servo's existing OpenXR
device implementation registers only its Windows D3D11 backend. This port
instead rewrites the device side for Linux using Surfman/EGL/OpenGL and the
DomeD socket directly. It does not enable the `openxr-api` Cargo feature,
register D3D11, or require a Windows graphics interop hook. `XR_RUNTIME_JSON`
is not used by this backend.

The preserved WPE facade regression test passed on 2026-10-22: loader
negotiation, EGL/GLES swapchains, rendered stereo frames, pose transforms,
tracking flags, GL state, RGBA orientation, host-loss events and lease exit.
Its source remains at `../../web-candidate/fltk-wpe/dome-openxr` relative to
the Servo checkout; the installed WPE browser has been removed. That test
checks the shared hook's existing contract; the separate browser test below
checks this new Servo client through actual Servo WebGL.

An immersive request retains Servo's secure-context and user-activation checks
and opens a native dialog showing the requesting document's actual origin.
Only an accepted request acquires a browser lease. The initial tracked headset
position and yaw define local space; floor space is available only when the
host advertises a stage. Eye poses, asymmetric FOVs, frame IDs, visibility and
predicted display timing come from the host. There are no fabricated poses in
the device implementation.

Servo's Surfman layer manager renders the WebGL projection layer. The browser
reads its completed texture, reverses OpenGL row order and submits a bounded
RGBA projection image with the host poses and FOVs. Exit, navigation, errors
and device destruction release the browser lease. They never request exit of
DomeD's real XR session.

The current bridge supports immersive VR with one projection layer and an
RGBA transport at `framebufferScaleFactor: 1`. AR, depth layers, hands, controllers, hit testing, bounded
floor and DMA-BUF submission are not advertised. Rendering and transfer
performance need testing on a physical headset. Inline sessions are not yet
implemented by this device adapter. `XRFrame.predictedDisplayTime` currently
exposes the host clock in milliseconds; conversion to the document's performance
timeline remains work. The animation callback timestamp already uses Servo's
document timeline.

## Validation

```sh
python3 ports/dome-fltk/build.py --test
uv run --no-project --with python-xlib --with pillow --with dbus-next \
  python ports/dome-fltk/tests/browser.py
```

The integration test uses the real Servo engine and FLTK controls on a private
Xvfb display and session bus. Its local HTTP page exercises WebGL, page input,
navigation, activation, menu transitions, appearance and notification delivery.
Its isolated DomeD protocol fixture validates XR consent, real WebGL stereo
pixels, reference spaces and lease cleanup. It never contacts a live headset.
Logs and images are written to `../artifacts/servo-browser-tests`.

On 2026-10-22 the build passed 26 browser integration checks and both XR input
validation unit tests. A separate smoke run in the real DomeD Wayland session
loaded and rendered <https://servo.org>, with its native controls and universal
menu. The preserved WPE facade regression also passed. No live DomeD WebXR
socket was available during validation, so none of these results claim a
physical-headset test.

This is a browser prototype. The initial build uses Servo's dummy media backend;
audio/video playback, download management, persistent browser settings and a
complete accessibility adapter remain work. Servo's Linux OpenXR backend is
not used; the DomeD bridge is the only immersive device exposed by this port.

## Licensing

The browser port is GPL-3.0-or-later. Its native integration helpers derive
from Penguin Mail's FLTK port and the DomeD suite; their original SPDX notices
are retained. Servo and the small changes to its existing files remain under
their original MPL-2.0 notices. Dependency licenses remain in the upstream
repository and Cargo metadata.
