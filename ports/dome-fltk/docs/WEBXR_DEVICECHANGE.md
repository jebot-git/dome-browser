# WebXR device availability

`XRSystem.ondevicechange` and `addEventListener('devicechange', ...)` receive
trusted generic events when immersive-device availability changes. Delivery is
restricted to fully active documents permitted to use `xr-spatial-tracking`.
The event has no user activation and does not grant consent to enter XR.

Observation starts with the first listener and stops with the last listener.
Duplicate, once, aborted and property listeners retain normal EventTarget
behavior. Navigation, history suspension and document destruction stop
observation; a retained history document resumes with a fresh baseline when
reactivated. Observer generations discard queued work from an earlier
subscription. Cross-thread callbacks retain document IDs and a task source,
without rooting the Window or XRSystem.

The native DomeD discovery uses a single lazy background worker. It samples
capability handshakes every 500 ms while an authorized document is observing,
with socket operation timeouts. Monitoring opens no immersive lease and reads
no tracking data. Initial registration does not manufacture a change event.
Socket removal, reconnection and replacement are detected. The additive
`hello.active` and `hello.availability_generation` fields distinguish runtime
suspension from ordinary frame publication and lease activity. Older protocol-v1
hosts remain compatible through endpoint and capability changes.

The mock backend follows the same event path. Inline-only devices do not change
immersive availability. Mock disconnection is serialized through the registry
with listener registration before reaching the device worker, so an immediate
disconnect cannot overtake a new observer. This testing API remains disabled in
the shipping browser.

The semantics follow the device-change event and Permissions Policy sections of
the [WebXR Device API](https://www.w3.org/TR/webxr/).

Validation consists of `webxr-devicechange.py`, the unchanged upstream
`xrDevice_disconnect_ends` assertions for both WebGL versions, and
`webxr-context-compatibility.py --devicechange` with DomeD's real bridge server
and private synthetic tracking. These fixtures verify event flags, multiple
documents, policy denial, listener cancellation, history suspension/restoration,
endpoint replacement and teardown. They do not establish physical headset
hotplug behavior or optional native backend support. See the milestone's
validation report for actual results and retained failures.

Broader device selection policy and resetting every WebGL context's XR
compatibility flag when the selected device changes remain follow-up work.
These availability tests alone do not establish that integration.
