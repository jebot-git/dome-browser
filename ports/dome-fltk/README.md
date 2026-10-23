# Dome Browser

An experimental FLTK 1.4.5 browser embedding Servo directly through its Rust
API. The controls are native FLTK widgets. Servo renders HTML, CSS, JavaScript
and WebGL into an offscreen GL surface; the shell copies completed RGBA images
into its page widget. It does not launch servoshell or embed a second browser
window.

The source baseline is Servo `2190554aaa6990416dd92851fcd8240ac63ee750`, fetched
from <https://github.com/servo/servo> on 2026-10-22. Upstream `main` through
`61065f91128` was merged on 2026-10-23, including its WebGL, document state,
JavaScript binding and audio updates. The port is in
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

## Sessions and bookmarks

With no URL argument, startup restores the last tab set from the profile's
`session.json`: tab order, selected tab and each tab's zoom. Changes are saved
at most twice per second and again on normal exit, including closing the final
tab. `--no-restore` starts fresh. An explicit URL starts with that URL instead.
`--smoke-test` neither restores nor saves sessions. Restore reloads the URLs;
back/forward history, scroll position, form contents and XR permissions are not
serialized. Restoring an XR page does not enter immersive mode automatically.

**File → Save Session** (Ctrl+Shift+S) writes a separate `saved-session.json`.
**File → Restore Saved Session** (Ctrl+Shift+R) replaces the open tabs with that
snapshot. Automatic saving does not overwrite this manual snapshot. A corrupt
or unsupported automatic session is preserved and autosaving is disabled for
that run; use `--no-restore` or move the damaged file aside to start fresh.
Session files use atomic replacement, mode 0600, a versioned schema and bounded
input. Profiles are locked against concurrent writers in other session buses.
URLs containing embedded usernames/passwords are not persisted.

**Bookmarks → Bookmark This Page** (Ctrl+D) prompts for a title and section.
**Open Bookmark** (Ctrl+Shift+O) selects a saved URL in a native FLTK menu;
**Remove Bookmark** removes a selected entry. All commands also appear in
DomeD's universal menu. The profile's `bm.txt` is directly editable and uses
Dillo's format, with UTF-8 titles and sections:

```text
:s0: Unclassified
:s1: News
s0 https://servo.org/ Servo
s1 https://news.ycombinator.com/ Hacker News
```

**Import bm.txt** merges a selected Dillo file by section name and URL, retaining
the original file. It never silently modifies `~/.dillo/bm.txt`. Empty imported
sections are retained. Section IDs are normalized when saving, as in Dillo.
Native files are re-read before modifications so manual edits remain visible.
Malformed files produce an error instead of being replaced with an empty list.
The format supports HTTP(S), file URLs and `about:blank`; other URL schemes
are rejected. To export, copy the profile's `bm.txt`.

The profile's `hs.txt` stores recently completed page URLs, one URL per line,
most recent first. It keeps up to 2,000 unique URLs within the profile-file size
limit and excludes `about:blank` and URLs with embedded credentials. There are
no timestamps or form contents in this file. **History → Open History** (Ctrl+H)
uses the native picker, and **Clear History** removes the list and completions
after confirmation. The address bar offers inline completion from this history
while typing, selecting the suggested suffix so continued typing replaces it.
It supports both full URLs and host/path prefixes without a scheme, preserving
the original URL's scheme when the suggestion is accepted. An exact known URL
wins over a longer prefix match. Backspace/Delete remain normal editing keys.
`hs.txt` uses the same private atomic writes as session/bookmark files. Invalid
history files are preserved, and automatic history writes are disabled for that
run until the user explicitly clears history.

## Ad blocking, passwords and mouse input

Network filtering uses `adblock-rust` with bundled EasyList and EasyPrivacy,
including block/allow exceptions, resource types and third-party rules. Trusted
request origins remain available when referrer policy omits the referrer, and
redirect targets are checked before fetching. Privacy menu commands toggle
blocking, allow the current site, reload rules, update the official HTTPS lists,
and show status. Updates download off the UI thread and replace the cache only
after successful validation. Add custom network rules to `adblock.txt` in the
profile; exceptions use `adblock-allowlist.txt`. Cosmetic hiding, scriptlets and
redirect resource substitutions are not implemented.

Bundled filter sources, retrieval metadata and license notices are in
`assets/filters`. EasyList/EasyPrivacy are distributed under their GPL-3.0-or-later
option; adblock-rust is MPL-2.0. These notices are also installed with the browser
documentation.

KWallet support uses GIO D-Bus with `org.kde.kwalletd6`; it does not add a Qt
frontend. Saving requires native confirmation and filling requires selecting an
account. Wallet unlock and access permissions remain under KWallet's control.
Each profile has a separate wallet folder. The Privacy menu can disable wallet
integration or forget the current site's saved accounts. Missing or unavailable
wallets leave manual entry available; there is no plaintext password fallback.
HTTP authentication credentials are scoped by origin and challenge (including
realm); proxy credentials are never saved. Servo's HTTP authentication cache is
kept in memory and is no longer loaded from or written to `auth_cache.json`.

HTML login support uses trusted Servo form hooks, rather than injected scripts.
It handles top-level same-origin forms with one password control and a preceding
username field, including forms whose submit event is handled by JavaScript.
It respects autocomplete restrictions and excludes registration/new-password
forms and frames. Autofill requires user interaction and checks the current
origin again before applying credentials. HTTPS is required, with HTTP loopback
allowed for local applications. Arbitrary script-only login flows and forms with
multiple password fields are outside this initial support.

