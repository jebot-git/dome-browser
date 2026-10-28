/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// https://w3c.github.io/reporting/#interface-reporting-observer

dictionary ReportBody {
};

dictionary Report {
  required DOMString type;
  required DOMString url;
  // Concrete report-body dictionaries are converted separately so that their
  // derived fields survive conversion to this polymorphic output member.
  any body = null;
};

[Exposed=(Window,Worker)]
interface ReportingObserver {
  constructor(ReportingObserverCallback callback, optional ReportingObserverOptions options = {});
  undefined observe();
  undefined disconnect();
  ReportList takeRecords();
};

callback ReportingObserverCallback = undefined (sequence<Report> reports, ReportingObserver observer);

dictionary ReportingObserverOptions {
  sequence<DOMString> types;
  boolean buffered = false;
};

typedef sequence<Report> ReportList;
