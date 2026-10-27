# Native pen routing and pointer lifetime

Native pen samples are ordered per tool in Paint. Only one sample for each
tool is awaiting Script at a time. Script processes pending capture before
deciding whether its document can handle the sample. If capture is absent
and the actual hit belongs to another document, Paint retries the same input
in that document. The embedder receives one completion for the external
input; routing probes and release-hover updates stay internal.

WebRender supplies the current hit and the previous owner's unclipped
viewport coordinate from one rendered scene snapshot. Captured motion can
therefore leave an iframe or the page viewport without using stale positions.
Frame transforms, page scale and document scrolling participate in the
rendered coordinate conversion. Missing or non-invertible frame transforms
cancel the old stream rather than reusing a coordinate for a new target.

Pen and touch pointer IDs share a positive-ID allocator per WebView. Mouse
keeps ID zero. Constellation distributes pointer lifetime, button state and
active-document ownership to the WebView's documents, including frames
activated while a pointer exists. Capture from another document is ignored;
an unknown or retired pointer reports `NotFoundError`. Hover capture remains
a no-op when no buttons are active.

The routing queue allows 64 pen streams and up to 128 queued external samples
per stream, plus bounded internal cleanup.
Overflow completes dropped external inputs with dispatch failure, cancels the
gesture, and suppresses held input until release. It cannot activate a click.
Closing a target frame or its Script channel also completes forwarding with
dispatch failure. A released captured pointer restores actual-document hover
without requiring another hardware motion sample.

## Validation

`tests/native-pen-frames.py` drives the shipping browser through a private
native Wayland fixture. Run once normally and once with `--cross-origin`:

```sh
python3 ports/dome-fltk/tests/native-pen-frames.py \
  --binary "$DOME_BROWSER_CANDIDATE" --compositor "$DOME_TEST_COMPOSITOR" \
  --output "$DOME_TEST_OUTPUT"
```

Use the optional pen-capable FLTK library on `LD_LIBRARY_PATH`, as in the
existing native pen tests. The script verifies capture outside the frame and
page, cross-document hover and held motion, late capture, retired-pointer
cleanup, independent pen/touch IDs, transformed frames and scrolling.

`tests/pen-input-outcomes.py` uses the separate `xr-native-input` example to
submit ordered bursts and check every external completion. It checks routing
retries, internal release hover, overflow cancellation/recovery and removal of
the owner frame. Its `--binary` argument points to that example executable.
The example rejects duplicate or unexpected completions. It is not installed
and adds no input injection interface to the browser.

These are synthetic native input tests in isolated compositors. They do not
establish physical tablet, WiVRn-Pro or full WebXR conformance. Existing native
pen, upstream pen WPT, touch, controller, mouse, reset, visibility and general
browser regressions remain part of candidate validation.
