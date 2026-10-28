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

The development branch also routes backend-originated context loss through a
weak document/context-generation observer. Synchronous query failures mark loss
without requiring an additional JS context, return the operation's lost-context
result and invalidate the previous resource generation. Non-nullable resource
creators preserve the existing invalidated script-object behavior. Input
payloads and native reply sends tolerate cancellation, and native creation
releases resources if its requester disappears.

Loss events remain asynchronous. Driver-originated loss restores automatically
only after the event is canceled; explicit `WEBGL_lose_context` restoration keeps
its existing behavior. A notification for a retired generation cannot affect a
restored context. Persistent backend callbacks retain only a pipeline identifier,
context identifier and task route; event tasks retain their target for delivery.

The controlled regression uses the separate, non-installed testing embedder:

```sh
python3 ports/dome-fltk/tests/webgl-backend-loss.py \
  --binary /absolute/path/to/optimized/xr-native-input \
  --output /absolute/path/to/new-report-directory
```

The runner enables internal preference access only in that testing embedder.
The shipping browser does not enable that interface. Its default-off testing
preference requests a context-local backend state transition without resetting
the GPU. The fixture covers 64 interrupted query/creation operations, upload
cancellation, overlapping notifications, event permissions, restored pixels,
resource generations and departing frames. All run under software rendering.
The existing browser/Khronos and native XR lifecycle suites remain separate.

The library now exposes capability-gated reset queries and loss-aware retirement
on the Linux EGL backends. A context without reset notifications returns `None`,
not a healthy-reset result. Querying a different context temporarily selects it
and restores the previous binding. Retirement of an explicitly marked share
group rejects further normal context use, releases EGL handles without issuing
GL deletion commands and retains ownership when destruction needs a retry.
Other backends inherit an unsupported result rather than advertising recovery.

Validate these library paths separately with:

```sh
python3 ports/dome-fltk/tests/graphics-reset-retirement.py \
  --cargo /usr/bin/cargo --output /absolute/path/to/new-report-directory
```

The runner uses production policy modules and private offscreen native/software
contexts. Healthy native reset queries and context/display retirement establish
library behavior; they do not establish recovery from a physical GPU reset.
Display invalidation is confined to the probe subprocess's EGL connection.

Reset-query entry points and notification behavior follow
[KHR_robustness](https://registry.khronos.org/OpenGL/extensions/KHR/KHR_robustness.txt)
and the EGL context-creation extensions. Native resource retirement and creation
of a replacement context are separate steps: a reset context remains unusable
even after its reset-status query reports that reset processing has finished.

This library and routing work does not complete browser graphics recovery. The
remaining requirements are:

- Enable supported creation flags with an explicit compatible legacy fallback,
  then capability-gate the actual reset-status query.
- Harden native context/surface initialization failures during a real reset.
- Integrate the real query into the backend loop and retain resources until
  outstanding renderer ownership and native destruction have settled. Preserve
  the tested weak document/generation routing and completion of pending replies.
- Restore automatically only when the loss event permits it and reset has
  completed. Recreate native resources and keep previous handles invalid.
- Handle frontend renderer/context failure and adapter/capability changes.
  WebGL-only tests do not establish whole-browser recovery.
- Exercise controlled backend-loss cases and existing WebGL, upstream, XR,
  history and privacy regressions, followed by available native device tests.

Local runtime validation covers desktop OpenGL on Mesa Intel Arc A770 and
llvmpipe. GLES attribute routing has unit coverage; native GLES and non-EGL
platform runtime coverage remain unverified.
