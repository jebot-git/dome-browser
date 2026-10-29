/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// skip-unless CARGO_FEATURE_WEBXR
// https://www.w3.org/TR/webxr/#xrvisibilitymaskchangeevent
[SecureContext, Exposed=Window, Pref="dom_webxr_enabled"]
interface XRVisibilityMaskChangeEvent : Event {
  [Throws] constructor(DOMString type, XRVisibilityMaskChangeEventInit eventInitDict);
  [SameObject] readonly attribute XRSession session;
  readonly attribute XREye eye;
  readonly attribute unsigned long index;
  [SameObject] readonly attribute Float32Array vertices;
  [SameObject] readonly attribute Uint32Array indices;
};

dictionary XRVisibilityMaskChangeEventInit : EventInit {
  required XRSession session;
  required XREye eye;
  required unsigned long index;
  required Float32Array vertices;
  required Uint32Array indices;
};
