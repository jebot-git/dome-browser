# XR request lifetime

Session requests carry the requesting pipeline and that document activation's
generation. Navigation, history suspension and destruction cancel backend
sessions owned by that generation, including responses which have not produced
a DOM XRSession yet. Cancellation uses the registry's existing FIFO and retires
with its session records; no permanent map of closed documents is retained.

Returned sessions are wrapped in a Rust response guard after transport. If the
document task is cancelled, dropped or cannot be queued, the guard aborts its
unadopted session. Successful adoption consumes the guard. Plain Session values
remain serializable without a destructor that would abort a session merely
because it crossed a process boundary.

Response adoption checks both document activity and the captured activation
generation. A stale response rejects with InvalidStateError and cannot clear a
newer request's pending state. Deactivation clears stored active-session state
and marks immersive layers dirty. Already queued end events cannot remove a
newer active session. Completed backend activity is pruned even when no
device-change notification is pending.

Failed response delivery aborts the corresponding backend session. Request
callbacks move their DOM roots into their one response task instead of retaining
another root in the callback. Viewer-only inline requests keep their existing
sensor-free behavior.

`webxr-pending-session.py` covers pending and completed child removal, navigation
and retained-history restoration for inline and immersive modes. Registry units
cover response-guard abandonment/adoption, document-generation isolation and
cancellation while a response remains outside the DOM.
`webxr-context-compatibility.py --pending-sessions` uses the real DomeD bridge
server with continuous synthetic frame publication, verifies native request
cleanup after frame removal, and then creates and ends a fresh session. It
deliberately allows an in-flight consent dialog to complete before checking
cleanup. Dismissing a pending native consent dialog automatically on navigation
remains separate follow-up work. These fixtures do not establish physical
headset behavior.

The document lifecycle requirements are checked against the
[WebXR Device API](https://www.w3.org/TR/webxr/). Actual test results and retained
baseline failures are recorded in the milestone validation report.
