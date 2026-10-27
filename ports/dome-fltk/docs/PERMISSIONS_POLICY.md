# Document permissions policy

Dome uses document Permissions Policy for `xr-spatial-tracking`, `geolocation`
and `gamepad`. WebXR and geolocation default to same-origin delegation;
gamepad retains its wildcard default. A policy grant remains separate from
user consent and device availability.

The implementation parses the response's structured `Permissions-Policy`
dictionary and the iframe's `allow` attribute. Navigation captures the parent
policy and the declared source origin; the destination document computes its
inherited policy against its actual final origin. This includes redirects,
opaque sandbox origins, srcdoc, and child-initiated navigation. A destination's
header or an iframe attribute cannot re-enable an inherited restriction.
Changing `allow` updates iframe introspection immediately; the loaded document
retains its policy until the next navigation. The parent-owned container snapshot
also survives document eviction. Reloading a discarded history entry uses the
current iframe configuration, including grants or restrictions changed while
away; destroying the browsing context removes its cached snapshot.

`document.permissionsPolicy` and `iframe.permissionsPolicy` provide the
`PermissionsPolicy` methods. Frame introspection describes the embedding
configuration rather than exposing the loaded child's response policy. The
feature list contains the three registered features above. Unknown features
are not advertised as enforced. The legacy `Feature-Policy` header and
`featurePolicy` aliases are not implemented.

For a document without spatial-tracking permission:

- Inline support queries remain true, and viewer-only sessions remain usable.
- Immersive support queries reject with `SecurityError`.
- Required spatial features reject with `NotSupportedError`; optional spatial
  features are omitted.
- The WebGL creation attribute `xrCompatible` has no effect.
- `makeXRCompatible()` resolves without selecting an XR device or changing the
  context's compatibility flag. Ordinary graphics remain usable.

The compatibility behavior follows section 11.3 of the
[9 June 2026 WebXR draft](https://www.w3.org/TR/2026/CRD-webxr-20260609/).
The upstream WPT `webxr_permissions_policy.https.html` still expects rejection
from `makeXRCompatible()`. Its original assertions are retained in validation,
with that obsolete expectation reported separately rather than counted as a
pass.

Policy parsing and inheritance follow the
[26 October 2026 Permissions Policy draft](https://www.w3.org/TR/2026/WD-permissions-policy-1-20261026/).
Source expressions use the existing CSP source matcher; opaque origin identity
is preserved rather than comparing the serialization `null`.

Validation uses isolated profiles, private native Wayland surfaces, loopback
HTTP origins, and synthetic XR devices. `webxr-permissions-policy.py` covers
header and attribute declarations, nested frames, redirects, opaque origins,
and dynamic navigation. Its `--history-eviction` mode makes 25 child navigations
per scenario, verifies a fresh request for the evicted entry, and covers unchanged
restrictions, newly delegated access, and newly applied restrictions.
`webxr-context-compatibility.py --policy-denied`
checks the shipping browser with a live private XR host available and verifies
that policy-denied calls do not contact it. The original positive context and
immersive-recovery suites remain regression controls. Repeated and concurrent
mock-device disconnection and subsequent cleanup must all settle; the production
browser continues to leave the mock API disabled.

Positive WebXR availability notifications are described separately in
[WebXR device availability](WEBXR_DEVICECHANGE.md), including observer lifetime
and native bridge behavior. A passing denied-devicechange WPT case alone does
not establish positive event support.

Remaining work includes Permissions Policy reporting / report-only delivery,
additional feature registrations and their API integrations. Automatic GPU-reset recovery,
graphics-adapter migration and broader physical-device compliance validation
also remain outside this milestone. OffscreenCanvas is not advertised by the
shipping browser and is not covered by these policy checks.
