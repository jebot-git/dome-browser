/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// https://w3c.github.io/webappsec-permissions-policy/#reporting
// Reporting API currently defines ReportBody as an abstract dictionary. Keep
// that representation, rather than migrating the existing CSP dictionary to
// the historical interface hierarchy still referenced by Permissions Policy.
dictionary PermissionsPolicyViolationReportBody : ReportBody {
  required DOMString featureId;
  DOMString? sourceFile = null;
  unsigned long? lineNumber = null;
  unsigned long? columnNumber = null;
  required DOMString disposition;
  DOMString? allowAttribute;
  DOMString? srcAttribute;
};
