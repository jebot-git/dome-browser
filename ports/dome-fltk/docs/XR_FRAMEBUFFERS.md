# WebXR framebuffer allocation and submission

Each compositor layer returns a `LayerInfo` containing its ID, allocated
framebuffer dimensions, full-sized eye viewports and actual buffer attributes. This information travels
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
loss/compatibility behavior remains part of the continuing compatibility work.

## Dynamic viewports

`XRView.requestViewportScale()` changes the view's rendering region without
reallocating its framebuffer. Requested and current scales belong to the
underlying session view, so they survive new `XRView` wrappers, different
reference spaces, animation frames and base-layer replacement. Each view has
an `index` corresponding to its position in the pose's views array.

The first `getViewport()` query for a view in an animation frame commits its
requested scale and locks it for that frame. Later requests take effect on
the next frame's first query. Null, undefined and nonpositive requests are
ignored; values above one clamp to one. WebIDL rejects nonfinite inputs.
Dimensions round downward with a one-pixel minimum, preserving each eye's
full-sized origin. A changed scale creates a new viewport object even if its
dimensions round identically. Repeated queries keep the same object for the
remainder of the frame, including after an inline canvas resize. An unchanged
scale and rectangle retain the object across frames. Retained views can
request a future scale, but viewport queries
reject inactive frames or views from another session with `InvalidStateError`.

Inline sessions and DomeD sessions support these requests. The mock device
also supports scaling for standards tests. Other device implementations keep
their full-sized viewports until their presentation paths opt in. At frame
completion, DomeD receives the exact scaled rectangles used for drawing; it
validates them against the original allocation and converts their vertical
origins for native submission. The allocation and full-sized eye rectangles
remain unchanged. No scale-recommendation heuristic is implemented, so
`recommendedViewportScale` returns null.

