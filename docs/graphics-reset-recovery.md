# Graphics reset recovery

Driver-originated loss must use the existing WebGL context-loss lifecycle while
keeping the browser and XR host responsive. Explicit `WEBGL_lose_context`
restoration is already implemented; it is not proof of driver-reset recovery.

The current development branch adds the context-creation prerequisite in the
vendored Surfman 0.14.0. Callers can request supported EGL reset notification and
robust access. Actual descriptors retain each context's observed attributes;
shared contexts preserve their notification strategy. Default browser creation
has not enabled these flags yet.

Run the independent local context regression with:

```sh
python3 ports/dome-fltk/tests/graphics-context-creation.py \
  --cargo /usr/bin/cargo --output /absolute/path/to/new-report-directory
```

The runner performs optimized, locked/offline compilation after resolving its
small standalone harness from the existing lockfile. It runs seven tests from
the production EGL attribute module and four configurations on each of the
native/default and software renderers. It verifies actual reset/robustness
attributes, descriptors with different/no current context, borrowed native
imports with binding restoration, compatible/incompatible context sharing and
rendered-pixel readback. Original failed descriptor/binding regressions are
retained with the development artifacts. No GPU reset is requested.

This prerequisite does not complete browser graphics recovery. The remaining
requirements are:

- Enable supported creation flags with an explicit compatible legacy fallback,
  then capability-gate the actual reset-status query.
- Register weak document/context-generation loss notification without keeping
  a DOM root alive in the rendering backend.
- Complete or fail every pending synchronous command on loss. A lost backend
  must not strand a receiver or cause an abrupt browser exit.
- Deliver the established DOM loss lifecycle exactly once; invalidate old
  resources and XR compatibility consistently.
- Restore automatically only when the loss event permits it and reset has
  completed. Recreate native resources and keep previous handles invalid.
- Handle frontend renderer/context failure and adapter/capability changes.
  WebGL-only tests do not establish whole-browser recovery.
- Exercise controlled backend-loss cases and existing WebGL, upstream, XR,
  history and privacy regressions, followed by available native device tests.

Local runtime validation covers desktop OpenGL on Mesa Intel Arc A770 and
llvmpipe. GLES attribute routing has unit coverage; native GLES and non-EGL
platform runtime coverage remain unverified.
