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
The registry queues session preparation separately from backend creation. The
DomeD backend opens a native asynchronous consent window, so the normal browser
loop continues processing navigation, document cancellation, queries and inline
sessions. Only one immersive preparation is presented at once. Pending requests
remain bound to their selected device generation and document generation;
cancellation, denial and device replacement release their preparation state before another
request proceeds. An accepted request is revalidated before backend creation,
including a runtime identity change within the same discovery entry. Closing a prompt defers the next
immersive preparation until the browser loop has consumed focus restoration. A
deferred live request schedules another iteration without continuously waking
while consent is simply waiting for an answer. Native window keyboard ownership
is tracked separately from the remembered focused widget; reports are synchronized
after native focus transitions. A following consent may wait up to one second
for the same tab to regain real keyboard/page focus after an owned prompt closes.
It never bypasses that focus check; the same tab must be the active native owner
whenever preparation is polled.

`webxr-context-compatibility.py --pending-sessions` requires removed requesting
frames to leave no prompt awaiting an answer and open no native lease. A fresh
accepted request must still work afterward. `--consent-cancellation` exercises removal and
navigation while consent is already displayed, retained history, explicit denial,
a queued live request after cancellation, and a fresh accepted request. These use
the real DomeD bridge server with a private continuously published synthetic host; they do not establish physical headset
behavior. Registry units additionally cover queue cancellation, generation scope,
responsive queries/inline sessions, denial, device replacement and inline fallback.

The document lifecycle requirements are checked against the
[WebXR Device API](https://www.w3.org/TR/webxr/). Actual test results and retained
baseline failures are recorded in the milestone validation report.
