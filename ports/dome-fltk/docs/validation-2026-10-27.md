# Browser validation, 27 October 2026

The optimized FLTK/Servo candidate passed 185 selected unit tests, 548 runtime
checks and seven upstream Pointer Events cases. The native pen suite contains
87 checks covering pressure/orientation, pending capture, capture replacement,
compatibility mouse targets, auxiliary buttons, cancellation and editable text
double-clicks. General browser checks passed with both the older toolkit and the
pen-enabled toolkit. Touch, mouse, controller, visibility and reference-space
reset regressions also passed.

Upstream assertions and test pages were unmodified. The test-only
`native-pen-wpt.py` adapter delivers pen actions through an isolated nested
Wayland compositor. It covers auxiliary clicks, overriding/releasing pending
capture, and repeated or unsuccessful capture requests. The compositor fixture
requires an explicit build feature and is excluded from normal builds.

Staged browser SHA-256:
`9e5b4314405a5c57251b6942f888244c437b94e9f8d415b97376e85dacb6c9e9`.
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
Pointer Events conformance claims. Capture across frames and viewport boundaries,
broader auxiliary default actions and broader standards coverage remain work in
progress. Physical touchscreen/tablet and WiVRn-Pro/controller validation remain
required. The user-confirmed limited immersive rendering baseline is preserved.
