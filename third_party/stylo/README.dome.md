# Dome's pinned Stylo source

This directory vendors the tracked source of https://github.com/servo/stylo at
`0ab78ce95162eca027c874b505b505ef17a85ac8` (the revision used by the integrated Servo snapshot).
The source is about 6 MB. Upstream license headers and component license files
are retained. `UPSTREAM_FILES.json` records SHA-256 hashes of the original files.

Local changes are limited to:

- `Cargo.toml`: remove `rustfmt.toml` from workspace members. It is a formatter
  configuration file, not a Cargo package; a local source workspace must load.
- `style/properties/longhands.toml`: keep `stop-color`, `stop-opacity` and `paint-order` enabled for
  Servo independently of the upstream experimental SVG preference. Their existing
  Color/Opacity/SVGPaintOrder parsers, CSSOM bindings, reset-property cascade,
  animation types and paint damage are retained. Other experimental SVG properties
  keep their upstream preference gates.

`dome.patch` is the complete delta from the pinned upstream source. Verify the
snapshot, including the local delta, with `python3 etc/verify_stylo_vendor.py`.
All eleven Stylo workspace packages are patched together by the root Cargo
manifest, so style values and selectors cannot split into Git/path crate types.
The root lockfile records the updated Stylo package versions as local paths.
Cargo's shared Git checkout is not modified.

SVG presentation-attribute parsing, computed-paint serialization and snapshot
invalidation live in Servo's `components/script` and `components/layout`.
