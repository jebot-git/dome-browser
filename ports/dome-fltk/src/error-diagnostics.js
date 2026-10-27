// Opt-in error observation: no API replacement, cancellation or page content capture.
(() => {
  'use strict';
  if (location.protocol !== 'https:' && location.protocol !== 'http:') return;
  const NativeError = Error;
  const emit = console.warn.bind(console);
  const encode = JSON.stringify.bind(JSON);
  const defer = typeof queueMicrotask === 'function' ? queueMicrotask.bind(globalThis) : callback => setTimeout(callback, 0);
  const observe = function domeErrorObserver(event) {
    try {
      const record = Object.create(null);
      record.kind = event.type;
      record.origin = location.origin;
      record.path = location.pathname;
      record.valueType = event.type === 'error' ? typeof event.error : typeof event.reason;
      if (event.type === 'error') {
        if (typeof event.message !== 'string') return; // Resource events have no exception.
        record.message = event.message.slice(0, 2048);
        record.filename = typeof event.filename === 'string' ? event.filename.slice(0, 2048) : null;
        record.line = event.lineno;
        record.column = event.colno;
      }
      const stack = new NativeError().stack;
      record.observerStack = typeof stack === 'string' ? stack.slice(0, 16384) : null;
      defer(() => {
        try {
          record.canceled = event.defaultPrevented;
          emit('DOME_ERROR_TRACE ' + encode(record));
        } catch (_) {}
      });
    } catch (_) {}
  };
  addEventListener('error', observe, true);
  addEventListener('unhandledrejection', observe, true);
})();
