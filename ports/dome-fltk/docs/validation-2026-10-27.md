# Browser validation, 27 October 2026

This record identifies the earlier context-compatibility candidate. The subsequent
[dynamic viewport validation](validation-dynamic-viewports-2026-10-27.json)
records the newer implementation and expanded regression scope.

The optimized FLTK/Servo candidate passed 200 selected unit tests, 3,131 runtime
checks and 33 upstream cases across Pointer Events and WebXR. The native pen
suite contains 87 checks covering pressure/orientation, pending capture, capture replacement,
compatibility mouse targets, auxiliary buttons, cancellation and editable text
double-clicks. General browser checks passed with both the older toolkit and the
pen-enabled toolkit. Touch, mouse, controller, visibility and reference-space
reset regressions also passed. The unit total comprises 182 frontend tests,
three pen-payload tests, two pointer-ID allocator tests, five renderer
coordinate tests, seven WebXR geometry/inline tests, and one XR factory
concurrency test. The concurrency test checks 256 request/result pairings
across 32 threads; it counts as one unit test.

Frame-routing regressions cover 17 native checks for each of same-origin and
cross-origin frames, plus nine ordered-input completion checks. They cover
capture outside frame/page bounds, late capture, cross-document pointer
identity, retired-pointer cleanup, mixed pen/touch IDs, page zoom, transforms
and independent scrolling. Burst tests verify one completion per external
input, overflow cancellation/recovery and dispatch failure after frame removal.
See [native pen routing](PEN_ROUTING.md) for the implementation and fixtures.

Framebuffer scaling and lifecycle coverage comprises 369 native rendering
checks, including the earlier 147 scaling checks across 18 scale/context
combinations. Layer attributes add 1,037 checks with a transparent canvas and
another 1,037 with an opaque canvas. These exercise RGB/RGBA backing, implicit
clears, attachment retirement and preserved WebGL state. There are also 21
bounded-space checks including new allocations after viewport changes.
The upstream totals comprise seven pen cases, 22 layer cases
and four reset cases. The layer runner supplies the native mock device's
scale-one fixture metadata expected by its Chromium-specific test-controller
field; original assertions and setup helpers remain unchanged. See
[WebXR framebuffer validation](XR_FRAMEBUFFERS.md) for allocation, state
preservation and pixel-validation scope.

Context compatibility adds 45 checks with an isolated native XR host and 31
without a host. Both WebGL versions exercise queued compatibility negotiation,
creation-time requests, native surface eligibility, preserved bindings and
resources, immersive layer rejection, and device availability changes. Native
surface checks do not acquire a session lease or begin tracking. Full WebGL
context loss/restoration and graphics-adapter migration remain unimplemented.

Earlier fixture corrections make pen assertions wait for the relevant trusted
click or completed tap cycles, and snapshots are selected in frame order.
Initial assertions ran before
their input sequence completed; the synchronized full suite passes. The
native relay skips an empty socket write after a payloadless request; all
unexpected relay errors remain fatal.

Upstream assertions and test pages were unmodified. The test-only
`native-pen-wpt.py` adapter delivers pen actions through an isolated nested
Wayland compositor. It covers auxiliary clicks, overriding/releasing pending
capture, and repeated or unsuccessful capture requests. The compositor fixture
requires an explicit build feature and is excluded from normal builds.

Staged browser SHA-256:
`3830475bb36458721411c2bcc6008fbd2531443b22d8cb11fa1d8f4d4e2b6e62`.
The separate test embedder SHA-256 is
`4204c1edc82b7b0215a9f59a92d111b2ef80743d2056005c232dfc975ce7f41b`.
The tested implementation is commit
`89592bb386053e13dc3e25173b8fee818e240ae9`. A fresh publication regression
round passed all 22 runtime/upstream jobs and six unit selections after
checking the staged binary hashes and the implementation's source manifest.
Its machine-readable summary is [validation-2026-10-27.json](validation-2026-10-27.json).
These hashes identify the tested local artifacts; other build paths or
dependencies may produce different hashes.

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
immersive rendering baseline is preserved. This round verified the existing
staged release binary without replacing the installed browser or restarting
sessions.

Focused library tests use the browser's normal dependency feature configuration:

```sh
cargo test --release --locked -p servo-webxr-api -p dome-browser --lib geometry
cargo test --release --locked -p servo-paint -p dome-browser --lib pointer_ids
cargo test --release --locked -p servo-embedder-traits -p dome-browser --lib pen
cargo test --release --locked -p servo-webgl -p dome-browser --lib concurrent_factories_stay_paired
cargo test --release --locked -p webrender --lib pipeline_point_tests
```
