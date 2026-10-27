# XR device selection and context compatibility

Support queries, immersive session requests and WebGL compatibility checks use
one selected immersive device. A request does not silently fall through to a
different device after compatibility was checked against the selected one.
The existing available device is retained while any XR session is active.
When no session remains, discovery priority chooses the preferred available
device; simulated devices retain their existing front-of-list priority.
Viewer-only inline sessions do not enumerate or monitor immersive devices.

Discovery records have stable IDs, and native discovery also identifies the
current endpoint/runtime generation. Losing or replacing the selected device
advances the selection generation and ends active sessions. A closing session
stops pinning selection immediately, but device-change notification waits until
its backend has queued the session-end event. First-frame and later-frame
production failures also end the session and release its activity record.

WebGL 1 and 2 contexts retain the generation they were made compatible with.
The owning Window tracks them weakly, including contexts whose canvases are
detached, and observes selection changes while a context is compatible or a
compatibility request is pending. No XRSystem listener or getter is required.
A changed generation clears XR compatibility without losing the ordinary GL
context, textures or buffers. An unselected-device change does not clear valid
compatibility. Pending results cannot re-enable an old device generation or
overwrite a more recent completed compatibility request.

Observation is shared with XRSystem listeners and stops when the document is
inactive or all interests disappear. Context finalization queues a weak Window
rescan rather than invoking DOM code during collection. A retained history
document reconciles its contexts against a fresh selection baseline on resume,
without replaying device-change events from its inactive interval. Capability
observation opens no lease and requests no tracking.

This follows the selection and XR-compatible flag integration in the
[9 June 2026 WebXR draft](https://www.w3.org/TR/2026/CRD-webxr-20260609/).
`webxr-device-selection.py` covers mock selection, session retention, context
invalidation and notification ordering. `webxr-context-compatibility.py
--device-selection` covers native endpoint replacement and history restoration
with no XR event listeners. Registry units cover lifecycle interleavings,
viewer-only inline isolation and frame-production failures. Actual results and
retained failures are recorded in the milestone validation report.

Automatic graphics-adapter migration, real GPU-reset detection, optional backend
availability hooks and broader physical-device validation remain separate work.
