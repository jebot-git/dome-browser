# Dome Browser

The latest scoped native input and XR regression results are recorded in
[the October 2026 validation report](docs/validation-2026-10-27.md).
Native reset monitoring and delayed WebGL restoration results are recorded in
[the native graphics validation record](docs/validation-native-reset-2026-10-28.json).
Live scene reconstruction results are recorded in
[the renderer recovery validation record](docs/validation-renderer-recovery-2026-10-28.json).
Reporting history and worker observer lifecycle results are recorded in
[the reporting validation record](docs/validation-reporting-lifecycle-2026-10-28.json).
Typed permissions-policy bodies and WebXR diagnostics are covered by
[the policy reporting validation record](docs/validation-policy-reporting-2026-10-28.json).
Iframe potential diagnostics and load lifecycle checks are recorded in
[the iframe policy validation record](docs/validation-iframe-policy-2026-10-28.json).
WebGL XR compatibility requests are covered by
[the compatibility reporting record](docs/validation-webgl-xr-policy-2026-10-28.json).
Gamepad API-use diagnostics are covered by
[the gamepad policy record](docs/validation-gamepad-policy-2026-10-28.json).

Supported contexts prefer robust access and reset notifications. The FLTK
frontend retries renderer reconstruction after permanent native loss. It replays
compact CPU scene and resource state into a distinct native owner while keeping
live documents, iframe globals, forms, selections, scroll positions and zoom.
WebGL producers receive context loss and may restore fresh resource generations.
Old renderer notifications cannot complete frames belonging to the replacement.

Validation covers controlled retirement and repeated reconstruction on native
and software graphics for WebGL 1 and 2, including visible canvas, image, system
font and downloaded webfont pixels. The opt-in test embedder captures each
rendered frame before presentation exchanges its offscreen buffers. No physical
GPU reset was requested. Opaque blob and snapshot resources defer reconstruction
until their producers release them; the current Dome frontend does not emit
those resource types. Optional native-window backends and physical devices
require further validation.

An experimental FLTK 1.4.5 browser embedding Servo directly through its Rust
API. The controls are native FLTK widgets. Servo renders HTML, CSS, JavaScript
and WebGL into an offscreen GL surface; the shell copies completed RGBA images
into its page widget. It does not launch servoshell or embed a second browser
window.

The source baseline is Servo `2190554aaa6990416dd92851fcd8240ac63ee750`, fetched
from <https://github.com/servo/servo> on 2026-10-22. Upstream `main` through
`61065f91128` was merged on 2026-10-23, including its WebGL, document state,
JavaScript binding and audio updates. The port is in
`ports/dome-fltk`. Engine changes include an accelerated offscreen constructor,
live native-renderer reconstruction, and the requesting document's origin in the
WebXR device interface for consent. A further engine fix delivers text-only notifications
without waiting for a nonexistent image-resource callback. Active XR sessions
are also ended when a document is suspended in history or its window is torn
down, so navigation and tab closure release the device promptly.

## Build and run

Use the patched DomeD FLTK prefix, a recent Rust toolchain, a C++17 compiler,
libclang with its resource headers, and the development files for Fontconfig,
FreeType, Cairo, GIO, X11, Wayland, EGL/OpenGL and GStreamer (including
the base/bad development libraries for app, video, play and WebRTC). Runtime
GStreamer plugins determine supported media. Unsupported media types are handed
to an external player; expanding browser media playback is outside the current
work. The helper uses the local
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
uses a native picker with 20 entries per page, and **Clear History** removes the list and completions
after confirmation. The address bar offers inline completion from this history
while typing, selecting the suggested suffix so continued typing replaces it.
It supports both full URLs and host/path prefixes without a scheme, preserving
the original URL's scheme when the suggestion is accepted. An exact known URL
wins over a longer prefix match. Backspace/Delete remain normal editing keys.
`hs.txt` uses the same private atomic writes as session/bookmark files. Invalid
history files are preserved, and automatic history writes are disabled for that
run until the user explicitly clears history.

History and tab captions are shortened for display at Unicode boundaries. Each
entry retains its own selection index even when shortened captions look alike;
navigation and saved history keep the complete URL. Native window titles are
also bounded so long page titles remain usable. `tests/history-popup.py` checks
repeated opening, paging, selection and clean shutdown with a private 2,000-entry
history containing long URLs and a long Unicode page title.

## PDF files

Top-level PDF navigations save the completed response into the user's Downloads
folder and open the system's PDF viewer. DomeD's `application/pdf` default takes
precedence over the standard desktop MIME default. Both file and URI desktop
handlers are supported. The browser passes the original response bytes, including
authenticated requests, POST results and redirects, without downloading again.
Incomplete or cancelled transfers remove their partial files and never launch
a viewer. Saved files have private permissions and a `.pdf` filename. If a viewer
is unavailable, the complete file remains saved and the status bar reports it.
Embedded PDFs do not launch external applications automatically; an HTML page
whose URL ends in `.pdf` remains a normal page.

`tests/pdf-handoff.py` uses private desktop handlers to check these behaviors,
including UTF-8 filenames, local files, compressed responses and interrupted
transfers. It does not change the user's application defaults.

## Mail and torrent links

`mailto:`, `misfin://MAILBOX@HOST`, and `magnet:` links open the selected mail
or torrent application. Misfin uses its own configured application; it does
not fall back to an ordinary email handler. An enabled Penguin Mail build
opens its existing Misfin composer and leaves sending to the user.
The browser reads DomeD's `dome-mimeapps.list` overrides for
`x-scheme-handler/mailto`, `x-scheme-handler/misfin`, and
`x-scheme-handler/magnet`, then uses the standard
desktop defaults when no DomeD override exists. An unavailable explicit override
reports an error. The browser does not choose or install a torrent/mail client,
change defaults, send mail or download torrents itself.

Address-bar input and explicit command-line URIs request handoff directly. Page
links require native confirmation before an application opens; declining keeps
the page open. Handoffs preserve the complete mail fields or magnet parameters
and inherit the current desktop/Wayland environment. Generated smallnet document
links support these schemes too. Native address input exceeding its existing
8 KiB event limit is rejected rather than dispatching a truncated URI.
`tests/external-links.py` verifies dispatch using private fixture applications
on X11 and, with `--wayland`, a private nested DomeD compositor.

## Ad blocking, passwords and mouse input

Ad blocking is both cosmetic and privacy-focused, using one shared `adblock-rust`
engine for network decisions and native cosmetic selectors. Network filtering
uses bundled EasyList and EasyPrivacy,
including block/allow exceptions, resource types and third-party rules. Trusted
request origins remain available when referrer policy omits the referrer, and
redirect targets are checked before fetching. Privacy menu commands toggle
blocking, allow the current site, reload rules, update the official HTTPS lists,
and show status. Updates download off the UI thread and replace the cache only
after successful validation. Add custom network rules to `adblock.txt` in the
profile; exceptions use `adblock-allowlist.txt` and apply only to the known
top-level origin. An exempt iframe origin cannot turn off filtering on a
different site. The network request supplies this context independently of
referrers and UI history updates; early loads cannot inherit the previous page's
exception. Opaque or cross-site ancestry is withheld, so those frames and requests
without a known top-level context remain filtered even on an exempt page.
`tests/adblock-exceptions.py` exercises iframe isolation, top-level exceptions,
and restoring filtering when leaving an exempt site. Rules with literal `$removeparam=name` options remove nonempty named query
values through adblock-rust before HTTP transport. For example,
`/privacy$removeparam=utm_source` applies to matching paths. Retained fields keep
their raw bytes and order, and fragments are preserved. The native operation
accepts only query-field removal within the existing HTTP(S) URL: changes to
scheme, authority, credentials, path or fragment cancel the request. Fetch
continues with its original method, body, credentials and security policy.
WebSocket transport queries stay synchronized with Fetch's HTTP(S) URL, including
HSTS upgrades. Preload reuse checks the effective requested URL after rewriting.
The engine's default modifier scope covers documents, subdocuments and fetch/XHR;
use an explicit resource type or `$all,removeparam=name` for other transports.
`tests/adblock-query-removal.py` covers fetch, POST bodies, cookies, CORS and
preflight/denial controls, scripts/images/workers/beacons/WebSocket, redirects,
ordinary/opaque preloads, navigation fragments, site exceptions and an explicit
user-disabled control.

Native request metadata
distinguishes Beacon (`$ping`) and WebSocket (`$websocket`) traffic from fetch/XHR;
keepalive fetches remain XHR. WebSocket rules see the WS(S) equivalent of the
actual HTTP(S) handshake URL, retaining HSTS upgrades.
`tests/adblock-transports.py` checks that blocked requests never reach the local
server, each resource-type rule preserves the other transports, and cosmetic
hiding remains active in the same engine.

Cosmetic CSS filters use adblock-rust's hostname resources, generic class/ID
queries, cosmetic exceptions and `$generichide` exceptions. Servo discovers
connected elements and class/ID changes in native DOM hooks, batches queries, and
applies validated selectors as private user-origin `display:none!important`
styles. Page CSP and author-important declarations cannot override these styles;
they create no DOM nodes or entries in `document.styleSheets` and require no
page JavaScript. Historical token-cache eviction causes repeat queries rather
than dropping new rules. Same-document URL changes and native policy/list changes
refresh existing content; inactive documents refresh on reactivation. Site
exceptions use trusted document ancestry, not the displayed tab URL. Connected
author shadow trees, including closed and nested roots, participate in generic
class/ID discovery and policy resets; UA widget internals are excluded. Local
`blob:` and `data:` documents also participate. Blob documents with a trusted
HTTP(S) document origin use that immutable origin for hostname rules; opaque
data and sandboxed blob documents receive generic rules only. Neither an author
base URL nor an HTTP address embedded in a sandboxed blob URL selects exceptions.
`tests/adblock-blob-data.py` checks these boundaries and explicit disabled controls;
`tests/adblock-inherited-first-paint.py --local-url blob` (or `data`, with optional
`--opaque`) checks actual first-paint pixels.
`tests/adblock-shadow.py` covers these roots, mutations, preassembled-host
insertion, generic-hide URL changes, encapsulation and rendered pixels.

`tests/adblock-cosmetic.py` covers hiding, legitimate content, dynamic insertion,
class changes, hostname exceptions, generic-hide URL exceptions, history changes,
live menu controls/list reloads, no-referrer cross-site frames and CSP. Its
script-disabled HTML/SVG fixtures verify rendered pixels. SVG navigation now uses
an XML SVG document rather than an HTML image wrapper: all `+xml` MIME subtypes
select XML processing, while SVG remains eligible for image preloads. The fixture
also checks the SVG root namespace/content type and embedded XHTML scripts under
both denied and nonce-allowed CSP policies. This does not establish complete SVG
scripting conformance. Procedural actions,
scriptlets and redirect substitutions remain follow-up
work. Local `about:blank` and `srcdoc` frames participate in cosmetic
discovery. Hostname rules use their immutable HTTP(S) security origin; opaque
frames receive generic rules. The real document URL remains the lifecycle check
for asynchronous replies. `tests/adblock-inherited-frames.py` covers these
contexts, legitimate content and an author-controlled base URL pointing at a
host with a hiding exception.

Pending native cosmetic queries block display updates until their CSS has
been installed, while layout queries remain available.
`tests/adblock-first-paint.py` samples actual pixels across script-disabled,
chunked document loads, with an explicitly disabled-blocker negative control.
`tests/adblock-shadow-first-paint.py` repeats this pixel capture for newly
populated closed shadow roots.
`tests/adblock-progress.py` checks progress under large DOM/class churn; this
workload still needs performance optimization.

`tests/adblock-session.py --binary /path/to/browser --output /private/output`
runs a five-minute private session with a bounded 4,000-node document, changing
classes, open/closed shadow roots, child-document teardown, blocked fetch/beacon
probes and legitimate network controls. `--disable-adblock` supplies a negative
control; `--default-lists` adds bundled EasyList/EasyPrivacy to the explicit fixture
rules. The latter uses 127.0.0.2 because EasyList intentionally exempts 127.0.0.1
from generic hiding. Both five-minute controlled runs passed 98 checks. Memory
continued to grow with filtering enabled and disabled, so this is evidence of
sustained functionality rather than a long-session memory-stability claim.

Fifteen-minute enabled/default-list and disabled controls each pass 278 checks
and close cleanly. The enabled run blocks both privacy probes while delivering
all 447 legitimate requests; the disabled run delivers every probe. RSS grows
about 279 MiB and 271 MiB respectively. Snapshots show automatic GC reclaiming
detached DOM objects and allocated heap, followed by rising peaks. This does not
establish bounded memory use or an adblock-specific leak. Diagnostic flags and
concurrent workloads differ, so frame times are not an isolated comparison.
See `../artifacts/servo-rendering-goal/memory-attribution-v2-summary.json` and
`../artifacts/servo-rendering-goal/memory-attribution-v3-control-summary.json`.

For local attribution, `DOME_BROWSER_MEMORY_CAPTURE=/private/memory.json` requests
aggregated engine memory categories and DOM interface counts. Optional `_DELAY_MS`,
`_INTERVAL_MS` and `_COUNT` suffixes configure timing (up to 24 snapshots).
Document URLs are removed; mapped library and font paths remain. Reports use the
private atomic storage writer. This diagnostic is disabled unless a capture path
is supplied and does not force garbage collection or expose internal APIs to
web content. See `../artifacts/servo-rendering-goal/memory-diagnostics-v1-validation.json`.

`DOME_BROWSER_GC_PROFILE=1` enables the existing native GC profiler for local
attribution. `tests/dom-lifetime.py` exercises 524,288 detached/connected span
allocations and replacements; its reduced workload passes and shows normal GC.
Neither diagnostic forces collection or changes the normal GC configuration.

Standalone SVG titles now expose `SVGTitleElement` and update native tab/window
titles after parser completion, text edits, removal, root replacement and atomic
moves. `tests/svg-title.py` checks both SVG and HTML title semantics. Idle callbacks
snapshot the runnable list for each period, defer recursive registrations until
the next period and ignore zero timeout values as specified;
`tests/idle-period-boundaries.py` covers cancellation and period boundaries.
The native rich-text input fixture waits for the actual control pixels before
its first click; an initial blank frame capture is insufficient readiness.

Non-replaced CSS `aspect-ratio` participates in normal block flow, floats and
inline blocks, flex sizing and positioned layout. Preferred sizes and min/max
constraints transfer through the ratio independently of intrinsic content
measurements. Ratio-derived heights provide percentage containing blocks;
automatic content minimums respect definite maximums, while explicit minimums
retain precedence. Border-box constraints are floored before transfer.
`tests/aspect-ratio.py` covers 39 sizing and mutation checks without media
decoding. The five initially failing aspect-ratio WPT groups now pass; the
expanded 17-group run passes 16 groups. The broken-image/alternate-text case
now passes, while two scrolling assertions disagree with the
hidden/clip overflow normalization observed in both Dome and Chromium. Original
WPT expectations remain unchanged. Broader table, margin and orthogonal-writing
coverage is still needed; this is not a claim of complete CSS sizing conformance.

