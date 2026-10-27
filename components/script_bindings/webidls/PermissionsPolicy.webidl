/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// https://www.w3.org/TR/permissions-policy-1/#the-permissionspolicy-object
[Exposed=Window]
interface PermissionsPolicy {
  boolean allowsFeature(DOMString feature, optional DOMString origin);
  sequence<DOMString> features();
  sequence<DOMString> allowedFeatures();
  sequence<DOMString> getAllowlistForFeature(DOMString feature);
};

partial interface Document {
  [SameObject] readonly attribute PermissionsPolicy permissionsPolicy;
};

partial interface HTMLIFrameElement {
  [SameObject] readonly attribute PermissionsPolicy permissionsPolicy;
};
