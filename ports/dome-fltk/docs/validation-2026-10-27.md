# Browser validation, 27 October 2026

The optimized FLTK/Servo candidate passed 192 selected unit tests, 591 runtime
checks and seven upstream Pointer Events cases. The native pen suite contains
87 checks covering pressure/orientation, pending capture, capture replacement,
compatibility mouse targets, auxiliary buttons, cancellation and editable text
double-clicks. General browser checks passed with both the older toolkit and the
pen-enabled toolkit. Touch, mouse, controller, visibility and reference-space
reset regressions also passed. The unit total comprises 182 frontend tests,
three pen-payload tests, two pointer-ID allocator tests and five renderer
coordinate tests.

The current round adds 17 native frame checks for each of same-origin and
cross-origin frames, plus nine ordered-input completion checks. They cover
capture outside frame/page bounds, late capture, cross-document pointer
identity, retired-pointer cleanup, mixed pen/touch IDs, page zoom, transforms
and independent scrolling. Burst tests verify one completion per external
input, overflow cancellation/recovery and dispatch failure after frame removal.
See [native pen routing](PEN_ROUTING.md) for the implementation and fixtures.

Upstream assertions and test pages were unmodified. The test-only
`native-pen-wpt.py` adapter delivers pen actions through an isolated nested
Wayland compositor. It covers auxiliary clicks, overriding/releasing pending
capture, and repeated or unsuccessful capture requests. The compositor fixture
requires an explicit build feature and is excluded from normal builds.

Staged browser SHA-256:
`cd9432ef98eb9dffba0d88a787733bbf47356927576c0a5d262a0aba4c913b4e`.
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
cargo test --release --locked -p servo-paint -p dome-browser --lib pointer_ids
cargo test --release --locked -p servo-embedder-traits -p dome-browser --lib pen
cargo test --release --locked -p webrender --lib pipeline_point_tests
```