Failed images with nonempty `alt` text, and images that have never had a source,
generate native non-replaced text without adding author-visible DOM nodes.
Empty alternate text gives a failed image zero natural size. Valid and pending
images, CSS image replacement and dimensioned quirks-mode fallback retain image
boxes. Alternate-text mutations and quirks dimension changes reconstruct the
appropriate box. `tests/image-fallback.py` covers 47 standards-mode checks;
`--quirks` runs eight checks in a separate quirks-mode document. Native frame
captures verify that fallback text actually paints. Dimensioned quirks-mode
fallback boxes contain native shaped text with wrapping, RTL alignment and a
content-box clip. Intrinsic sizing uses the text's min/max contributions and
wrapped block size. Layout damage reconstructs this internal text context so
style changes and newly available fonts refresh its measurements and paint.
`tests/image-fallback-boxed.py` covers 46 geometry/state checks and 23 pixel
checks, including clipping, CSS replacement and late CSS font loading, without
adding DOM children. The optional broken-image icon is omitted for text boxes;
reference-browser intrinsic sizes include different icon/spacing behavior.
Vertical writing remains disabled by the engine's default preference; these
tests record that capability without claiming vertical-text support.

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
| Tray | The browser does not register a tray item. Use its window/task entry and universal menu. |
| Notifications | Uses the session's `org.freedesktop.Notifications` service after native, origin-specific site consent. Returned IDs and notification actions are scoped to the service owner; activation focuses the originating tab. |
| Activation | Exposes `org.freedesktop.Application` on `org.domed.Browser`. Subsequent launches open their URLs in the existing instance. An unresponsive existing instance produces an activation error. |
| Dialogs | Uses FLTK file, select, color, JavaScript and HTTP authentication dialogs. Unsupported device permissions are denied. |
| WebXR | Sensor-free inline canvas sessions; immersive mode shares DomeD's existing XR session through protocol v1, described below. |

## Web editing compatibility

Contenteditable keyboard editing is enabled for all sites. Servo's text
insertion path now supplies the inserted text in `beforeinput` and `input`,
allowing controlled rich-text editors to update their own document models.
Native `keypress` events also expose character values through `charCode` and
`which`. These changes apply to the engine, including pages outside Discord.
They do not implement full IME integration or all contenteditable navigation.

## Embedded HTML in SVG

Inline SVG `foreignObject` content participates in native CSS layout, painting,
hit testing, DOM geometry, focus and input on every site. SVG viewBox scaling,
alignment, numeric/percentage dimension attributes and SVG transform attributes
on `g`, `a` and `foreignObject` are supported. Descendant SVG mutations invalidate
the enclosing viewport's layout and image caches. Embedded controls receive trusted
events through the engine; no script forwards or fabricates clicks.

This remains partial SVG support: other SVG graphics are rasterized behind
the HTML islands. Nested SVG viewports, `use` instances, SVG masks and full
vector-shape DOM hit testing are not implemented by this path.

## Experimental Discord compatibility

IndexedDB is enabled, with persistent storage inside the browser profile.
Native `requestIdleCallback`, `cancelIdleCallback` and `IdleDeadline` are exposed
on all Window origins. Idle work runs after ready tasks drain and yields between
callbacks; pending timers and rendering shorten the deadline. Timeouts race idle
execution, canceled callbacks are removed, inactive documents are skipped, and
windows sharing an event loop take turns. The Discord timer fallback is no
longer injected.

The optimized `native-idle-candidate` passes 56 focused WPT assertions: 27 in
17 HTML fixtures and 29 WebIDL checks. The HTML adapter injects only its completion
reporter and preserves the original test logic. The Chromium reference passes
54 of these assertions; its dynamic timer and animation-frame deadline failures
are retained as reference differences. The browser integration fixture also
checks native idle scheduling alongside Cache API and IndexedDB persistence.
The broader matrix exposed an input wakeup regression: consuming a rendering
timer before waiting could leave queued input unprocessed. The corrected loop
preserves the pending rendering checkpoint. Native browser clicks, PRIMARY paste
and rich-text input pass again on the validated `cache-batch-errors-candidate`.
Results are in `../artifacts/servo-rendering-goal/native-idle-validation.json`.

Dome enables native CacheStorage and Cache methods, backed by per-origin
SQLite storage in the browser profile. Named caches, request metadata and binary
response bodies persist across restarts. Writes consume real Response streams;
`addAll()` stages one atomic batch and uses ordinary Fetch policy checks. Opaque
responses retain their filtered public view and private cached body. Discord's
empty-cache script fallback has been removed from source.

Native WASM streaming reads the Response body through its stream reader,
including buffered, constructed, cloned and cached responses. The optimized
Cache/WASM lifecycle candidate passes 37 Cache checks (including restart
persistence), 17 WASM checks, and unchanged WPT subsets in windows and dedicated
workers: 64 Cache and 131 WASM cases in each context. Cache names preserve exact
UTF-16 code units. Source stream errors retain their original rejection value;
MIME parameters are rejected. Both idle and pending-compilation workers terminate
before browser close, and accepted SpiderMonkey async tasks finish during shutdown.

The candidate also passes 46 browser, 17 security, 18 native Wayland/media checks,
rich-text/SVG input and three SharedWorker shutdown cases. Its immutable binary
and validation record are in `../artifacts/servo-rendering-goal/` as
`cache-lifecycle-v2-candidate` and `cache-lifecycle-v2-validation.json`.
These focused embedding runs do not replace the full WPT runner.

The optimized `coep-candidate` additionally passes 36 native COEP checks for
response-header parsing, document/worker inheritance and credentialless
network/cache enforcement, plus three non-secure-context controls. The latter
use a temporary profile with content filtering disabled because EasyPrivacy
blocks the fixture's `0.0.0.0` origin; other regression runs retain filtering.
Cache37/WASM17 and the focused 390-case WPT matrix also pass on this candidate.
Reporting delivery, broader isolation coverage and full WPT coverage remain.

The public Discord login form renders with these fallbacks (tested 2026-10-23)
and the browser exits cleanly afterward. The user also confirmed successful
login and viewing direct messages. Server switching and editor interaction
fixes are tested with isolated native input fixtures; they still need validation
in an authenticated Discord session. Microphone/camera permission
support is not implemented in this port, so voice/video calls are unsupported;
screen sharing is also outside this compatibility experiment.

A subsequent check using disposable storage copies from an inactive profile
renders Discord's authenticated Friends view and X's authenticated Home timeline.
Both stay alive for 30 seconds and close normally. Discord reports no page-console
error; X reports an account-settings API HTTP 404 rejection, including with content
filtering disabled. These checks perform no submissions and leave the source
profile unchanged. Discord's black icons were reduced to separate SVG raster
failures with modern colors and CSS variables in presentation attributes;
the RGB paint controls pass. Those rendering fixes remain in progress.

```sh
python3 ports/dome-fltk/tests/compatibility.py
uv run --no-project --with python-xlib python ports/dome-fltk/tests/rich-text.py
uv run --no-project --with python-xlib python ports/dome-fltk/tests/rich-text.py --compat
uv run --no-project --with python-xlib python ports/dome-fltk/tests/rich-text.py --native-svg
```

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

The bridge supports immersive VR with an active projection layer and an RGBA
transport with scaled WebGL framebuffers. Scale requests are clamped to keep
all eye viewports non-empty and within device/GL allocation limits. The
reported dimensions, viewports and native submission share the same per-layer
geometry. See [WebXR framebuffer validation](docs/XR_FRAMEBUFFERS.md). Multiple
projection layers may be allocated while `updateRenderState` chooses the active
base layer. AR, depth
layers, hands, hit testing, bounded floor and DMA-BUF submission are not
advertised. The installed release has no controller stream. The development
adapter opts into the host's versioned controller extension only when the host
advertises it; older hosts retain their stereo-only behavior. Rendering and transfer
performance need testing on a physical headset. Inline sessions are not yet
implemented by this device adapter. The development adapter carries the host's
converted monotonic instant through Servo and converts it at the requesting
window's performance timeline before exposing `XRFrame.predictedDisplayTime`.
Animation callbacks sample their document time when the task runs. Older hosts
without clock conversion retain an elapsed-time estimate anchored to the first
received frame; the runtime epoch is never used directly as a DOM timestamp.

A subsequent user-run test on `svg-color-v1-candidate` confirms that immersive
rendering works in the tested DomeD scope. Future WebXR work starts from that
working baseline. This does not establish broader API, device or runtime coverage;
the isolated browser integration fixture continues to test protocol/WebGL behavior.

The controller development path carries immutable per-source Gamepad layouts,
independent button touch states, aim/grip poses in the same local anchor as the
headset, and queued select/squeeze transitions. Focus loss, tracking loss,
disconnection, descriptor replacement and history overflow cancel gestures.
Held controls must be released before a subsequent gesture can start. Trusted
requesting-webview identity and native FLTK page focus prevent background tabs
and browser chrome from receiving page controller input. DomeD separately gates
input on native focus, modal UI, realm, lock state and fresh policy publication.

Controller changes remain uninstalled while validation is in progress. The
native sampler now orders primary/squeeze changes by OpenXR `last_change_time`
and locates controller, head and eye poses at each action time. The host publishes
those samples and the newer display frame atomically. `monotonic_time` comes
from `XR_KHR_convert_timespec_time` when the runtime provides it. Missing action
timestamps and deactivated bindings cancel/rearm gestures. New clients establish
their initial baseline from a fresh post-consent tracking frame. Untracked poses
produce null poses and empty submissions while the browser lease remains live.
The timing and controller-lifecycle changes pass private optimized validation;
physical controller bindings and live runtime conversion still require WiVRn-Pro validation. Queue and
synthetic-input tests do not establish physical-device conformance. The private `tests/webxr-input.py` fixture uses the real Servo
DOM and DomeD native transport with a synthetic input feed. It creates no
OpenXR runtime session and does not touch the active desktop or headset.

