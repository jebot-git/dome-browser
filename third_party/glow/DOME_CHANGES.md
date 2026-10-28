# Dome Browser changes to glow 0.17.0

The original package archive has SHA-256
`29038e1c483364cc6bb3cf78feee1816002e127c331a1eec55a4d202b9e1adb5`.
All upstream files and licenses are retained. Changes use the upstream package
licenses (MIT OR Apache-2.0 OR Zlib).

- Add fallible native loader entry points and cached-version validation.
- Check required query entry points and missing metadata before using results.
- Keep implementation-defined vendor text usable without requiring UTF-8.
- Return an empty string for unavailable ordinary string-query results.
- Add exact native uint scalar queries using desktop double or GLES 3 int64
  queries, preserving high stencil-mask bits and legacy desktop GL support.
  Unsupported exact queries return None before callers change clear state.
- Add isolated native metadata fixtures; these issue no GPU reset.

The Servo workspace and standalone context-validation harnesses patch the same
version to this directory. Dependency versions are unchanged. Native EGL
integration and surface-allocation failure handling are tracked separately;
this does not establish whole-browser GPU recovery.