Middle-click and Shift+Insert paste PRIMARY mouse selection into editable web
controls. Mouse selections publish PRIMARY, while Ctrl+C/Ctrl+V continue to use
the separate CLIPBOARD. Password selections are not exported. The native address
field also accepts PRIMARY paste. Wheel down scrolls down and wheel up scrolls
up, following standard desktop direction.

## Import a local Vivaldi profile

Close Dome Browser, then run from this checkout:

```sh
python3 ports/dome-fltk/migrate-vivaldi.py --passwords
```

The optional password step requires Python PyGObject/GIO, `cryptography`, and
access to the existing KWallet encryption key. `--source` selects a Vivaldi
profile directory; `--target` selects a Dome profile. The defaults are
`~/.config/vivaldi/Default` and Dome's normal profile. Omit `--passwords` to
import only bookmarks and history.

Folder paths become bookmark sections. Trash and unsupported URLs are excluded;
existing Dome bookmarks are retained. The most recent history is merged within
Dome's 2,000-entry limit. A private `migration-backups` directory keeps the
previous profile files, a full eligible Vivaldi history archive, and counts.
The script locks the target profile, snapshots SQLite input without modifying
Vivaldi, validates it, and writes profile files atomically with mode 0600.

Supported Linux v10/v11 passwords go directly from memory to the profile's
encrypted KWallet folder and are verified there. Existing wallet entries are
preserved; rerunning safely skips them. No plaintext password export or password
report is created. Imports retain Dome's exact origin/form-action policy and
skip non-web logins, insecure origins and cross-origin form actions. Importing
does not expand the login forms that Dome's password manager can fill.

Synthetic migration fixtures can be checked without accessing a real wallet:

```sh
python3 ports/dome-fltk/tests/migration.py
```

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

The integration scripts default to `target/debug/dome-browser`. Set
`DOME_BROWSER_BINARY` to test a release, staged or installed binary with the
same fixtures, for example `DOME_BROWSER_BINARY="$PWD/target/release/dome-browser"`.

Additional security/input integration checks use a private session bus, an
in-memory mock KWallet, local HTTP fixtures and independent X11 selection
owners. They never access your real wallet:

```sh
uv run --no-project --with python-xlib --with pillow --with dbus-next \
  python ports/dome-fltk/tests/security.py
```

Results are written to `../artifacts/browser-security-tests`.

On 2026-10-22 the build passed 41 browser integration checks and nine unit
tests covering persistence, filtering, wallet origin policy and XR input. A separate smoke run in the real DomeD Wayland session
loaded and rendered <https://servo.org>, with its native controls and universal
menu. The preserved WPE facade regression also passed. No live DomeD WebXR
socket was available during validation, so none of these results claim a
physical-headset test.

This is a browser prototype. The initial build uses Servo's dummy media backend;
audio/video playback, download management, complete browser settings and a
complete accessibility adapter remain work. Servo's Linux OpenXR backend is
not used; the DomeD bridge is the only immersive device exposed by this port.

## Licensing

The browser port is GPL-3.0-or-later. Its native integration helpers derive
from Penguin Mail's FLTK port and the DomeD suite; their original SPDX notices
are retained. Servo and the small changes to its existing files remain under
their original MPL-2.0 notices. Dependency licenses remain in the upstream
repository and Cargo metadata.

## Saving files and copying images

**File → Save Page** (Ctrl+S) saves the current URL's original response, without
bundling its subresources. Right-click a link for **Save Link**, or an image for
**Save Image** and **Copy Image**. Saves go directly to the Downloads directory
configured by `XDG_DOWNLOAD_DIR` in `$XDG_CONFIG_HOME/user-dirs.dirs` (normally
`~/.config/user-dirs.dirs`), falling back to `~/Downloads`. The setting is parsed
as data, without executing shell code.

Downloads use Servo's network session, including HttpOnly cookies, HTTP
authentication, redirects and TLS validation. Server filenames (including UTF-8
`filename*`) are sanitized; existing files and symlinks are never overwritten.
Failures remove the incomplete file. HTTP errors are reported in the status bar.
Files are created with mode 0600.

Copy Image decodes the original resource and publishes bitmap clipboard data
through FLTK on X11 and Wayland; Copy Image Link remains available separately.
PNG, JPEG, WebP, GIF, BMP and ICO are supported; SVG copying is not yet supported.
The original pixel dimensions are retained. FLTK's copy surface composites
transparency onto white; encoded images over 64 MiB or exceeding the decode
limits are rejected. Saving images keeps their original bytes and transparency.
Supported save schemes are HTTP(S), data, and file when the current page is also
local. Blob URLs and automatic attachment/HTML `download` handling remain engine
limitations; use Save Link for ordinary file links. Saves refetch the URL with
GET, so they do not reproduce a response generated by a submitted POST form.

Run the isolated native integration fixture with:

```sh
python3 ports/dome-fltk/tests/downloads.py
```

## GitHub compatibility

A repository-page investigation on 2026-10-22 reproduced JavaScript errors for
missing `crypto`, `IntersectionObserver` and `document.adoptedStyleSheets`,
followed by failures in GitHub's component runtime and React rendering. The
port now compiles Servo's `webcrypto` feature and enables its implemented
IntersectionObserver and adopted-stylesheet preferences. These are targeted
compatibility settings; other experimental APIs retain their defaults.
The same repository page then rendered its file list with no page-console
errors in the smoke test. The optimized release build, all 11 port unit tests,
and the isolated X11 download/bitmap-clipboard fixture passed.