These rules follow the [WebXR viewport scaling algorithms](https://www.w3.org/TR/2026/CRD-webxr-20260609/#dom-xrview-requestviewportscale).

`tests/webxr-layer-scaling.py --dynamic` exercises both WebGL versions,
asymmetric eyes, two allocation scales, independent eye requests, frame
locking, persistence, tiny scales, invalid input, retained views and inline
sessions. Native captures compare submitted rectangles and pixels, including
orientation and the unused area beside reduced viewports. The dedicated
upstream scaling page runs with `tests/webxr-layer-wpt.py --dynamic-only`.
Its four optional recommendation cases are reported separately; all scaling
assertions must pass. Secondary-view behavior has its own fixture below.

The [27 October validation record](validation-dynamic-viewports-2026-10-27.json)
contains the tested source and binary hashes: 203 selected unit tests, 3,747
runtime checks and 57 passing upstream cases across 24 accepted regression
suites, with four optional recommendation cases unsupported. The 610 viewport
checks include 366 API assertions and native rectangle/pixel validation.
The mouse fixture waits for the complete event-report sequence before checking
selection counts; HTTP delivery order does not determine browser event order.
This remains scoped software/native-fixture validation, with physical-device
and optional-backend validation still outstanding.

## Secondary views and changing view sets

The mock backend preserves each view's eye, projection, transform and
`isFirstPersonObserver` metadata. It accepts arbitrary primary and secondary
view counts, including repeated eye labels. Secondary views appear only in an
immersive session that was granted `secondary-views`; inline sessions retain
their single primary view. Updating primary views while omitting secondary
views retains the latter, while an explicit empty secondary list clears them.
Invalid combined updates leave both sets unchanged.

View IDs remain stable while a view is retained. Scale requests follow those
IDs even when their array index changes; removed views cannot affect replacement
views. New layouts pack asymmetric view widths and fit their rectangles within
the layer's existing framebuffer allocation. Viewport caches include view
identity, avoiding reuse for a replacement view with identical geometry.

`tests/webxr-secondary-views.py` checks feature gating, metadata, changes of
view count, retained and replaced identities, independent scaling and GPU pixel
readback in both WebGL versions. `tests/webxr-layer-wpt.py --secondary-only`
runs the upstream secondary-view and viewport pages without replacing their
assertions. These follow the [WebXR Test API view-update rules](https://immersive-web.github.io/webxr-test-api/).
The [combined validation record](validation-secondary-views-history-pdf-2026-10-27.json)
records 209 selected units, 4,545 runtime checks, 69 upstream passes and three
worker-shutdown cases across 34 regression suites. Four optional viewport-scale
recommendation cases remain unsupported. History and PDF handoff were each
validated with both available FLTK runtime versions.

This mock-device coverage does not establish additional physical-camera or
headset interoperability. Context loss, Permissions Policy and broader
physical-device validation remain continuing work.

## Context eligibility

Every context starts with `xrCompatible=false`. A creation request with
`xrCompatible:true` asks the backend to check its surface-transfer path before
reporting compatibility. `makeXRCompatible()` performs the same check through
the XR registry and completes its promise on a subsequent document task.
Concurrent calls receive distinct promises. Factory dispatch keeps its paired
factory/command messages ordered across XR and compatibility threads. Immersive layers reject contexts
whose actual compatibility flag is false; inline layers use their existing
canvas and do not need this flag.

For DomeD and GL-window backends, the check imports a small independent
producer surface into a temporary context on the compositor's adapter and
checks that its framebuffer is complete. It does not open an XR session,
acquire a headset lease, read tracking data or copy application pixels. The
probe disposes of its temporary native objects and restores the producer as
the WebGL thread's current context. Layer allocation and frame transitions
also preserve native bindings across Surfman operations, so asynchronous XR
work cannot change the framebuffer observed by an application query. Mock devices check producer surface
allocation without claiming external adapter interoperability. The optional
Windows OpenXR path selects its graphics-requirements adapter for the import
check; that source path still needs Windows compilation/runtime validation.

An unavailable immersive device rejects negotiation with `InvalidStateError`
and clears an earlier compatibility flag. A device can become available again
without replacing existing WebGL resources. Adapter migration and complete
WebGL loss/restoration remain unfinished: this milestone does not add the
`WEBGL_lose_context` extension or claim resource invalidation, reset and
restoration events. Full Permissions Policy integration also remains part of
the broader standards work.

The eligibility and task-ordering requirements follow
[WebXR context compatibility](https://www.w3.org/TR/2026/CRD-webxr-20260609/#webgl-context-compatibility).

## Buffer formats and frame initialization

The Surfman/DomeD path uses single-sample images and reports `antialias=false`.
It reports `ignoreDepthValues=true` because the compositor receives color and
poses without depth. Inline layers use the context's actual default-buffer
antialias setting and keep its existing canvas allocation; layer-init options
do not change that default buffer.

Layers with alpha render directly into the imported RGBA surface. A layer with
`alpha=false` renders into an RGB texture, then copies it on the GPU into the
native RGBA presentation surface. Readback from RGB returns alpha one; the
DomeD submission uses an opaque blend flag. RGBA layers retain their
premultiplied color and alpha values and use the source-alpha blend flag. The
copy preserves read/draw framebuffer bindings, scissor state and desktop sRGB
write state. Allocation preserves texture-unit bindings and pixel-unpack
buffers, so a bound application upload buffer is not read during allocation.
Depth/stencil requests use packed storage when either component is requested.

Before each frame, the layer clears color to zero, depth to one and stencil to
zero, including when the context requests `preserveDrawingBuffer`. Clearing
restores application clear values, masks and separate front/back stencil
masks. Cached FBO references to imported surfaces are released before Surfman
recycles those textures. Reusing a texture name therefore cannot make the
next frame clear an older GL object. Failed FBO creation or incomplete storage
returns an error instead of falling back to clearing the default framebuffer.

The optional Windows OpenXR adapter uses the same color-storage and copy
helpers and single-sample imported images. This source path has not been
compiled or exercised by the Linux validation; it does not have a new Windows
runtime interoperability claim.

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

Add `--attributes` instead of `--lifecycle` to exercise 32 immersive layer
configurations across WebGL 1 and 2, each for three frames. This fixture reads
the initial buffer contents, draws through cleared depth/stencil storage,
checks RGB/RGBA pixels and native compositor blend flags, and preserves a
non-default texture unit and a small bound WebGL 2 upload buffer. Zero write
masks and deliberately unsuitable clear values carry across frames; an
explicit masked clear verifies that implicit clearing restored the masks.
Four inline cases compare layer properties with actual context attributes.
Use `--attributes --opaque-context` for the same matrix with context alpha and
premultiplication disabled. XR buffer formats and premultiplied submission
remain independent of those default-buffer settings.

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

`tests/webxr-context-compatibility.py` exercises native device presence and
absence, synchronous creation requests, queued/concurrent negotiation,
per-context flags, immersive rejection/acceptance, reconnects and preservation
of real textures, pixel contents and distinct WebGL 2 read/draw bindings. Its
private host records operations to verify that negotiation itself never opens
a lease or asks for tracking. Add `--no-xr-host` to exercise rejection and
sensor-free inline operation without an immersive device.
