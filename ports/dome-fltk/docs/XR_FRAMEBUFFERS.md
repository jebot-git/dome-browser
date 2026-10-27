# WebXR framebuffer allocation and submission

Each compositor layer returns a `LayerInfo` containing its ID, allocated
framebuffer dimensions and full-sized eye viewports. This information travels
through both the session and WebGL threads. `XRWebGLLayer` reports that
allocation, rather than the session's unscaled recommendation.

The allocator clamps the requested scale before creating GL resources. Every
eye remains non-empty, dimensions respect the device/transport and GL limits,
and the common allocation policy caps each dimension at 4096 pixels. Width and
height scale separately and round upward to integer dimensions. Shared edges
are mapped with integer arithmetic, keeping asymmetric or odd-sized eye
viewports adjacent without overlap. New allocations use the device's current
viewport layout, including after mock-device or window viewport changes.

The DomeD adapter submits the allocation's actual eye rectangles with its
completed texture and native poses/FOVs. It converts OpenGL's bottom-origin
rows and rectangles to the transport's top-origin coordinates. It accepts
scaled layers instead of rejecting every scale other than one. A surface
whose dimensions differ from its recorded allocation is not submitted.

Layer allocation preserves texture binding state. XR attachment updates restore
the application's framebuffer bindings, including the default framebuffer and
separate WebGL 2 read/draw targets. Failed surface creation releases the depth
texture created for that allocation; failure to create the script framebuffer
releases its compositor layer.

Frame completion detaches both color and packed depth/stencil images, clears
script-side attachment ownership (including both WebGL 2 alias slots), and
restores the application's read/draw bindings. This internal operation bypasses
page-facing opaque-framebuffer restrictions. Pages still cannot change, inspect
or delete opaque attachments. The allocated dimensions remain authoritative
between frames, when the transient texture wrappers have been removed.

The sizing and viewport requirements follow
[WebXR Device API, XRWebGLLayer](https://www.w3.org/TR/2026/CRD-webxr-20260609/#xrwebgllayer-interface).
This milestone does not establish full WebXR layer conformance. Broader context
loss/compatibility behavior, negotiated attributes and
dynamic viewport scaling remain part of the continuing compatibility work.

## Validation

`tests/webxr-layer-scaling.py` runs the real browser and native DomeD bridge
against a private synthetic XR host. A recording relay gives the fixture
asymmetric 63/64-pixel eye widths, a 127-by-95 recommendation and bounded
transport limits. The tests request nine scales in each of WebGL 1 and WebGL 2,
replace layers and contexts, and inspect actual rendered buffers and submitted
pixels. Distinct application read/draw framebuffers and a non-default texture
unit check GL state continuity. Every eye's extent, color and vertical
orientation are checked against the recorded transport image.

Add `--lifecycle` to verify 54 frame transitions across the same 18 layer/scale
combinations. This mode alternates depth-only requests, stencil-only requests
and requests with neither attachment. It checks native depth/stencil bits
during rendering, backend attachment/detachment command ordering in both
contexts, WebGL 2 attachment sizes after cleanup, stable dimensions and
framebuffer availability, rejected page mutations/inspection/deletion, and ordinary
framebuffer detachment/reattachment controls in both WebGL versions. The native
WebGL 2 bit queries outside callbacks are an implementation cleanup regression,
not an additional WebXR conformance requirement. Legacy WebGL 1 driver bit
queries on an incomplete framebuffer may retain the last complete buffer's
values, so this mode checks cleanup through the backend trace for that context.

```sh
uv run --no-project --with python-xlib --with pillow \
  python ports/dome-fltk/tests/webxr-layer-scaling.py \
  --binary /absolute/path/to/candidate \
  --native-fixture /absolute/path/to/native-xr-fixture \
  --compositor /absolute/path/to/nested-input-fixture \
  --output /new/private/output
```

`tests/webxr-layer-wpt.py` runs six upstream layer pages through the separate
mock embedder. Original assertions and setup helpers are hashed and preserved.
Besides resource embedding and completion reporting, its test adapter supplies
`FakeXRDevice.defaultFramebufferScale_ = 1`: the upstream scale page reads this
Chromium-private fixture field, and the native mock backend uses equal native
and recommended resolutions. The adapter does not replace production WebXR
methods or alter allocation/viewport results. This is a focused runner rather
than the full WPT infrastructure.

The bounded-space regression also creates a new layer after changing device
view resolutions. General browser, native pen/touch, controller, mouse,
reference-space reset and visibility regressions cover the shared changes.
These isolated software-rendered tests do not establish physical headset or
tablet interoperability, or hardware acceleration performance.
