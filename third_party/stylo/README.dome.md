# Dome's pinned Stylo source

This directory vendors the tracked source of https://github.com/servo/stylo at
`633f43ad86de1372fd7106d265c00ffcdf022c35` (the revision already used by Dome).
The source is about 5.8 MB. Upstream license headers and component license files
are retained. `UPSTREAM_FILES.json` records SHA-256 hashes of the original files.

Local changes are limited to:

- `Cargo.toml`: remove `rustfmt.toml` from workspace members. It is a formatter
  configuration file, not a Cargo package; a local source workspace must load.
- `style/properties/longhands.toml`: expose `stop-color`, `stop-opacity` and `paint-order` to
  Servo as well as Gecko. Their existing Color/Opacity/SVGPaintOrder parsers, CSSOM bindings,
  reset-property cascade, animation types and paint damage are retained.

`dome.patch` is the complete delta from the pinned upstream source. Verify the
snapshot, including the local delta, with `python3 etc/verify_stylo_vendor.py`.
All eleven Stylo workspace packages are patched together by the root Cargo
manifest, so style values and selectors cannot split into Git/path crate types.
The root lockfile changes only those package source locations; dependency
versions remain unchanged. Cargo's shared Git checkout is not modified.

SVG presentation-attribute parsing, computed-paint serialization and snapshot
invalidation live in Servo's `components/script` and `components/layout`.