The clock and event-pose contract follows the [WebXR Device API](https://www.w3.org/TR/webxr/)
and the runtime's [OpenXR time conversion](https://registry.khronos.org/OpenXR/specs/1.1/man/html/xrConvertTimeToTimespecTimeKHR.html).

The development adapter also registers a sensor-free inline device without a
headset connection, consent dialog or native XR lease. Its monoscopic viewer
uses the page canvas's default framebuffer and exposes no tracked input sources.
Projection uses the current drawing-buffer aspect ratio, field of view and clip
planes, including after canvas resizing. Field-of-view endpoints are clamped to
a finite perspective. Viewer view arrays are frozen. Inline animation callbacks
pause during an immersive session and resume after it ends; independent inline
sessions can end without stopping their siblings.

`tests/webxr-inline.py BINARY NATIVE_FIXTURE OUTPUT --no-host` checks the canvas
path without a headset endpoint. Omit `--no-host` to also check immersive
coexistence against the private synthetic native fixture. These checks include
actual projected triangle pixels, not only matrix values. The separate
`tests/webxr-transform-wpt.py --binary BINARY --output OUTPUT` runs unchanged
assertions from four upstream transform suites and one inline field-of-view
suite with real inline setup in place of fake-device setup. This is an adapted
subset of WPT.

Tab hide/show now propagates through shared WebView state to active documents
and child frames, including children created while the tab is hidden. Inert
documents created with DOMParser or createHTMLDocument start hidden. Inline
sessions mirror document visibility and dispatch one XRSessionEvent for each
transition. Hidden XR sessions pause animation callbacks while device event
processing stays responsive to shutdown. Switching to another tab hides the
original immersive session; address-bar focus leaves the document visible.
`tests/webxr-inline.py BINARY NATIVE_FIXTURE OUTPUT --page-lifecycle` exercises
these transitions, child-frame callback suspension, navigation and 32 repeated
inline create/end cycles in a background tab. The private regression observed
the same thread count before and after those cycles; this is a scoped diagnostic.

The staged native visibility milestone integrates compositor suspension and
FLTK visibility reporting, with a fallback for the installed older toolkit.
Initial visibility-state performance entries and retained performance timelines
also pass scoped regression validation. Native inline mouse input is validated
for canvas ownership, content-space rays, transient action ordering and lifecycle
cancellation (34 checks without a headset and 38 with a private immersive host).

Native touch/pen and multitouch engine integration passes 43 private checks,
including stable primary identity, auxiliary contacts, canvas ownership,
iframe routing, no-hit cancellation, capture cleanup, hidden pose denial and
32 repeated gestures. The test-only
`examples/xr-native-input.rs` embedder forwards trusted events through the public
`WebView::notify_input_event` API; `tests/webxr-touch.py` drives its stdin protocol
in an isolated profile and compositor. This input-control interface is not part
of the installed browser. These tests do not establish physical-device conformance.

The staged DomeD compositor and FLTK toolkit now deliver native Wayland touch
through an optional raw-contact callback into the browser. Contacts keep their
original WebView and primary identity, including when the first finger starts in
browser chrome. Focus/tab changes and window minimization cancel page contacts;
late releases cannot select in a replacement view. Unclaimed primary contacts
retain native FLTK widget interaction. Older toolkits continue to launch without
the optional callback, with native touch unavailable.

`tests/native-touch-toolkit.py` validates the actual Wayland protocol path at
100% and 150% toolkit scaling, including frame batching, fractional coordinates,
capability removal, callback replacement and cancellation without button
activation. `tests/native-touch-browser.py` exercises that path through the real
browser into DOM and inline WebXR. Its compositor input fixture is compiled only
with DomeD's `input-test-fixture` feature and accepts stdin commands only in a
nested test compositor; the normal compositor build excludes it. These isolated
software-rendered tests do not establish touchscreen hardware interoperability.

The staged native stylus path includes the browser, FLTK and DomeD. DomeD routes tablet
proximity, tip, barrel buttons and supported axes through its Wayland tablet
manager. Pressed tools keep their original surface; window visibility and device
removal terminate that ownership. `tests/native-tablet-host.py` uses the separate
`native-tablet-probe.cpp` client to inspect actual protocol frames on a native
FLTK surface. The staged toolkit now also provides an optional frame-batched
pen callback for hover, contact, barrel buttons, erasers and supported axes.
Its IDs are local to the process. Native primary-tip widgets remain usable
without a raw callback; cancellation cannot activate a button or transfer a held
contact into a replacement handler. `tests/native-pen-toolkit.py` and
`native-pen-probe.cpp` cover those transitions at 100% and 150% scaling,
including native surface recreation and pens without pressure sensors.
The browser's optional pen callback delivers trusted pen PointerEvents with
pressure, orientation, hover, barrel/eraser buttons and capture, alongside
independent finger and mouse input. The staged native pen suite covers DOM,
compatibility mouse events, editing and inline WebXR lifecycles. See
[native pen routing](docs/PEN_ROUTING.md) for cross-frame capture, pointer
lifetime, bounded input queues and their isolated validation fixtures.

The staged browser/runtime pair transports cumulative native reference-space
origin state. OpenXR changes are applied at their scheduled display time; a
browser that skips tracking frames still receives the total change, and unknown
continuity produces a null reset transform. Frame-scoped reset events reach base
and offset spaces before animation callbacks using the new origin. Reset handlers
can create spaces or end a session. Spaces with reset listeners are retained;
other offset spaces are tracked weakly using their concrete DOM type.

Bounded offset spaces retain their interface and transform bounds through the
inverse origin offset. The headless test backend updates floor and bounds state
per session and preserves viewport events when merging frame updates. Private
validation uses `tests/webxr-reset.py`, `tests/webxr-bounded.py` and the two
unchanged upstream reset suites through `tests/webxr-reset-wpt.py`. The latter
runs the original setup and assertions on loopback HTTP with the separate mock
embedder, rather than the full WPT infrastructure. The event contracts follow the
[WebXR Device API](https://www.w3.org/TR/webxr/) and
[OpenXR reference-space change event](https://registry.khronos.org/OpenXR/specs/1.1/man/html/XrEventDataReferenceSpaceChangePending.html).

Broader WebXR feature/conformance coverage, auxiliary default actions, DomeD
shell touch interaction and physical touchscreen/tablet/controller/runtime
validation of the staged revisions remain follow-up work.

Authorized active documents can observe immersive-device availability through
`navigator.xr.ondevicechange`. Native observation stops when no listener or
compatible/pending WebGL context remains, its document becomes inactive, or its
browsing context is destroyed.
It uses capability handshakes without entering XR or reading tracking data.
DomeD runtime suspension and endpoint replacement are covered by the private
bridge fixture. See [device-change behavior and validation](docs/WEBXR_DEVICECHANGE.md).
Support queries, immersive requests and context compatibility share one selected
device, preserving it during active sessions. Device replacement clears context
compatibility without losing ordinary graphics resources; retained history
documents reconcile their contexts on resume. See
[device selection and compatibility](docs/WEBXR_DEVICE_SELECTION.md).
Deferred session requests are owned by their document activation. Removing or
navigating that document cancels unadopted backend sessions; stale history
responses cannot replace a fresh request. See
[XR request lifetime](docs/WEBXR_REQUEST_LIFETIME.md).

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

This is a browser prototype. Media uses Servo's GStreamer backend. Complete
browser settings and a complete accessibility adapter remain work. Servo's Linux OpenXR backend is
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

## Resizing, dragging and image validation

The FLTK shell provides a hover handle for textareas while Servo lacks native
CSS resize handles. It supports inline resize direction/none declarations and
computed size limits; it is not a complete implementation of stylesheet CSS
resize. Native cursors now reflect page resize and move cursors. The engine's
box hit regions also cover visible non-scrollable boxes so a dialog painted
above an iframe receives mouse input instead of the underlying iframe.

The isolated native Wayland rendering regression uses a nested DomeD session,
synthetic input and generated images. It checks textarea resizing, dragging a
fixed panel above a cross-origin iframe before/after scrolling, iframe
input/scrolling, static/animated WebP pixels, screen geometry and
stylesheet-colored inline SVG. Before dragging a resize handle, the test waits
for its painted pixels. Native video playback remains outside the current scope:

```sh
uv run --no-project --with python-xlib --with pillow \
  python ports/dome-fltk/tests/interactions-rendering.py
```

It needs ffmpeg with libvpx/libx264/AAC, Xvfb, wl-copy and the adjacent DomeD
release compositor (override with `DOME_COMPOSITOR`). Results are in `../artifacts/browser-interactions-media`.
Live checks on 2026-10-23 confirmed dragging and resizing the 4chan Quick Reply
panel; no post was submitted. They also rendered X's public landing/login
controls without logging in. Bing image-result thumbnails rendered, but opening a larger preview
failed with a Bing React component error (#130); this remains a site compatibility
issue rather than a reproduced WebP decoder failure.

The shell supplies Servo's native screen geometry callback, including the current
monitor size/work area and outer window bounds. Without it, the Screen API
returned zero dimensions and X selected a layout without its desktop sidebar.
Inline SVG serialization now preserves computed CSS paint on the cloned SVG,
including inherited color/currentColor, so icons can retain page colors when
rasterized independently of the page stylesheets.

Progressive MP4/WebM playback is supported with appropriate GStreamer plugins.
HLS is not advertised merely because an adaptive demuxer is installed: Servo's
byte-fed `servosrc://` cannot resolve the playlist's resources, and bypassing the
browser network session would also bypass its request policy. Media Source
Extensions are not implemented by this Servo baseline. On 2026-10-23 an isolated
copy of the saved X session confirmed sidebar labels/icons and Explore navigation
after these fixes. All 18 interaction/media/SVG, 41 browser and 17 security checks
passed on the release build;
X's embedded player still reports an unsupported browser without HLS/MSE, even
though progressive H.264/AAC playback passes the local fixture.

That player result describes the earlier milestone. A fresh authenticated probe
on 2026-10-24 selects a progressive MP4 from X's media origin, reports valid
dimensions and a fully buffered duration, and renders its video image. MediaSource
and HLS support remain absent; their absence alone does not explain every playback
issue. Playback advancement and looping are being checked separately.

## Servo rendering and shutdown regressions

SVG `<use>` snapshots honor modern `href` precedence over legacy `xlink:href`,
including an empty modern attribute, and resolve absolute same-document URLs and
percent-encoded fragments. Imported resources live in private `<defs>` instead
of painting independently; duplicate/circular imports are bounded. The optimized
candidate fixes six reproduced failures: `tests/svg-use.py --cycles` passes
15/16 pixel checks, with source-stylesheet paint still failing. Its assertion
remains failing rather than treating that known defect as a pass. Chromium
passes all 16. `tests/svg-use-inheritance.py` improves from 0/8 to 3/8 in Dome;
Chromium passes 8/8. Remaining cases include stylesheet paint, custom properties
and the SVG `color` presentation attribute in that candidate. The optional base-URL probe has a
reference discrepancy and does not establish standards compliance.

The candidate passes 33 general regression jobs, 30 focused adblock jobs, five
privacy suites, 13 native Wayland checks and 20 frontend tests. Anonymous Discord
and X samples survive 30 seconds and close normally; X's QR/cookie overlap remains
unclassified. See `../artifacts/servo-rendering-goal/svg-use-v1-validation.json`.

The subsequent `svg-color-v1-candidate` implements the SVG `color` presentation
attribute through the native CSS cascade and attribute invalidation.
`tests/svg-color.py` passes all 15 DOM and 12 pixel checks, matching Chromium;
the previous candidate failed six DOM and four pixel checks. Coverage includes
root/group inheritance, stylesheet and inline overrides, CSS-wide keywords,
custom properties, invalid values, namespaces, mutation and removal. The use
inheritance fixture improves to 4/8, while its four stylesheet/custom-property
failures remain. Ten SVG suites, public Discord/X survival and closure, browser,
security, Wayland/input, rich text, two first-paint adblock gates and 20 frontend
tests pass. This targeted rerun does not replace the earlier full regression
matrix with a broader conformance claim. See
`../artifacts/servo-rendering-goal/svg-color-v1-validation.json`.

The later `svg-instance-v3-candidate` cascades each detached use instance through
the native style resolver, using its host's inherited values and its own selector
ancestry. Paint URLs retain the defining document's resource context; imported
gradients, patterns and clipping resources are definitions rather than separately
painted content. Connected SVG and stylesheet mutations trigger batched SVG
snapshot invalidation at reflow. Instance nodes and IDs are not added to the page
DOM; recursion and node bounds apply. Empty-gradient normalization and removal of already-resolved custom
declarations from raster snapshots also fix fallback-color and hidden-symbol
rendering errors. The original page DOM/CSSOM is preserved.

Six controlled fixtures pass 27 DOM checks and 68 pixel checks. Their complete
1280x712 RGB frames match Chromium exactly. Coverage includes host-dependent
colors/variables, selector boundaries, nested uses, same-root and cross-root
references, resource links, symbols/viewports, hidden content and source/stylesheet
mutations. The candidate also passes 33 general jobs, 30 focused adblock jobs,
five privacy suites, 13 native Wayland checks and 20 frontend tests. See
`../artifacts/servo-rendering-goal/svg-instance-v3-validation.json`.

The later optimized `svg-isolation-v2-candidate` fixes a reproducible custom-element
crash during private SVG cloning. Upgrade code enters the constructor realm before
wrapping JavaScript values. Renderer-only clones use a separate inactive document
and retain neither global nor scoped custom-element registries. Public clones and
explicit upgrades keep their normal registry behavior. After serialization, private
computed styles, parsed declarations and large style-attribute values are released.
Internal SVG images no longer appear in Resource Timing; author CSS images retain
their timing entries.

The constructor fixtures pass six global and four scoped checks, the timing fixture
passes seven checks, and 22 unchanged cloning/upgrade WPT assertions pass. At that
milestone, two of four cross-realm error-reporting assertions passed, versus zero
before; autonomous constructor errors and valid scoped construction remained open.
The subsequent construction milestone below addresses those failures.

The existing 95 SVG checks and six exact Chromium frame comparisons remain green,
along with 33 general jobs, 30 focused adblock jobs, five privacy suites, 13 native
Wayland checks and 20 frontend tests. Fresh anonymous Discord/X runs stay alive for
30 seconds and close cleanly without page-console errors; X's QR/cookie overlap
was unclassified at that milestone; its current attribution is below. See
`../artifacts/servo-rendering-goal/svg-isolation-v2-validation.json`.

Stability remains unfinished. The sixty-second SVG churn fixture passes five DOM
and four final-pixel checks over 7,240 animation cycles. Allocated heap rises from
about 115 to 188 MiB, versus 133 to 414 MiB in the previous candidate. Detached SVG
nodes still accumulate between collections; this is a targeted allocation reduction,
not a bounded-memory or leak-free result. GC pacing, allocator retention, complex
use-instance animation and shadow-tree stylesheet coverage remain open.

The optimized `registry-options-v3-candidate` retains the agent's active
constructor registry context. Traced invocation entries survive GC and restore the
previous registry after nested calls, success or exceptions. HTML constructors
consult that context before their global fallback, compare constructor identity
across wrappers, and record the registry on directly constructed elements.
Autonomous creation errors are reported to the constructor's associated global.
Window-associated documents create their global registry before parsing; inactive
author, template and private snapshot documents start with a null registry.

Element factories and shadow attachment now honor explicit scoped and null
`customElementRegistry` options. Invalid foreign global registries are rejected
before mutation. Global definitions select only elements associated with that
registry, so an explicit null registry cannot accidentally invoke a global
constructor. Fragment parsing resolves the actual insertion parent's document and
registry, including foster parenting and formatting reconstruction; nested template
contents remain inert. A pinned, provenance-checked html5ever/markup5ever callback
provides that context before element creation in both parser sinks. The default
synchronous parser is runtime-tested; the opt-in asynchronous sink is typechecked
but has not received a separate runtime acceptance run.

Cloning preserves scoped document/root associations and declarative null-registry
flags. An import fallback is propagated through every ordinary descendant without
replacing a non-null registry on an intermediate ancestor. Shadow descendants keep
their separate registry boundary. The final 31-check fixture and three painted
`:defined` checks pass. The preceding 22 construction checks, four cross-realm
error assertions, 54 HTML constructor assertions and 22 cloning/upgrade assertions
remain green. The earlier 28-assertion scoped subset now passes 28/28; the broader
factory, parsing, import, adoption and upgrade audit improves from 65/174 to
170/174 with no previously passing assertion lost. All 20 import assertions pass.
The first registry-options candidate was rejected for an import fallback regression;
the final candidate fixes it rather than hiding the failing test.

Four raw WPT assertions remain visible. Three require copying a closed cloned
root's availability to ElementInternals, a step Chromium explicitly marks as not
currently specified. Independent non-cloning upgrade controls pass. One
insertAdjacentHTML assertion disagrees with the current HTML body-context
substitution steps and needs further standards review. These distinctions are
recorded in `../artifacts/servo-rendering-goal/registry-options-standards-discrepancies.json`;
this is not complete scoped-registry or whole-browser conformance.

The 95 SVG checks and six exact reference frames, 33 general jobs, 30 focused
adblock jobs, five privacy suites, 13 native Wayland checks and 20 frontend tests
also pass on this candidate. Anonymous Discord/X sessions survive 30 seconds and
close without reported page errors or crashes. Discord's login form remains intact.
X's QR/cookie overlap is reproduced in Chromium with a matched viewport, dark
preference and Servo user agent: the authored fixed card uses z-index 1000 above
the banner's 30. The shared reduction passes six controls in each engine and has
an exact frame match, so this observation is page-authored stacking rather than a
Dome-specific rendering failure. Authenticated workflows were not revalidated.
GC pacing, allocator retention and broader compatibility remain unfinished; the
sixty-second churn diagnostics do not establish bounded memory. See
`../artifacts/servo-rendering-goal/registry-options-v3-validation.json` and
`../artifacts/servo-rendering-goal/x-overlap-attribution.json`.

Inline SVG snapshots preserve computed fill/stroke paint, same-document
paint-server URLs and fallback colors. Computed-paint changes invalidate the
snapshot after parallel style traversal, so stylesheet rules, inherited
`color`/`currentColor`, CSSOM edits and custom properties repaint correctly.
The public X login buttons now show black phone/Apple icons on their light
backgrounds; the previous candidate left those icons white.

The optimized `svg-visibility-candidate` validates computed descendant opacity,
display, visibility, local clipping and paint order in SVG snapshots, including
restyling. Snapshot inline styles are normalized and replaced properties are
removed before adding computed values. Root opacity is applied once by layout;
visibility remains in the vector snapshot so visible descendants can override
a hidden SVG root, while ordinary hidden images stay hidden.

The candidate passes nine compositing pixel checks, six CSSOM checks, 24
compositing restyle checks and ten visibility checks. Earlier paint6,
restyle15 and gradient45-pixel/90-CSSOM suites also pass, as do 46 browser,
17 security and 18 Wayland/media checks and native SVG/rich-text input.
The immutable binary and results are `svg-visibility-candidate` and
`svg-visibility-validation.json` in `../artifacts/servo-rendering-goal/`.

The optimized reporting/fallback follow-up passes 70 `reportError()` assertions
in windows and dedicated workers, five unchanged reportError WPT checks, and
nine unavailable-paint-server fallback checks. Symbols, revoked proxies and
objects with throwing conversion hooks keep their original ErrorEvent value;
reporting does not invoke their conversion hooks or panic the script thread.
Empty native Error messages use their intrinsic type name. External SVG
paint-server loading itself remains unsupported; failed references use their
specified paint fallback, including transparent paint when omitted.

`layout-capture-candidate` passes 21 regression jobs, including 395 focused WPT
checks. The opt-in `DOME_BROWSER_LAYOUT_CAPTURE=/absolute/path/report.json`
produces a private structure, computed-style and geometry snapshot after 15s;
`DOME_BROWSER_LAYOUT_CAPTURE_DELAY_MS` changes that delay. Text, input values,
cookies and storage are excluded. `layout-geometry-candidate` adds SVG geometry
attributes to this diagnostic output with the same validated engine.
The diagnostic also records media capability queries, player state and buffering,
with source origins/schemes instead of signed URLs. Set
`DOME_BROWSER_LAYOUT_CAPTURE_COUNT` (1–24, default 1) and
`DOME_BROWSER_LAYOUT_CAPTURE_INTERVAL_MS` (default 5000) for a time series;
subsequent files use `.1.json`, `.2.json`, and so on.

Continuous inline-SVG mutations are coalesced while a snapshot is being decoded
and rasterized. The last completed frame stays visible until its replacement is
ready, then the superseded source and raster resources are retired. Each SVG
owns its snapshot URLs, so removing one element cannot evict an identical
sibling's frame. Detach and failed snapshots also release pending resources.
`tests/svg-animation.py` verifies that paint changes reach the screen during
uninterrupted animation, a detached SVG disappears, and an identical sibling
remains visible. All four pixel checks match the Chromium reference. X's large
animated logo now renders in anonymous live-page checks.

The optimized `svg-animation-media-candidate` passes 24 regression jobs,
including the animation/ownership fixture, 395 focused WPT checks and both COEP
fixtures. Public Discord and X checks remain alive at 30 seconds and close
normally. The candidate also passes `tests/media-init-shutdown.py`, which delays
real GStreamer plugin initialization while requesting browser closure. In-process
engine shutdown waits for the media backend to finish initializing before
returning to the embedder; this prevents process exit from unloading GLib during
plugin constructors. This test uses a private profile and a test-only Linux
interposer, with no delay in production. Results are recorded in
`../artifacts/servo-rendering-goal/svg-animation-media-validation.json`.

Font-face registrations also survive stylesheet replacement when the new rules
reuse already-loaded font URLs. Sources remain eligible for ordinary Fetch and
HTTP caching, old rule owners are retired before additions, and templates are
deduplicated only for the same defining rule. This fixes X's heading switching
from Chirp Bold to Chirp Light after its client updates the stylesheet. The
computed weight stays 600 and resolves to the available bold face.

`tests/font-replacement.py` checks 11 replacement, removal, restoration and
shared-URL ownership cases using bundled DejaVu fonts. `--check-csp` adds two
policy controls: a new rule cannot use a cached URL after `font-src 'none'`,
while an already-loaded independent face remains usable. The 13-case native
and Chromium fixtures have identical hashes and pass. All six existing font
tests also pass. The optimized `font-replacement-candidate` passes 27 regression
jobs, including 395 focused WPT checks, SVG animation, delayed media initialization
shutdown and the font fixtures. Anonymous Discord/X live checks render and close
cleanly; X's separate undefined page exception remains. Results are in
`../artifacts/servo-rendering-goal/font-replacement-validation.json`.

The script font-loading API remains gated by `dom_fontface_enabled`, which is
disabled in the normal Dome build. CSS font loading no longer constructs hidden
DOM `FontFace` wrappers that accidentally install the constructor despite that
preference. CSS fonts and `document.fonts.ready` continue to work.
`tests/fontface-disabled.py` verifies 16 exposure and rendering checks across
loading and removing independent CSS faces.

The compiled API also preserves CSS-connected faces during `clear()`, removes
faces when their defining rules disappear, and starts the faces matched by
`FontFaceSet.load()`. Each load waits on those faces' promises, preserving result
order and rejection reasons without waiting on unrelated loads.
`tests/fontface-core.py` targets an API-enabled test build: 18 checks cover
membership, unused/script faces, Unicode ranges, ordered results, CSP and CORS.
The normal build does not enable that configuration.

Loaded script faces retain sanitized bytes when detached and transfer them into
every document where they are added. Each face has its own rendering identity,
even when another face uses the same URL. Deleting or clearing a script face
retires its templates and cached fonts without removing sibling or CSS owners;
a pending load cannot reinstall a face after deletion. Weak set observers allow
load completion to notify multiple documents without retaining their lifetimes.
`tests/fontface-ownership.py` passes 28 checks in both the native API-enabled
candidate and Chromium, covering URL/buffer retention, deletion/re-addition,
same-URL owners and cross-document membership. The previous native candidate
panicked while rendering a shared face in another document. Pending-load closure
and the 18 core loading checks pass with the fix. The normal release also passes
33 general jobs, 30 focused adblock jobs, five privacy suites, 13 native Wayland
checks and 18 frontend tests. See
`../artifacts/servo-rendering-goal/fontface-ownership-v1-validation.json`.

Font bytes are now owned by unattached script faces or pending completions until
a document installs them. Canceled stylesheet loads do not cache bytes, and
collection retires unpainted resources after their last owner disappears. Seven
native lifetime tests pass (six failed before the fix), alongside six existing
font tests. They cover detached buffer allocation release, unpainted removal and
re-addition, script-only and mixed CSS/script loads, canceled stylesheets and
independent owners. `tests/font-resource-lifetime.py` loads and removes 104 unused
CSS fonts in a disposable software-rendered browser, with eight warmup cycles.
In two before/after runs, measured RSS growth fell from about 68 MiB to about
4 MiB; this diagnostic does not establish that the browser is free of other
memory growth. The optimized candidate passes the same broad regression gates.
See `../artifacts/servo-rendering-goal/font-resource-v1-validation.json`.

Descriptor/matching behavior, CSS-connected status/ready/events and worker support
still need work before full API exposure. Nine tentative family-parsing WPT failures are retained; the
selected API-enabled WPT groups remain at 6/7 passing.

Runtime errors reach the embedder console after error-event cancellation has
been checked. Dedicated-worker errors first propagate to their owner and, when
still unhandled, to the owner's global. Canceled errors stay out of the console;
an unhandled worker error is reported once. The native
`tests/error-reporting.py` fixture passes 16 cases (100 DOM assertions plus 16
console-count checks) across Error/undefined values, listener cancellation,
`onerror` true/false returns and worker propagation. Chromium agrees on 14 cases;
its forwarded-to-Window cancellation behavior differs in the remaining two.
Those cases retain the [HTML reporting requirements](https://html.spec.whatwg.org/multipage/webappapis.html#report-an-exception)
as the native expectation, with the reference difference recorded explicitly.

The optimized `error-observer-candidate` passes 28 regression jobs, including
395 focused WPT checks and the new error-reporting suite. Set
`DOME_BROWSER_ERROR_TRACE=1` to add a document-start error observer that logs
exception metadata, cancellation state and an observer stack. It does not
replace APIs, cancel events, or read DOM text, input values or storage. Four
trace-enabled robustness cases pass 20 assertions, including revoked proxies
and throwing conversion hooks. Ordinary regression runs leave tracing disabled.
X's remaining undefined error is uncanceled and also occurs with filtering
disabled in temporary profiles; no originating frame was recovered by this
observer. `tests/live-pages.py --disable-adblock` provides that isolated filter
control. Both public samples still render and close normally. Results are in
`../artifacts/servo-rendering-goal/error-reporting-validation.json`.

The subsequent `module-evaluation-candidate` fixes the cause of X's undefined
exception. A native debugger trace identified the module executor, and inspection
at the SpiderMonkey API boundary confirmed that it passed a pending evaluation
promise to `ThrowModuleErrorsSync`, whose contract excludes top-level await.
Module execution now returns its evaluation promise and attaches a rejection
reaction when reporting is enabled. Successful awaits do not report errors;
actual rejections retain their original value and report after settlement.
Aborted evaluation without a pending exception rejects with `QuotaExceededError`.

`tests/module-evaluation.py` covers synchronous and asynchronous modules,
dependencies, dynamic imports, original rejection values and module-worker
termination. The Chromium reference passes all 17 cases; the previous native
candidate passed six of the initial 16 cases. The optimized fix passes all 17
native cases and all 29 regression jobs, including 395 focused WPT checks,
browser/security/input fixtures, SVG/font tests and shutdown controls.
Fresh 30-second anonymous X and Discord checks report no page-console errors or
crashes and close normally. These checks use isolated profiles and software
rendering; authenticated workflows and GPU behavior are not established by them.
Build provenance, native debugger evidence and validation results are recorded
in `../artifacts/servo-rendering-goal/module-evaluation-validation.json`.

Private SVG raster snapshots now serialize resolved colors as legacy sRGB,
including modern `oklab()`/`oklch()` paint, currentColor, gradient stops, stroke,
alpha and paint-server fallbacks. CSSOM retains its modern color syntax. SVG
presentation attributes retain variable declarations for substitution during
the cascade; trailing junk after CSS-wide keywords is rejected. The 34-check
native pixel/CSSOM fixture matches Chromium, including variable changes after
initial paint. This fixes the black icons in authenticated Discord Friends and
server-channel views, which now render in their theme colors.

Cache batch failures also retain their original rejection when canceling sibling
fetches synchronously reenters a pending body reader. Only the first failure can
reject the batch; canceled siblings cannot replace it with AbortError. Twenty
controlled checks cover HTTP 404, partial responses, Vary wildcard, atomicity,
successful streaming and caller-supplied abort reasons. All pass in native Servo
and Chromium; the previous native build passed 11/20. The optimized
`cache-batch-errors-candidate` passes all 35 regression jobs, including 451 focused
WPT assertions, the 34 SVG paint checks, Cache restart persistence and the input
and shutdown suites. Fresh authenticated Discord Friends/server and X Home
checks stay alive for 30 seconds and close cleanly. Discord has no page-console
error; X's account-settings API HTTP 404 rejection remains. Results are in
`../artifacts/servo-rendering-goal/cache-batch-errors-validation.json`.

Dome also exposes native `stop-color` and `stop-opacity` parsing, CSSOM and
cascade support through its pinned Stylo source. SVG presentation attributes
feed the cascade, computed stop styles reach the standalone rasterizer, and
changes invalidate the snapshot. This includes non-inherited defaults,
explicit inheritance, `currentColor`, color alpha multiplied by stop opacity,
percentage opacity and clamping. CSS-wide presentation-attribute keywords are
preserved as declarations; for example, `fill="initial"` now resets a green
inherited fill to black. The tracked Stylo snapshot retains the existing
revision and dependency versions; provenance, the small local patch and the
verification command are in [README.dome.md](../../third_party/stylo/README.dome.md).

The six-case paint fixture passes, including the previously failing stylesheet
stop-color case. The gradient-stop fixture passes 45 pixel checks and 90
computed-style checks across initial paint, change and restoration. Its HTML
hash and computed-style reports match the Chromium reference. WPT-format paint,
repaint, gradient-stop and presentation-keyword reftests are checked against
whole-frame references in the native embedding; this is not a full WPT run.

Native engine shutdown interrupts and wakes registered SharedWorkers before
waiting for their JavaScript runtimes. Three regression cases keep HTTP, blob
and actively executing blob workers alive, verify that two connections share
each worker, then close through the normal window protocol. All three
previously hung and now exit cleanly. This does not establish complete per-tab
SharedWorker lifetime conformance.

```sh
python3 ports/dome-fltk/tests/svg-paint.py --output /tmp/dome-svg-paint
python3 ports/dome-fltk/tests/svg-restyle.py --output /tmp/dome-svg-restyle
python3 ports/dome-fltk/tests/svg-stops.py --output /tmp/dome-svg-stops
python3 ports/dome-fltk/tests/worker-shutdown.py --output /tmp/dome-worker-shutdown
python3 ports/dome-fltk/tests/live-pages.py --output /tmp/dome-live-pages
python3 etc/verify_stylo_vendor.py
```

These fixtures accept `--binary` and use private profiles, D-Bus sessions and
Xvfb. Browser, security, rich-text and interaction/media scripts accept
`DOME_BROWSER_TEST_OUTPUT` to preserve separate candidates' results. The final
optimized candidate passed 46 browser, 17 security, 18 native Wayland/media
checks, all 78 native Stylo unit tests and the native SVG/rich-text input
fixture. Fresh anonymous Discord and
X runs render their public login/landing controls, survive 30 seconds and close
normally. Discord reports no page-console exception; X still reports one.
These runs do not establish authenticated whole-application correctness.

The paragraph above describes the earlier gradient milestone; subsequent
validation and X's module-execution fix are recorded above. Authenticated
whole-application behavior and the HLS/MSE player limitation remain compatibility
work. The installed system application has not been replaced by these test
candidates. Detailed reports and before/after artifacts are in
`../artifacts/servo-rendering-goal`; `gradient-final-validation.json` records the
earlier gradient milestone.

## SVG image resources and local-file authority

SVG `<image>` resources use the browser's image cache and fetch policy, including
CSP, CORS, cookies and content filtering. Decoded resources are prepared
asynchronously for private static SVG snapshots. Public `href` attributes and
XML serialization retain the author's URLs; snapshot clones do not issue author
requests or load events. Simple removal or same-document moves preserve a
pending or completed request, including moves made by a load handler. Source changes
and adoption invalidate obsolete generations; reconnecting after an effective
base-URL change selects the new resource.

The file protocol checks local-file authority before opening, inspecting or
listing a path. Native top-level file navigation remains supported. A successful
local document receives an origin-bound grant, which same-principal inherited
contexts can retain. HTTP documents and workers, unrelated opaque origins,
forged referrers and copied grants from another principal cannot use that grant.

The optimized candidate and validation scope are recorded in
`../artifacts/servo-rendering-goal/svg-image-request-v1-validation.json`.
The new candidate passes 16 image lifecycle checks, 16 move/request-identity
checks, 17 SVG policy/adoption/adblock/shutdown jobs with 151 checks and 42 pixel
probes, and 149 cookie checks. These results supplement the recorded general,
privacy, Wayland and scoped standards regressions. The 440 network unit tests
are inherited from the file-boundary candidate with unchanged network sources;
the 20 frontend tests are also inherited with unchanged frontend sources.

Reusable checks use temporary profiles, generated local files and private
Xvfb/DBus sessions. They do not require changing an active desktop session:

```sh
uv run --no-project --with python-xlib --with pillow --with websockets --with dbus-next \
  python ports/dome-fltk/tests/svg-image-loading.py \
  --binary target/release/dome-browser --output /tmp/dome-svg-images
uv run --no-project --with python-xlib --with pillow --with websockets --with dbus-next \
  python ports/dome-fltk/tests/svg-image-reparent.py \
  --binary target/release/dome-browser --output /tmp/dome-svg-image-moves
uv run --no-project --with python-xlib --with pillow --with websockets --with dbus-next \
  python ports/dome-fltk/tests/file-boundary.py \
  --binary target/release/dome-browser --output /tmp/dome-file-boundary
```

The file-boundary runner explicitly reports nonexecuting file-origin blob
contexts rather than counting them as passing isolation controls. Add
`--require-file-blobs` to make their remaining execution gaps fatal. External
SVG `<use>` resource documents, full SVG image animation/canvas/IDL semantics,
never-connected image fetching and comprehensive long-session/authenticated
site coverage remain incomplete. The extended move test also preserves native
base-sensitive reconnect behavior; Chromium retains the old resource in that
case, so the reference comparison is not a claim of identical 16/16 behavior.
The existing `document.open()` cancellation difference for already-started HTML
and SVG images also remains recorded for later standards/lifecycle coverage.

The observed X app QR-code overlay overlapping its cookie banner is classified
as a non-bug by the user. It is excluded from browser rendering failure counts
and acceptance blockers; the original captures remain in the validation evidence.

## Blob worker origin and revocation

The optimized `blob-trust-v1-candidate` preserves the origin carried by a blob
claim during mixed-content and upgrade checks. A blob response can use that
origin only when its URL matches the current request claim. The blob backend
continues to validate the token and origin before returning bytes, and the file
protocol continues to require its explicit principal-bound grant.

Dedicated classic blob workers created by a local page now execute and retain
the creator's authorized local-file access. Claims acquired before URL revocation
remain usable; revocation before construction, missing entries, foreign HTTP
creators and unrelated opaque creators cannot execute a worker in the parent.
The 24-check browser fixture includes HTTP-parent and foreign-creator positives.
Six additional Rust checks acquire claims from the real blob store and cover
request/response matching, redirects, unclaimed/opaque origins and revocation.
All 446 network unit tests pass on this change.

```sh
uv run --no-project --with python-xlib --with pillow --with websockets --with dbus-next \
  python ports/dome-fltk/tests/blob-worker-policy.py \
  --binary target/release/dome-browser --output /tmp/dome-blob-workers
cargo test --locked --offline -p servo-net --test main blob_trust
```

The candidate also passes the recorded SVG, cookie, adblock, general rendering,
Wayland and scoped standards regressions. Evidence and exact limits are in
`../artifacts/servo-rendering-goal/blob-trust-v1-validation.json`. A separate
local HTTPS/HTTP probe passes the tested iframe, insecure redirect and fetch
boundaries in native and reference browsers; it does not prove every navigation
mechanism. The 20 frontend unit tests and earlier GC pacing measurements remain
inherited evidence, with unchanged frontend and GC code.

File-origin blob document navigation remains unsupported. The file-boundary
runner reports that nonexecuting document explicitly and now verifies the local
blob worker as an executing positive. This milestone does not establish complete
module/shared/service-worker behavior, authenticated site operation or long-session
reliability. Broader SVG resources, standards coverage, XR beyond the accepted
DomeD scope and tertiary protocols remain outstanding.

## GC pacing and cleanup tasks

The optimized eager-GC candidate uses an 8 MiB base native-allocation threshold
and checks SpiderMonkey's eager-collection hints while the event loop is idle.
Due input and rendering work are polled first. Collections return through normal
microtask/render checkpoints; incremental GC remains disabled.

Three sequential 900-second DOM/adblock diagnostics with bundled lists compare
the 38 MiB baseline, an 8 MiB threshold alone, and 8 MiB plus idle collection.
All 278 checks pass per run. The final policy reduces sampled peak allocated
heap from 348.43 MiB to 172.53 MiB and peak native RSS from 662.67 MiB to
416.31 MiB. Its 17 normal collections have a 48.383 ms maximum pause, compared
with 399.14 ms for the baseline's one collection. Total normal GC time rises
from 399.14 ms to 757.295 ms. The worst observed frame gap is 293.05 ms, so
latency is not fully resolved. Privacy fetch/beacon probes stay blocked and
445 legitimate requests complete in the final run. These single controlled
software-rendered workloads do not prove bounded memory or a statistical
performance guarantee. See `../artifacts/servo-rendering-goal/gc-eager-v1-validation.json`
and `gc-eager-v1-pacing-comparison.json`.

FinalizationRegistry cleanup is delivered through ordinary JavaScript engine
tasks after microtasks. The GC hook records work without running author script.
Queued callbacks and incumbent globals remain traced until execution or
cancellation; runtime teardown disables the hook and releases its roots.
Native/reference diagnostics cover held values, unregistering, task ordering,
cleanup-created microtasks, exceptions, cross-realm registries and dedicated
workers. The native 19-check diagnostic passes with default adblocking enabled
and disabled. Five window-close and five worker-termination stress runs close
cleanly with GC activity recorded. This is release-build evidence for those
cases, not proof of every GC interleaving, all worker/worklet environments or
complete host settings semantics. No cleanup deadline is guaranteed.

`python ports/dome-fltk/tests/finalization-registry.py --binary /path/to/browser --output /private/output`
runs the private-profile diagnostics and five repetitions of each teardown
case. Use the same Python dependencies as `cache-api.py`; `--lifecycle-runs 1`
provides a shorter runner check. See `../artifacts/servo-rendering-goal/finalization-v1-validation.json`.

## Current development priorities

The primary goals are stability, robust adblocking, local workstation security,
and correct rendering under web standards. Discord and x.com are diagnostic
samples, not unconditional visual-parity requirements. Failures attributable to
nonstandard requirements, unsafe operation, proprietary technology, or deliberate
prevention of adblocking do not count as browser rendering failures. Reproducible
failures of standards-compliant content still count as defects. Compatibility
work must not require disabling adblocking or weakening security.

After stability reaches an acceptable failure level, mandatory smallnet support
is the next implementation priority, before further rendering expansion:
Gopher, Gopher+, Gemini, Nex, Spartan, Scroll, finger, and read-only FTP. Other
smallnet protocols are evaluated for viability, and viable additions are
implemented during this phase. WebXR compatibility and standards compliance
follow smallnet integration. Rendering improvements are the final major phase.
The existing working DomeD WebXR baseline is preserved throughout. Stability
remains a requirement in every phase. Adblocking and privacy filtering apply to
web protocols; smallnet protocols are outside that scope by user instruction.
Further native media
playback/MSE development remains outside this scope; external-player handoff is
the supported extension for unsupported media.

Cookie storage uses the profile's private `ck.txt`; see below. Third-party
cookies are blocked by default, with a persistent native control.

### Current stability milestone

The optimized `webgl-gc-v1` candidate corrects the observed desktop crash in
graphics resource cleanup. It passes 16 focused checks, two 15-minute local
sessions with clean shutdown, 33 general regression jobs, 13 native Wayland
checks, and both application-level cleanup cases. All five relevant graphics
conformance cases now close normally, with the same 148/164 passing assertions
as the previous candidate; 16 pre-existing mismatches remain.

The measured sessions settled to approximately flat final RSS levels. This is
a provisional stability milestone for the recorded scope, allowing smallnet
implementation to begin. It does not establish every-site, every-hardware, or
bounded-memory behavior. Anonymous Discord and X captures retain their observed
page shells; authenticated behavior is outside these checks. Existing privacy,
cookie, adblock, network-unit and frontend-unit results are inherited rather
than fresh reruns. See
`../artifacts/servo-rendering-goal/webgl-gc-v1-validation.json` for scope and
source-provenance limitations.

### Native protocols and navigation

The development source has initial native Gopher menus, text and search; Nex
directories and text; finger retrieval; and Spartan loading with gemtext,
native input prompts, redirects and character encoding. These share bounded
loading and retain native protocol addresses. The Gopher+ development path adds
counted, dot-terminated and connection-terminated replies, server errors,
attributes and alternate-view links. RFC 4266 encoded-tab search and command
fields use the standard `gopher://` request layout. The convenience `gopher+://`
alias uses the original Gopher+ memo's request layout (one command tab for a
non-search item); it is not a newly standardized URI scheme. Menu `+` markers
select that enhanced path and expose information/view links. Metadata and
referenced attributes remain inert until a link is selected. Native ASK forms
provide text, masked password, multiline, choice and checkbox controls, notes,
explicit file selection and response saving. `Select` follows the original
client's checkbox convention; repeated fields allow multiple selections. Forms
stay separate from the browser window: navigation, tab changes and shutdown
cancel the pending form. Reload opens a new form rather than reusing answers.
Submitted data stays in the pending load and is not put into generated URLs.
File prompts require an explicit native selection; server-supplied filenames
do not authorize local file access. A saved response replaces the selected
destination atomically with private permissions.
Pre-filled Gopher+ form URIs require native review before any supplied data is
sent. Their result uses a clean ASK address; reload asks for fresh input.
Referenced ASK definitions use the ordinary Fetch checks, with native approval
before contacting a different definition server. Approval lasts for that one
form load, answers go to the original form target, and reference chains are
bounded. Explicit ASK fields take precedence over a referenced definition.
Gemini development support uses native TLS 1.2/1.3, gemtext/plain rendering,
status handling, native input and client identity controls, redirects and
cancellation. Server certificates use first-use pins in the private profile's
`gemini-trust.json`; a changed certificate requires native review before a
request. Client identities require explicit selection and can optionally be
remembered for a server, path and server certificate. Sensitive input uses a
masked control and is removed from resulting public addresses and persistence.
Sensitive-input redirects to other protocols are currently refused.

Identity selection prefers existing Penguin Mail Misfin and XNedit XR references,
then the shared `$XDG_DATA_HOME/de-xr/identities/index.json` catalog. It reuses
existing keys. Explicit creation publishes a private, Misfin-compatible PEM pair
to that catalog for Dome Identities, Penguin Mail and XNedit XR. Manual PEM import
requires certificate and private-key file selection. Remembered references bind
the approved certificate fingerprint; private key bytes are absent from browser
settings. Encrypted PEM keys are not supported. Creating a certificate does not
register a provider mailbox. See `dome-identities/README.md` from the workspace
root for shared manager operations and catalog permissions.

Read-only FTP development support provides directory and file retrieval,
UTF-8 machine-readable listings and name-list fallback, EPSV with IPv4 PASV
fallback, FTP URL path components and `;type=a`, `;type=i`, `;type=d`. Each load
uses a fresh control connection. Data connections use that control peer's address
and the browser's existing port restrictions. Anonymous login uses a nonpersonal
identifier. Named login and optional server accounts use a native masked prompt;
credentials apply only to the current load and are removed from result URL
chains. No FTP credentials are stored in the browser profile.

Text documents use the existing inert native document policy. Other FTP files
require a native save destination, with private atomic writes and explicit
replacement confirmation. Transfers are bounded; known binary sizes and final
server completion replies are checked before accepting data. Navigation releases
both data and control connections. FTPS, transfer resume and cross-load login
retention are not provided by this initial FTP implementation. See
`tests/ftp.py` for private protocol/UI fixtures (`--wayland` uses a private nested
DomeD compositor) and RFCs
[959](https://www.rfc-editor.org/rfc/rfc959.html),
[1738](https://www.rfc-editor.org/rfc/rfc1738.html),
[2428](https://www.rfc-editor.org/rfc/rfc2428.html), and
[3659](https://www.rfc-editor.org/rfc/rfc3659.html) for protocol mappings.

The desktop entry advertises the implemented Gopher, Gopher+, Gemini, Mercury, Nex,
Spartan, finger, FTP and Scroll URI handlers alongside HTTP(S).

Native resources use the existing `session.json`, `saved-session.json`, `hs.txt`
and `bm.txt` formats. Sessions retain tab order, the active tab and per-tab zoom;
native bookmarks and address completion use the same controls as web pages.
Sessions also retain each Scroll tab's resource or abstract view; older session
files default to the resource view. View state does not change the saved URI.
Gemini/Scroll lookup queries and section fragments are retained. Finger's lookup
username is part of its resource address. FTP login state remains per-load.
Gopher+ supplied form data is saved as a fresh ASK address, and Spartan upload
bodies are omitted from saved resource addresses, so restoring navigation does
not repeat a previous submission. Temporary native action links are not saved.

Scroll development support follows the author's
[archived speculative specification](https://raw.githubusercontent.com/michael-lazar/smolnet-portal/master/docs/scroll_spec.txt).
Requests include an ordered BCP47 language list derived from the native locale;
`language-tags` validates and formats the tags. Success responses consume the
three UTF-8 metadata lines before interpreting file bytes. Scrolltext supports
five heading levels, section hash links, nested lists and quotations, fences,
thematic breaks, escaped line types, normal links and native input links.
Optional inline emphasis/code toggles are currently presented literally.
Metadata dates are displayed as supplied.

Opening a resource does not prefetch its abstract. Selecting Resource abstract
requests the abstract and renders it as Scrolltext independently of the
underlying resource MIME type. The selected view retains the public URI,
including its query and fragment, across reload, back/forward and session
restoration. Resource and abstract entries at the same URI keep independent
view state. Open resource explicitly retrieves the ordinary resource even when
the current URI has a fragment. Explicit source downloads still save the resource.
Input links support Scroll and Spartan; unsupported input schemes remain
visibly disabled. Scroll input replaces the entire previous query. Sensitive
responses remove the query from resulting public URLs and stored navigation.
Cross-protocol sensitive redirects remain refused.

Scroll uses the existing `gemini-trust.json` file with separate `scroll://`
authorities, preserving Gemini records and keeping certificate/identity scopes
separate between schemes. Native certificate review, shared PEM reuse/creation,
TLS 1.2/1.3, SNI and cancellation share the existing TLS implementation. Text uses
the inert native document policy. Images render in a native document wrapper
that keeps the public resource URI and Scroll metadata; other data uses the
native save chooser. Document and image previews retain their 8 MiB limit;
explicit file saves use the streamed transfer path described below.
`tests/gemini.py --protocol scroll`
exercises private Scroll fixtures; `--wayland` uses a private nested DomeD session.
Native smallnet dialogs use FLTK's non-modal transient window state to stay
above their browser while allowing replacement navigation to cancel a request.

`tests/smallnet-forms.py --wayland` exercises native Gopher+ controls, explicit
file handoff, referenced definitions, reload and cancellation in private DomeD.
Native image navigation covers Gopher/Gopher+, Gemini, Scroll, Nex, Spartan,
and FTP. The generated wrapper embeds the retrieved image without a second
protocol request. PNG/SVG pixels and Gopher GIF are covered by private fixtures;
this does not claim every image decoder. Finger remains a text protocol.

Ctrl+S saves the original native response bytes, rather than the generated HTML.
Save Link and Save Image use the XDG Downloads directory, preserve encoded image
and binary bytes, and keep the current page open. Copy Image exports a decoded
raster image to the native clipboard. Unsupported binary navigation opens an
explicit native save chooser. Document and image previews retain the existing
8 MiB bound. All eight mandatory native protocols have file saves that stream in acknowledged
64 KiB chunks through Save Link and Ctrl+S; binary navigation through the native
chooser also streams for file protocols. FTP retains its announced-size and
final server-completion checks. A failed or canceled
chooser save removes its temporary file and preserves the previous destination.
Progressing transfers use an idle deadline instead of the former whole-load
limit. Gemini and Scroll keep their buffered TLS reader through the save,
require a clean TLS end of stream, and complete the client TLS shutdown before
accepting the file. Scroll removes all three metadata lines before streaming
resource bytes. Gopher binary transfers retain their EOF-delimited bytes;
text source saves remove dot framing incrementally. Gopher+ file saves support
counted, dot-terminated and EOF-delimited bodies. Counted and dot-terminated
responses finish at their frame boundary even if the server keeps the socket
open; an incomplete count or explicit dot frame fails. Basic Gopher retains
its existing compatibility with text peers that omit the final dot line.
ASK form responses use the same stream, including an explicitly selected
response destination alongside a context-menu download. Selected destinations
are replaced only after successful transfer completion.

Native protocol redirects re-enter Fetch with the original consumer. This keeps
streamed bytes and completion notifications attached to the user's download;
redirect-limit failures also deliver their final error notification. Private
fixtures cover 128 MiB FTP files, unknown sizes, slow progress, incomplete and
failed transfers, cancellation, and a Gemini redirect to FTP on Wayland and X11.
Additional 128 MiB fixtures cover Nex/Spartan Save Link, chooser save/cancel and
Ctrl+S on both backends, finger source saves, and slow/interrupted transfers.
The large-transfer driver also provides Gemini/Scroll save, cancellation,
input/identity/redirect continuation and clean/unclean TLS completion cases.
Gopher/Gopher+ cases cover native chooser and source saves, all three Gopher+
frames, long text lines, original encoded bytes, and large form/URI-data responses
after explicit native approval. `--gopher-framing count|dot|eof` selects the
private fixture framing; document and metadata previews remain bounded.
These are scoped tests, not a general memory or every-server compatibility claim.

Native web saves obtain the active document's network context asynchronously.
Context-menu saves use the document that opened the menu, including child
frames. `tests/downloads.py` covers authenticated saves, frame context, filenames,
clipboard pixels, and clean exit. `tests/smallnet-media.py --wayland` covers the
native media workflows in a private DomeD session; omit `--wayland` for X11.

Each mandatory protocol has native transport and streaming file code. Broader
server interoperability, end-to-end installed URI dispatch,
associations, and Scroll abstract-mode persistence still prevent a complete
smallnet integration/compliance claim.
Smallnet protocols remain outside adblocking and privacy scope.

The optimized `smallnet-pending-nav-v2` candidate also repairs replacement of
unfinished first and subsequent page loads. Superseded pending fetches are
terminated without replacing the WebView or its context group. Four Nex/HTTP
lifecycle cases, 16 protocol loading/interaction cases, 16 graphics cleanup
checks, 33 general regression jobs, five privacy suites, 13 native Wayland
checks and 34 optimized frontend unit tests pass. Anonymous Discord and X
sessions close normally. This is scoped regression evidence; broader protocol
support and full platform compliance remain outstanding. See
`../artifacts/servo-rendering-goal/smallnet-pending-nav-v2-validation.json`.

## Cookie storage and editing

**Tools → Cookies → View or edit ck.txt** opens the native FLTK editor.
**Tools → Cookies → Delete all cookies** clears the profile's cookies after
confirmation. Cookies are saved on mutation, rather than waiting for shutdown.

`ck.txt` contains comments and one JSON object per cookie line. The `cookie`
field contains Set-Cookie text; the remaining fields retain host-only status,
persistence, creation/access time and absolute expiry. Edit the cookie text to
change a value, delete a record line to remove that cookie, or delete the whole
file to clear stored cookies. `expiry_time` controls stored absolute expiry:
reloading does not restart `Max-Age`. Preserve the metadata when editing values.

Cookie access reloads external file edits. The native editor refuses to overwrite
a file changed while the dialog was open. For extended external edits, close the
browser first so site requests cannot race the editor. Invalid records cause
persisted cookies to be withheld and disk writes to stop until the file is
repaired; the malformed contents are preserved. Cookie-file diagnostics omit
record contents. Symlinks are rejected and writes use a private, atomic file
replacement with mode `0600` on Unix.

Existing `cookie_jar.json` is imported once and removed only after `ck.txt` has
been saved durably. `.ck-initialized` records that `ck.txt` is authoritative;
deleting cookies must not import old values again. Private resource contexts
keep their cookies in memory and have no attached cookie file.

**Tools → Cookies → Block third-party cookies** controls cookie sending,
receiving and script access. The checked default blocks third-party cookies;
uncheck it to allow them. The setting is saved privately in
`cookie-policy.json` and applies to subsequent cookie access, including existing
frames and workers. It does not delete stored cookies: use **Delete all cookies**
to clear them. Manual edits to the policy file take effect on restart. Invalid
saved settings default to blocking and are preserved; a failed save leaves the
current policy in force.

Classification uses the scheme and registrable domain, including private public
suffixes. Ports do not create different sites. Cross-site ancestry remains
third-party even when a descendant returns to the top-level site's origin.
Referrer-Policy does not determine cookie access. Top-level navigation establishes
a first-party context for its destination. HTTP authorization remains separate
from cookie policy. WebSocket handshakes obey the same cookie control and use
the initiating origin, with one handshake key and a Host header including a
non-default port.

The local HTTPS regression covers blocked/allowed modes, restart persistence,
nested frames and workers, named SharedWorker isolation, opaque sandbox frames,
redirects, WebSockets, invalid settings, and save failures:

```sh
uv run --no-project --with python-xlib --with pillow --with dbus-next \
  python ports/dome-fltk/tests/cookie-policy.py \
  --binary target/release/dome-browser --output /tmp/dome-cookie-policy-test
```

SameSite enforcement applies independently of this control. Strict cookies are
withheld from cross-site requests; Lax, missing and invalid SameSite values allow
the safe top-level-navigation exception. Default cookies use Lax enforcement
without the optional temporary allowance for unsafe methods. `SameSite=None`
requires `Secure`. Cross-site script access exposes only eligible None cookies,
and cross-site subresource responses cannot set restricted SameSite cookies.
Top-level navigation can set them even when they were withheld from its request.

Navigation captures the source document's cookie site before that document can
be discarded. Redirects reselect cookies for the current target and method; a
cross-site redirect chain cannot regain Strict/Lax cookies on its return hop.
Toolbar reload retains the original navigation's same-site status, while script
reload uses the current document. HTTP and `ck.txt` edits share the same attribute
parser, including repeated and invalid SameSite attributes. Old insecure None
records are ignored without discarding unrelated valid records or immediately
rewriting user files. Nonpositive Max-Age expires safely, including extreme
negative values.

The local HTTPS navigation/script regression is `tests/samesite.py`, using the
same command form as the privacy fixture above. These tests do not establish
complete cookie-standard conformance. Cookie Store API guards are present, but
that API remains disabled by default and its runtime behavior is not covered by
the default-browser fixtures; broader Cookie Store and cookie WPT coverage remain
follow-up work. The navigation fixture covers ordinary HTTPS navigation and
reloads. `tests/samesite-hsts.py` also covers HTTP-to-HTTPS HSTS upgrades, toolbar
and script reloads, browser-initiated navigation, and same-site upgraded links.
The saved navigation classification checks the final response URL, so a toolbar
reload cannot disclose a Strict cookie withheld during an upgrade. Existing
redirect-chain restrictions remain in place. HSTS combined with redirect chains
and history restoration need broader dedicated coverage.

## External media player

GStreamer handles its existing supported formats. For unsupported media, use
**Tools → Open media in external player**. The browser finds media whose type is
unsupported or whose element reported a playback error. A single match opens
directly; multiple matches use a native chooser. If the site has no accessible
media element, the page URL is handed to the player's website extractor. A
link's context menu also offers **Open link in external player**.

The default is the first available `mpv`, `vlc` or `celluloid` executable on PATH.
This workstation's `mpv` is the Dome/XR fork. **Tools → External player...** saves
an absolute executable path in the private profile's `external-player.json`.
`DOME_BROWSER_EXTERNAL_PLAYER` can override that path for a session. The player
must accept its URL after `--`; no shell command or argument string is evaluated.
Launches happen only through native user actions, never from page media errors.

The player receives an HTTP(S) media or page URL. Browser cookies are not
exported. Blob-backed streams need the external player's support for the page
URL, and private content may need authentication configured in that player.
Media query strings needed for signed URLs are preserved and are not logged by
the handoff code.

Further in-browser media playback/MSE development is outside the current goal.
The experimental native MSE work is preserved and disabled by default.

The focused UI regression uses an isolated X server/profile and a fake player:

```sh
uv run --no-project --with python-xlib --with pillow --with dbus-next \
  python ports/dome-fltk/tests/external-media.py \
  --binary target/release/dome-browser --output /tmp/dome-external-media-test
```

### Mercury development transport

`mercury://` uses plain TCP on port 1961 and the Gemini-family status/body
parser, native input controls, gemtext/plain/image presentation, and streaming
file saves. It preserves native addresses in history, bookmarks and sessions.
The request URI omits fragments and accepts up to 2046 bytes plus CRLF, matching
[the reference implementation](https://github.com/reiver/go-hg). TLS pin stores
and client identities are used only for Gemini/Scroll; a Mercury response
requesting a TLS identity is unsupported. Existing Gemini connections continue
to require TLS, with no automatic plain-TCP fallback. Mercury fixture coverage
is part of `tests/gemini.py --protocol mercury`. Broader interoperability and
additional viable protocol implementations remain required.

### Gopher TLS development transport

`gophers://` uses TLS on port 70 with the existing Gopher and Gopher+ request,
menu, search, view, form and file-transfer handling. Implicit menu and ASK
references retain TLS; explicit `URL:` links retain their own scheme. Gopher+
commands use RFC 4266 URI layout on this scheme. Native trust pins are separate
for each scheme/host/port, persist before the first selector is sent, and require
review when the server key changes. Certificate dates and handshake signatures
are checked. This uses the same native TOFU policy as Gemini/Scroll, rather than
the web's certificate-authority policy. Gopher TLS never loads client identities
and never falls back to unencrypted Gopher. EOF-delimited bodies require clean
TLS completion; counted/dot-framed responses finish at their protocol boundary.

### Guppy development transport

`guppy://` implements the v0.4.4 UDP request and sequenced-response format on
port 6775. Requests include at most 2048 bytes including CRLF and omit URI
fragments. Each session keeps one source port, retries requests/acknowledgements,
reassembles bounded out-of-order data, and requires explicit acknowledged EOF.
Packet boundaries may split lines and UTF-8 sequences. Native input, redirect
status notices, plain/gemtext/image presentation, profiles and streamed file
saves use the existing frontend. Guppy never reads TLS identity or trust stores.
Text previews retain the existing 8 MiB limit and are converted after retrieval;
files stream through the ordinary acknowledged file consumer. Cancellation drops
the active UDP session. Completed sessions briefly re-acknowledge duplicate EOF
packets, with a bounded lifetime and concurrency. The implementation and fixtures
are based on [the protocol specification and reference server](https://github.com/dimkr/guppy-protocol).


### Native DICT dictionary browsing (development)

`dict://` uses RFC 2229 over TCP (default port 2628). Definition URLs
`dict://host/d:word:database:n` and match URLs
`dict://host/m:word:database:strategy:n` support percent-encoded UTF-8 words,
optional database/strategy fields and numbered results. Omitted database and
strategy use `!` and `.` respectively. Fragments are never sent as commands.

Opening the server root lists its dictionaries and matching strategies. Native
lookup actions select a dictionary or strategy and open a FLTK query form;
rendering the index sends no word lookup. An empty word in `/d:` or `/m:` is a
local query-form convenience and is never sent as an empty protocol command.
Match results link to definitions in the dictionary that supplied the match.
Sessions, bookmarks and history retain public DICT resource URLs. Source saves
contain readable definition/result text, without DICT status framing.

Response parsing requires the announced result count, dot terminators and the
final `250` completion. UTF-8 lines follow the RFC character and byte limits;
responses are bounded to 8 MiB and 1,024 items. Cancellation drops the connection,
and orderly shutdown does not wait indefinitely for peer EOF. Server text is
inert and escaped. Existing web filtering and cookie behavior is unchanged;
smallnet remains outside that filtering scope.

This implementation browses anonymous dictionaries. Optional AUTH/SASL identities
are explicitly unsupported; an authentication URI is explained locally and is
never retried anonymously. No TLS or identity store is consulted. Broader
interoperability and optional authentication remain separate work.


### Native CSO/Ph directory browsing (development)

`cso://host/` and `ph://host/` browse a directory with the read-only portion
of RFC 2378, using TCP port 105 by default. The root shows directory fields
and server information. Native lookup actions open a FLTK form; viewing an
index does not send a search. Gopher item type 2 entries open this client
at the descriptor's host and port, using pure CSO rather than Gopher wire
requests. A Gopher URI's omitted port retains its normal port 70 meaning.

Lookup resource URLs use this client's query convention:
`cso://host/?q=Ren%C3%A9e&field=name&return=name%2Cemail`.
`field` and `return` are optional; public URLs use UTF-8 percent encoding.
An empty `q` opens a local input form and is not sent as an empty query.
The client sends only directory queries, field/site information requests,
per-session character-set selection, and orderly quit. Database editing and
optional authentication are not implemented.

Search text is encoded as ISO-8859-1 with protocol quoting. Text outside that
protocol repertoire is explained locally without sending a different lookup.
Responses request ISO-8859-1; an explicit unsupported-option response uses the
US-ASCII/quoted-printable fallback. Latin-1 decoding preserves its codepoints
and does not apply web Windows-1252 aliasing.

Results preserve entry grouping, field-specific statuses and colons inside
values. A command completes only at a nonnegative status of at least 200;
peer EOF is not a successful partial result. Responses are bounded to 8 MiB,
16,384 lines per command, 64 KiB per line and 1,024 entries or field descriptors.
Cancellation releases the connection, and shutdown is bounded. Source saves
contain decoded UTF-8 text. Public lookup resources round-trip through native
sessions, history and bookmarks. Existing web adblocking and cookie policy
remain unchanged; smallnet remains outside that filtering scope.

CSO/Ph accepts the bare-LF response delimiters used by historical Qi servers, in addition to RFC 2378 CRLF responses. Outgoing commands always use CRLF. `tests/cso-reference.py` exercises an independently built Qi server and a private synthetic database; its loopback adapters preserve the server response bytes.


### Native news and NNTP browsing (development)

`news://server/group` and `nntp://server/group` show a native article overview.
`nntp://server/group/number` retrieves a numbered article; `news:Message-ID`
and `news://server/Message-ID` use the case-sensitive Message-ID without angle
brackets. Percent-encoded URI values are decoded once. Authority-less `news:`
resources use the configured default server and open a native server-selection
form when none exists. `news://server/` lists groups; Filter groups accepts
ordinary group names and the `*`/`?` wildcard forms. Catalogue previews explicitly
show when they stop after 1,024 groups. Overviews use 50-number pages and offer
older/newest navigation. Legacy readers use MODE READER, XOVER or bounded HEAD
retrieval when the modern capabilities/overview commands are unavailable.

The News server action selects the host, port, transport and optional additional
trusted CA PEM file. Settings are editable in private `news-servers.json` in the
browser profile. New default-server configuration prefers implicit TLS, usually
on port 563. Explicit authority URIs use conventional NNTP port 119 without TLS,
or implicit TLS on explicit port 563, unless configured otherwise for that
endpoint. STARTTLS, when selected, requires successful negotiation and refreshes
capabilities after the upgrade. TLS verifies certificate chains, validity and
hostnames; failed TLS is never retried as plaintext. Client identities and
optional news authentication are not implemented; servers requiring login return
a readable status. No posting, moderation, server-to-server transfer or cancellation
commands are sent.

Articles decode MIME headers, text charsets, multipart structure and transfer
encodings using pinned `mail-parser` 1.0.0. HTML bodies become inert readable
text. Attachments open native save controls and retain decoded bytes. Article
source saves stream the original message through the existing bounded download
consumer, removing NNTP dot framing and preserving CRLF bytes; source streaming
is independent of the 8 MiB display-preview bound. MIME previews allow at most
512 parts and 32 nested containers. Display and catalogue limits are explicit
client bounds, not claims about the protocol's allowed resource sizes.

`?before=N` selects an older overview page and `?part=N` selects an attachment;
these are local resource-query conventions, not NNTP commands. Sessions, history
and bookmarks preserve public resources. Smallnet continues to operate outside
web adblocking and cookie policy.

### Native Scorpion retrieval (development)

`scorpion://` and `scorpions://` implement the draft's read-only `R` request,
both on default port 1517. The TLS scheme uses the existing explicit trust and
shared PEM identity controls, with separate certificate pins and identity scopes.
Selected client chains must be complete and ordered from the identity to its
self-signed root. Every certificate signature is checked before use. Existing
self-signed identities remain supported. No identity is sent before selection.

Scorpion binary documents render through Servo using generated, escaped markup.
Headings, paragraphs, normal and input links, alternatives, blockquotes, inline
styles and preformatted text are supported. Interactive requests (`I`), server
modification (`S`) and metadata requests (`M`) are not issued. Interactive links
are identified as unavailable; alternatives stay closed until opened locally.
Ordinary document display does not prefetch other resources.

PC text and the implemented TRON character planes use Unicode font fallback.
Unmapped TRON characters stay visibly identified and the original source remains
available. ISO-2022 support is partial (JP-2/JP-3); unsupported designations fail
explicitly. Full ISO-2022, every TRON plane and general ULFI processing are not
claimed. Native `document:scorpion` files are supported; other formats use the
existing text, image or streaming download path.

Known-length responses finish at the declared boundary without waiting for the
peer to disconnect. Source saving preserves the original bytes through the
bounded download consumer. Input is submitted through native controls. Login
credentials apply only to one navigation and its same-authority redirects;
reload requires a new decision. Cross-authority redirects and a change from TLS
to plain transport require native review before contacting the destination.
Automatic redirect chains are limited to five hops.

`tests/scorpion.py` exercises these paths on isolated local servers. Its
`--reference-root` option builds pinned, unmodified upstream `scorpiond.c` and
`scorpionlib.c` using a read-only configuration, then tests retrieval and
C-generated documents. `tests/smallnet-profile.py --include-additional
--include-scorpion` includes both schemes in mixed sessions. Smallnet remains
outside web filtering and cookie policy.

### Titan submission (development)

`titan://` opens a preparation page, with separate links to view the resource
using Gemini or prepare an action. Navigation, bookmarks, history, session
restoration, reload and response redirects never submit content by themselves.
Incoming size, MIME and token parameters are discarded. The resource URL keeps
its query, while a newly approved intent places parameters before that query and
omits the fragment. Tokens and payloads stay transient.

Native FLTK controls offer text upload, file upload and deletion. Each action has
a separate final review identifying its destination, MIME type and exact byte
count. Empty text or files return to preparation; deletion is an explicit action.
Deletion also allows the resource MIME type to be selected, so servers can
distinguish deleting a text page from deleting a binary file.
The selected file must be a readable regular file and remain unchanged during
preparation and transfer. File uploads use bounded chunks without buffering the
whole file. Individual I/O stalls and shutdown are timed; a progressing upload
has no fixed total duration limit. Replacement navigation cancels its transfer.

Titan uses the existing verified TLS and shared PEM identity controls, with
scheme-specific pins and bindings. A server request for an identity or query
requires another explicit submission review before retrying. An interrupted
submission is not automatically retried. Gemini-format response documents and
downloads use the existing native rendering and save paths; redirect responses
offer a destination link that starts a new navigation.

`tests/titan.py` uses private TLS fixtures and native controls on X11 or Wayland.
Its `--reference-root` and `--perl-prefix` options run the unmodified App-Phoebe
4.07 server with a private wiki and user-local dependencies. No test uploads go
to public services. `tests/smallnet-profile.py --include-additional
--include-scorpion --include-titan` checks mixed restoration, including the
absence of a Titan submission. Smallnet remains outside web filtering and
cookie policy.


### Native read-only FSP archives

`fsp://host[:port]/path` uses FSP v2 over UDP (default port 21), with
status checks, directory flags/readme, complete directory blocks, file reads,
and session termination. Directory entries link to native resources; text and
images use the existing smallnet views. Binary files and files beyond the
preview limit use explicit native destination selection and bounded streaming.
File size and modification time are checked again before a transfer completes.

FSP has no query or fragment component. The optional URI user-information
component is the password (`fsp://password@host/path`), rather than an FTP-style
username/password pair. Passwords apply only to the current load and are removed
from saved browser URLs. The resource page also offers a native password form.
FSP transmits its password without encryption. It does not use TLS identities.
Tabs and browser processes coordinate per-server keys through private local
session leases; cancellation sends BYE and releases the lease. Other FSP client
families may use their own key coordination convention.

Primary specification: the FSP project's `doc/PROTOCOL`, FSP v2 document 0.19,
from <https://git.code.sf.net/p/fsp/code>. Native interoperability validation uses
the unmodified author daemon, a temporary archive, loopback UDP ports, and an
observer that forwards datagrams unchanged. No filesystem modification command
is issued by the browser. Web filtering remains scoped to web protocols.

```
uv run --no-project --with python-xlib --with pillow python ports/dome-fltk/tests/fsp.py \
  --binary /absolute/path/to/candidate --server-binary /absolute/path/to/fspd \
  --output /new/private/test-output --wayland
```


### Native WHOIS and RWhois directories

`whois://host[:43]/?q=Smith` sends a read-only WHOIS lookup (RFC 3912).
The path shorthand `whois://host/Smith` is normalized to that browser address.
Opening a server without a lookup displays native controls without connecting.
WHOIS replies finish at TCP EOF and retain their original bytes when saved.
Large replies offer a native streaming destination after the preview limit;
Save Source streams directly without the preview bound.

`rwhois://host[:4321]/auth-area=example.org?q=domain+example.org` combines
RFC 2167's historical authority-area referral address with this browser's
lookup parameter. Native RWhois 1.5 support negotiates the protocol, selects
the advertised dump display and result-limit capabilities, and requests
referral output rather than remote forwarding where that capability exists.
Records, typed object references, server information and partial results are
shown in an inert directory page. Referrals preserve the lookup and authority
area and are followed by explicit navigation, with a 16-hop bound. Changing
the lookup resets that bound. The authority area identifies the referral's
scope; the lookup itself supplies the server's search expression.

RWhois completion requires `%ok` or `%error`, including when the peer keeps
its connection open. Information blocks cannot terminate a result. Both
standard CRLF replies and the reference server's historical LF replies are
accepted; requests use CRLF. Save Source preserves the exact result bytes and
line endings, including result status and information blocks, rather than
reconstructing a transcript of the connection handshake. Large source saves
stream with bounded buffers and require a complete result terminator.

Lookup parameters (`q`, `encoding`, and the RWhois `hop` counter) are browser
address conventions; neither historical scheme currently appears in the
retrieved IANA URI registry. RFC 3912 does not define a WHOIS URI syntax.
Native forms offer UTF-8 or ISO-8859-1 request encoding for servers that need
it. Reply text uses valid UTF-8 or an ISO-8859-1 fallback unless the latter is
explicitly selected. Unsupported account or administrative operations are
not substituted for directory queries. Cancellation closes pending sockets;
forms, saved sessions, bookmarks and source downloads use the normal native
browser paths. Web filtering is scoped to web protocols.

Protocol definitions: <https://www.rfc-editor.org/rfc/rfc3912.html> and
<https://www.rfc-editor.org/rfc/rfc2167.html>. Interoperability tests use RFC
transcript fixtures and the unmodified Fedora-packaged ARIN server with an
indexed copy of its packaged sample data, exclusively on loopback sockets.
The server and indexer run as the regular user without system installation.

```
uv run --no-project --with python-xlib --with pillow python ports/dome-fltk/tests/whois.py \
  --binary /absolute/path/to/candidate --reference-prefix /private/extracted/rpm-prefix \
  --output /new/private/test-output --wayland
```


## Native renderer recovery checks

The focused fixture uses the separate `xr-native-input` test embedder. It serves
local pages, a system font and a webfont under a private profile, bus and
compositor. The current font fixture uses
`/usr/share/fonts/google-noto/NotoSansMono-Regular.ttf` and the base Fontconfig
configuration at `/etc/fonts/fonts.conf`.

```
uv run --no-project --with python-xlib --with pillow --with dbus-next python \
  ports/dome-fltk/tests/renderer-recovery.py \
  --binary /absolute/path/to/test-embedder --output /new/private/test-output
```

Add `--hardware` for the preferred accelerated adapter and `--webgl2` for WebGL 2.
Set `DOME_COMPOSITOR` to the private-fixture compositor binary when it is not
available at `../DomeD/target/release/dome-compositor`.

Each run checks three reconstruction cycles, a rejected replacement before each
successful recovery, resource-key continuity, live page state, visible pixels,
actual renderer selection and shutdown with the final context retired.

## Reporting lifecycle checks

Observer history is retained independently of pending endpoint delivery and is
bounded to the latest 100 reports per type. Endpoint delivery does not erase
buffered observation history or enqueue each report twice. Repeated `observe()`
registers once; `disconnect()` prevents new reports while queued notifications
still run. Worker removal releases the observer-list read borrow before mutation.

`Reporting-Endpoints` uses the engine structured-field parser, including quoted
commas, member parameters, repeated headers and duplicate names. Permissions
Policy state carries independent enforced and report-only inherited policies
and response-local reporting destinations. WebXR emits typed policy reports;
the two original reporting WPT assertion bodies now pass through the local
adapter. The existing CSP observer body is preserved. Wider Reporting API and
WebXR conformance remain incomplete.

Run the local delivery and lifecycle fixture against the `xr-native-input` test
embedder with a private DomeD compositor fixture:

```sh
DOME_COMPOSITOR=/path/to/private-compositor \
uv run --no-project --with python-xlib --with pillow --with dbus-next python \
  ports/dome-fltk/tests/reporting-observer.py \
  --binary /path/to/xr-native-input --output /tmp/reporting-lifecycle
```

The nine cases cover retained history, the buffer bound, queued disconnection,
record draining, repeated registration, filtering, one-time buffered observation,
callback mutation and worker disconnection/closure. They also require each of 124
CSP reports to reach its endpoint exactly once. This is scoped regression
coverage, not complete Reporting API conformance.

## WebXR policy reporting checks

Immersive availability checks and spatial-tracking session requests generate
permissions-policy diagnostics. Enforced policy can deny access; report-only
policy produces diagnostics without changing the access decision or bypassing
consent. Required and optional spatial features are checked, while viewer-only
inline sessions and policy introspection do not generate spatial-use reports.
Reports include feature, disposition and available script source location.

Internal report queues store typed Rust data. Each observer or `takeRecords()`
call receives fresh plain dictionaries with `type`, `url` and a concrete `body`.
Callback snapshots use the callback's realm, and page mutations cannot change
retained history or endpoint payloads. Delivery bookkeeping is internal. This
follows the current Reporting dictionary model; it does not claim legacy
Report/ReportBody interface or `toJSON()` compatibility.

```sh
DOME_COMPOSITOR=/path/to/private-compositor \
uv run --no-project --with python-xlib --with pillow --with dbus-next python \
  ports/dome-fltk/tests/permissions-policy-reporting.py \
  --binary /path/to/xr-native-input --output /tmp/policy-reporting

DOME_COMPOSITOR=/path/to/private-compositor \
uv run --no-project --with python-xlib --with pillow --with dbus-next python \
  ports/dome-fltk/tests/original-xr-reporting.py \
  --binary /path/to/xr-native-input --output /tmp/original-xr-reporting
```

The 21 local scenarios cover enforced/report-only precedence, response-local
endpoints, source locations, dictionary mutation, cross-realm callbacks, per-type
buffer limits and ordering, inline required/optional features, iframe inheritance
and navigation isolation. The original adapter preserves both WPT assertion
bodies and header files and verifies their digests. It is not the full WPT runner.

`webxr-context-compatibility.py --policy-report-only` adds the diagnostics observer
and report-only header to the existing private native-host test. Its original
consent, lease and stereo-submission assertions still apply, and the browser must
close cleanly. Broader reporting transport/lifecycle behavior and legacy interface
interoperability remain follow-up work. No physical headset conformance claim
is made by these fixtures.

## Iframe policy diagnostics

After each iframe load event, the embedding document checks the frame's current
attributes and declared origin for potential permissions-policy violations.
Enforced and report-only policies are evaluated independently, with enforcement
selected first. These diagnostics do not change the permission decision and do
not inspect the loaded child's response policy or redirect destination.

Potential reports use `potential-permissions-policy-violation` and include
`allowAttribute` and `srcAttribute`. Missing values remain null, empty values
remain empty strings, and observer/network snapshots preserve UTF-16 code units.
Ordinary feature-use reports omit these attribute fields. Endpoints belong to
the embedding document; child feature-use reports remain scoped to the child.

```sh
DOME_COMPOSITOR=/path/to/private-compositor \
uv run --no-project --with python-xlib --with pillow --with dbus-next python \
  ports/dome-fltk/tests/iframe-policy-reporting.py \
  --binary /path/to/xr-native-input --output /tmp/iframe-policy-reporting
```

The 23 local scenarios cover policy precedence, missing/empty/Unicode attributes,
load-handler changes, removal/adoption, declared versus redirected origins,
sandboxed origins, observer isolation and bounded history. The fixture acquires
no immersive device. Its XR feature cases are local regressions, not substituted
upstream camera tests or a complete Permissions Policy conformance suite.

## WebGL XR compatibility policy reports

Explicit WebGL 1 and WebGL 2 compatibility requests emit ordinary
`permissions-policy-violation` reports when restricted by enforced or report-only
`xr-spatial-tracking` policy. This covers initial context creation with
`xrCompatible: true` and each `makeXRCompatible()` call. Creation-time handling
runs after the context is rooted. Report-only policy retains the normal device
and compatibility checks; enforced restrictions keep their existing unavailable
behavior. Background device checks and policy introspection do not emit these
reports. Returning an existing context does not reapply creation attributes.

```sh
DOME_COMPOSITOR=/path/to/private-compositor \
uv run --no-project --with python-xlib --with pillow --with dbus-next python \
  ports/dome-fltk/tests/webgl-xr-policy-reporting.py \
  --binary /path/to/xr-native-input --output /tmp/webgl-xr-policy-reporting

DOME_COMPOSITOR=/path/to/private-compositor \
uv run --no-project --with python-xlib --with pillow --with dbus-next python \
  ports/dome-fltk/tests/webgl-xr-compatibility-wpt.py \
  --binary /path/to/xr-native-input --output /tmp/webgl-xr-compatibility-wpt
```

The 38 local policy scenarios cover both WebGL versions, enforced/report-only
precedence, observer-only and unknown endpoints, repeated context lookup,
ordinary creation controls, controlled context loss, source locations and exact
observer/network delivery. They use an inline-only registry with no immersive
device. The separate mock adapter runs six unchanged upstream cases for repeated,
concurrent and lost-context compatibility calls. This is selected regression
coverage; it does not include the upstream OffscreenCanvas creation cases or
claim full WebXR/WPT conformance.

## Gamepad policy reporting

Each explicit `navigator.getGamepads()` call checks enforced and report-only
`gamepad` policy and emits an actual-use diagnostic when restricted. Enforced
policy retains its `SecurityError`; report-only mode retains normal access and
the empty pre-gesture list. Inactive documents return an empty list before policy
reporting. Permission queries, policy introspection and device-event filtering
retain their non-reporting checks.

```sh
DOME_COMPOSITOR=/path/to/private-compositor \
uv run --no-project --with python-xlib --with pillow --with dbus-next python \
  ports/dome-fltk/tests/gamepad-policy-reporting.py \
  --binary /path/to/xr-native-input --output /tmp/gamepad-policy-reporting
```

The 14 local scenarios cover enforced/report-only precedence, exact observer and
endpoint counts, source locations, unknown endpoints, repeated polling, bounded
history, independent snapshots, inactive documents, cross-origin defaults and
non-reporting query/listener controls. They use separate profiles and no
controller gesture. Two unchanged original gamepad WPT pages cover advertised
policy support and blocked-frame listener registration through the local WPT
adapter. This is scoped regression coverage, not complete Gamepad conformance.

## Reporting delivery outcomes and request metadata

Reporting delivery handles successful responses, ordinary failures and HTTP 410
endpoint retirement on the owning document or worker. Endpoint identities keep
late results separate from another name, global or response configuration.
Observer history remains independent of delivery. The serialized User-Agent is
captured when each report is created.

Transport uses a null Fetch client, no window, no referrer, no service-worker
interception, CORS mode and same-origin credentials. Ancestor-aware cookie and
content-filtering context is carried separately, so this metadata correction
preserves third-party cookie controls and native adblock site exceptions.
Delivery remains best effort: attempted reports are consumed even on failure;
this change does not add a retry loop.

The contract follows the [Reporting draft delivery algorithm](https://w3c.github.io/reporting/#try-delivery).
`tests/reporting-delivery.py` covers response outcomes, creation-time metadata,
endpoint name/global isolation, worker termination, document removal and CORS.
`tests/reporting-filtering.py` checks native network/cosmetic filtering and a
trusted top-level exception. Run the latter with `--allowlisted` for that case.
Scoped validation totals are recorded in
`docs/validation-reporting-delivery-2026-10-29.json`. Geolocation implementation
and API tests are excluded. These checks do not establish whole-API or physical
headset conformance.

## XR session shutdown and callback lifetime

Active animation callback handles begin at one; requests after shutdown return
zero without retaining callbacks. Ending an already ended session rejects with
`InvalidStateError`. The first `end()` promise still completes after platform
teardown. Queued frame-rate and hit-test operations reject on shutdown, while
each concurrent frame-rate request retains its own result. Document deactivation
also releases a session whose explicit shutdown was waiting for a frame.

Callbacks already executing retain their prescribed order and cancellation
semantics. Input actions suppress new starts and selection after shutdown,
retaining cancellation of an action already started. The private mock device
honors both initial selection flags and releases ended sessions and their event
callbacks while the device stays connected. This adds no shipping device or
tracking capabilities.

The private `xr-native-input` example supplies opt-in logical-device and collection
controls. Run its regression drivers on isolated temporary profiles and a private
compositor:

```sh
uv run --no-project --with python-xlib --with pillow --with dbus-next python \
  ports/dome-fltk/tests/webxr-session-shutdown.py \
  --binary /path/to/xr-native-input --output /tmp/xr-session-shutdown
```

Use `--logical` with a different output directory for pending rate/hit-test
operations. `tests/webxr-session-retention.py` checks collection of ended inline
and mock sessions; `tests/webxr-session-wpt.py` runs five original session suites
with unchanged assertions and helpers through the local adapter. These fixtures
require the test embedder, rather than the shipping browser.

The scoped validation record is
`docs/validation-xr-session-lifecycle-2026-10-29.json`. It distinguishes current
WebXR units from retained network-unit results and unsupported optional cases.
The broader WebXR goal remains active; the user-accepted limited DomeD immersive
baseline is preserved. Geolocation implementation and API tests are excluded,
and its existing disabled shipping default remains unchanged.

## XR frame rendering eligibility

Animation callbacks wait until the session has a renderable layer. Inline
sessions also require an output canvas. Removing the layer pauses queued
callbacks; installing it resumes the uncancelled callbacks. An update made from
a callback takes effect at the next frame boundary, preserving the rest of the
current callback batch.

Pending render-state changes continue to merge and apply while animation
callbacks are paused. Native frames are still acknowledged so device events and
shutdown remain responsive. Renderability is checked separately from matching
the native frame's sub-images to the active layers.

`tests/webxr-render-state.py` checks these transitions with the private
`xr-native-input` example. It uses the same `--binary` and `--output` options as
the session-shutdown driver. `tests/webxr-render-state-wpt.py` runs five original
render-state suites with unchanged assertions and setup helpers, including both
inline and immersive no-layer cases. Two older inline-FOV setup failures remain
recorded: they call `makeXRCompatible()` with no immersive device, which the
[current WebXR algorithm](https://www.w3.org/TR/webxr/#dom-webglrenderingcontextbase-makexrcompatible)
requires to reject. `tests/webxr-inline-fov.py` checks
that rejection and ordinary inline FOV/rendering without the call for WebGL 1
and 2. Original assertions and failures are preserved. Use isolated temporary
profiles and a private compositor for these tests.

Scoped results and limitations are recorded in
`docs/validation-xr-render-state-2026-10-29.json`. Broader WebXR coverage and later
rendering work remain; these selected cases do not prove full conformance.
Geolocation implementation and API tests remain excluded and its disabled
shipping default is unchanged.

## XR transform values and transferable matrices

Transform positions, normalized orientations, inverses and reference-space
composition retain double precision until conversion to a native backend.
Quaternion normalization checks every component consistently and rejects a
non-normalizable result. The public matrix remains a 16-element `Float32Array`.
After its buffer is transferred, the next getter reconstructs the matrix from
the transform; repeated getters keep returning the attached cached array.

`tests/webxr-transform-values.py` checks value precision, normalization, inverse
identity, repeated matrix transfer and offset composition using the private
`xr-native-input` example. `tests/webxr-transform-core-wpt.py` adds twelve
original transform/pose/frame suites with unchanged assertions and setup
helpers. The older `tests/webxr-transform-wpt.py` remains a separate adapted
inline-setup regression; its results are not counted as original WPT cases.
These drivers accept `--binary` and `--output` and require isolated temporary
profiles and a private compositor.

The scoped record is `docs/validation-xr-transforms-2026-10-29.json`. Four
original setup failures remain explicitly recorded, including the two earlier
inline-FOV cases and two cases initialized with a null viewer-origin dictionary.
They are not counted as passing assertions. The valid tracking-loss diagnostic,
`tests/webxr-tracking-loss.py`, still reports seven genuine compatibility gaps
out of seventeen checks on both the prior candidate and this one. It uses
`clearViewerOrigin()` after valid initialization and checks restoration as well
as lost tracking. This diagnostic currently exits unsuccessfully; tracking-loss
pose relationships and emulation are the next implementation task.

The user-accepted limited DomeD immersive baseline is preserved. Broader WebXR
and later rendering/scroll-indicator work remain. Geolocation implementation
and API tests are excluded; its existing disabled shipping default is unchanged.


## WebXR request activation timing

Immersive session requests use the window's current transient activation state.
Deferred requests within that lifetime still reach the native consent dialog;
expiry, refusal and retry follow the same document/session lifecycle. The
existing opt-in XR application-launch mode has bounded XR intent without
establishing general HTML user activation, and document deactivation clears it.

`tests/webxr-activation-native.py BROWSER FIXTURE OUTPUT` drives trusted input,
consent, stereo frames and orderly shutdown in a private compositor/profile.
`tests/webxr-activation-launch.py EMBEDDER OUTPUT` validates the opt-in launch
path and canonical simulated activation with `XR_INPUT_SESSIONAVAILABLE_TEST=1`
and `XR_INPUT_STRICT_USER_ACTIVATION=1`. These settings belong to the separate
`xr-native-input` test embedder; normal browser defaults remain unchanged.

Validation: 124 unit tests and 1,362 runtime checks, including nine original
request-session and twelve original controller-action cases under strict
activation settings, three worker shutdown scenarios and anonymous Discord/X
smoke samples. Details are in
`docs/validation-xr-activation-2026-10-29.json`. Broader WebXR and later rendering
work remain; geolocation implementation and API testing are excluded and its
default remains disabled.


## Inline WebXR requested-feature intent and conversion

Default inline sessions remain available without activation. Explicit feature
requests follow the current draft's inline activation rule, including its
`inline-stereo` exception, before optional capabilities are filtered or implicit
viewer defaults are added. Feature lists use WebIDL `DOMString` sequences so
argument-conversion errors reject promises normally and leave later requests
usable. Native consent and document/session lifecycle checks are retained.

`XR_INPUT_STRICT_USER_ACTIVATION=1 tests/webxr-inline-intent.py EMBEDDER OUTPUT`
checks default and explicit features, conversion failures, expiry, deferred calls
and session recovery in the separate test embedder. The native `webxr-inline.py`
fixture uses genuine trusted input for feature capability checks and retains
its canvas projection, rendering, coexistence and shutdown assertions.

Validation: 124 unit tests and 1,472 passing runtime checks, including 98 focused
inline/conversion checks, 38 native inline checks, 18 original feature cases and
12 original controller cases under strict activation settings. Three worker
shutdown scenarios and anonymous Discord/X smoke samples also passed.
The unchanged upstream viewer-availability case expects explicit `viewer`
requests to bypass activation, while the current draft algorithm exempts only
`inline-stereo`. This selected-suite discrepancy is recorded separately and is
not counted as passing. See `docs/validation-xr-inline-intent-2026-10-29.json`.
Broader WebXR and later rendering work remain; geolocation implementation and
API testing remain excluded and its default stays disabled.


## WebXR visibility-mask events

`XRVisibilityMaskChangeEvent` exposes the session, eye, active-view index and
`Float32Array`/`Uint32Array` geometry, including constructor and SameObject
semantics. Provider events carry independent snapshots. The generic frame-update
path and headless provider deliver changed masks at coherent view boundaries,
respect secondary-view grants, and send empty geometry when a mask is removed.
Unchanged frames avoid duplicate events and geometry copies. Existing physical
DomeD providers report no masks and continue full-view rendering.

`XR_INPUT_STRICT_USER_ACTIVATION=1 tests/webxr-visibility-mask.py EMBEDDER OUTPUT`
checks event construction, array identity, snapshots, mask updates/removal,
secondary-view indices, inline/end behavior, and observed sub-frame input events.
The original visibility-mask suite passes under WebGL 1 and WebGL 2. The original
sub-frame suite remains unchanged, but its harness completion is not counted as
input-event proof because it does not return its watcher promise. The focused
fixture waits for every input event and verifies the current specified order,
source identity, active event-frame poses and expiry after dispatch.

Validation: 127 unit tests and 1,515 passing runtime checks, three worker shutdown
scenarios and reviewed anonymous Discord/X samples. The prior explicit-viewer
specification/test discrepancy remains recorded separately. See
`docs/validation-xr-visibility-mask-2026-10-29.json`. Broader WebXR and later
rendering work remain; geolocation implementation and API testing remain
excluded and its default stays disabled.


## Reproducible core WebXR audit

`tests/webxr-core-wpt.py EMBEDDER OUTPUT` runs the local top-level core WPT pages
with the separate optimized `xr-native-input` example in a private compositor,
profile and bus. Run it with the same Python dependencies and compositor/library
environment as the other XR test drivers. It preserves original assertion and
helper bodies, real response policy headers, and original test paths. Only the
vendor completion-reporting seam and transport are adapted. The command returns
1 whenever original cases do not pass; harness completion is recorded separately
and is never treated as a conformance pass.

On the unchanged b8dafc84175 implementation, 86 original pages completed with
262 reported passing cases and 16 non-passing cases: eight setup mismatches,
two specification/test disagreements, two disabled OffscreenCanvas cases and
four optional automatic viewport-hint cases. The WebDriver-dependent original
visibility page is listed separately. Its native minimize/restore/workspace/XR
visibility counterpart passes 77 checks, and no-headset inline rendering passes
30 checks. The promoted command reproduces every original case outcome.

See `docs/validation-xr-core-audit-2026-10-29.json` for the classification and
limits. The unreturned watcher in the original sub-frame case remains an
explicit evidence limitation; direct event-order and frame-lifetime checks are
retained in the visibility-mask validation. Current-specification interface
coverage, including tracked sources absent from the older local test snapshot,
still needs review. OffscreenCanvas remains disabled, and geolocation
implementation and API testing remain excluded with its default disabled.


## WebXR tracked sources and provider classification

`XRSession.trackedSources` is a separate live, SameObject collection, populated
only with a `tracked-sources` grant. `trackedsourceschange` reports additions,
removals and source replacements. Providers distinguish primary-action sources
from tracking-only sources; existing native metadata defaults to primary. A
native `HostInput` may supply `primary_action: false` to classify a tracker.
Tracking and auxiliary gamepad data remain available independently of primary
actions. Trackers with qualifying controls use an unmapped gamepad.

Role changes replace source objects, cancel held primary actions before removal,
and leave retained old spaces disconnected. Primary-source changes precede
tracked-source changes. End cleanup clears both collections. Native consent
preparation and session creation use the same feature negotiation. Headless
sessions snapshot existing active sources when registering under the device
lock; later changes remain buffered events. Repeated role assignments do not
stop the mock-device message loop.

`XR_INPUT_STRICT_USER_ACTIVATION=1 tests/webxr-tracked-sources.py EMBEDDER OUTPUT`
validates grants, policy, pose identity, metadata and role replacement, auxiliary
controls, disconnect/reconnect, cancellation and reentrant shutdown. Its
`primaryAction` initialization option and `setPrimaryAction()` are local test-only
extensions, guarded by the existing disabled shipping test-API preference.
`tests/webxr-tracked-native.py BROWSER FIXTURE OUTPUT` exercises the real native
backend using a synthetic host with a private metadata adapter. Set
`DOME_TRACKED_GRANT=0` for the ungranted case. The adapter creates no DOM events
and never connects to the desktop's XR socket. The focused
`tests/webxr-input-end-lifecycle.py EMBEDDER OUTPUT` checks end calls during all
four input-event phases using fresh devices.

Validation: 135 unit tests, 1,655 passing runtime checks, three worker-shutdown
scenarios, native granted/ungranted cases, and reviewed anonymous Discord/X
samples. An initial Discord asset-load failure produced a blank page; fresh
loads on both the baseline and candidate rendered the login form without those
errors. Filtering and transport protections stayed enabled.

The unchanged original core audit reports 260 passes and 18 nonpassing cases.
Sixteen earlier classifications remain. Two additional cases in the original
input-event shutdown fixture retain connected controllers across sessions but
expect only newly connected sources to cause addition events. Proper initial
source reporting exposes that setup assumption; these cases remain nonpassing.
Independent fresh-device shutdown checks pass. No original assertion is changed
or counted as a pass because a harness completed. See
`docs/validation-xr-tracked-sources-2026-10-29.json` for evidence and limits.
Physical tracker compatibility and full WebXR conformance are not established.
Further WebXR review and later rendering/scroll-indicator work remain.
Geolocation implementation and API testing remain excluded and disabled.


## Unique WebXR feature grants

Feature negotiation now returns each granted descriptor once, preserving its
first negotiated order. Required and optional repetitions share one grant;
unsupported required features still reject the request and unsupported optional
features remain omitted. Inline and immersive defaults retain their existing
mode checks. Request dictionaries are not rewritten, and repeated DOMString
conversions still occur before negotiation. The shared helper applies to the
current native, inline, headless, OpenXR and window providers.

`XR_INPUT_STRICT_USER_ACTIVATION=1 tests/webxr-feature-grants.py EMBEDDER OUTPUT`
checks duplicate descriptors, supported/default grants, unsupported features,
fresh frozen `enabledFeatures` arrays and conversion side effects. Validation
passes 138 targeted unit tests and 1,947 runtime checks: 1,664 fast regression
checks, five ended-session collection checks, and 278 checks in a 15-minute
cosmetic/privacy filtering session using bundled lists. All eight inline and
eight immersive session control objects were collected in the private explicit-GC
fixture. Three worker-shutdown scenarios and reviewed anonymous Discord/X samples
also pass with filtering enabled.

The sustained session closed normally. RSS grew by 29,042 KiB from its early
measurement window; later sampled medians were approximately flat. These timings
and memory samples are diagnostics, not a whole-browser bounded-memory claim.
The unchanged original core audit retains 260 reported passes and 18 classified
nonpassing cases. See `docs/validation-xr-feature-grants-2026-10-29.json` for
source, candidate and scope evidence. Further rendering/scroll-indicator
acceptance and installed-path checks remain; full device conformance is not
claimed. Geolocation implementation and API tests remain excluded and disabled.
