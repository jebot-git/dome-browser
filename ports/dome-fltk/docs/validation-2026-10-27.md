# Browser validation, 27 October 2026

The optimized FLTK/Servo candidate passed 199 selected unit tests, 759 runtime
checks and 33 upstream cases across Pointer Events and WebXR. The native pen
suite contains 87 checks covering pressure/orientation, pending capture, capture replacement,
compatibility mouse targets, auxiliary buttons, cancellation and editable text
double-clicks. General browser checks passed with both the older toolkit and the
pen-enabled toolkit. Touch, mouse, controller, visibility and reference-space
reset regressions also passed. The unit total comprises 182 frontend tests,
three pen-payload tests, two pointer-ID allocator tests, five renderer
coordinate tests, and seven WebXR geometry/inline tests.

Frame-routing regressions cover 17 native checks for each of same-origin and
cross-origin frames, plus nine ordered-input completion checks. They cover
capture outside frame/page bounds, late capture, cross-document pointer
identity, retired-pointer cleanup, mixed pen/touch IDs, page zoom, transforms
and independent scrolling. Burst tests verify one completion per external
input, overflow cancellation/recovery and dispatch failure after frame removal.
See [native pen routing](PEN_ROUTING.md) for the implementation and fixtures.

Framebuffer scaling adds 147 native rendering checks across 18 scale/context
combinations, plus 21 bounded-space checks including new allocations after
viewport changes. The upstream totals comprise seven pen cases, 22 layer cases
and four reset cases. The layer runner supplies the native mock device's
scale-one fixture metadata expected by its Chromium-specific test-controller
field; original assertions and setup helpers remain unchanged. See
[WebXR framebuffer validation](XR_FRAMEBUFFERS.md) for allocation, state
preservation and pixel-validation scope.

The pen checkbox test now waits for its trusted click before checking
activation, and snapshots are selected in frame order. An initial assertion
ran while only pointerdown had arrived; the synchronized full suite passes.

Upstream assertions and test pages were unmodified. The test-only
`native-pen-wpt.py` adapter delivers pen actions through an isolated nested
Wayland compositor. It covers auxiliary clicks, overriding/releasing pending
capture, and repeated or unsuccessful capture requests. The compositor fixture
requires an explicit build feature and is excluded from normal builds.

Staged browser SHA-256:
`561c2baab1fee4f033188adf66c5ed8d4e1623ac93a117ba9934f14914f4902e`.
This identifies the tested local artifact; other build paths or dependencies may
produce a different hash.

Example commands (use new output directories):

```sh
uv run --no-project --with python-xlib python ports/dome-fltk/tests/native-pen-browser.py \
  --binary /absolute/path/to/candidate --compositor /absolute/path/to/input-fixture \
  --output /new/native-pen-output
uv run --no-project --with python-xlib python ports/dome-fltk/tests/native-pen-wpt.py \
  --binary /absolute/path/to/candidate --compositor /absolute/path/to/input-fixture \
  --output /new/upstream-pen-output
```

The pen callback is optional at runtime. Native pen validation requires DomeD's
FLTK pen patch and tablet-capable compositor; general browser operation remains
available with the older toolkit.

These are scoped synthetic native tests, not physical-device or full WebXR or
Pointer Events conformance claims. Broader auxiliary default actions and broader
standards coverage remain work in progress. Physical touchscreen/tablet and
WiVRn-Pro/controller validation remain required. The user-confirmed limited
immersive rendering baseline is preserved. This round staged and verified the
release binary without replacing the installed browser or restarting sessions.

Focused library tests use the browser's normal dependency feature configuration:

```sh
cargo test --release --locked -p servo-webxr-api -p dome-browser --lib geometry
cargo test --release --locked -p servo-paint -p dome-browser --lib pointer_ids
cargo test --release --locked -p servo-embedder-traits -p dome-browser --lib pen
cargo test --release --locked -p webrender --lib pipeline_point_tests
```
