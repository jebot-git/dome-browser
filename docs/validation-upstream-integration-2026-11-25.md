# Servo integration validation — 2026-11-25

Integration branch: `maintenance/upstream-20261123`.
Dome parent: `0b613c820199f898ab83870351456a39dfaaa1ef`.
Upstream Servo parent: `ff999cf0349f2aff2ef60d791ddc88f6c145599d` (617 commits).

The workspace defaults to the Dome FLTK frontend. The integration retains native Wayland, bitmap-only font selection and
system HarfBuzz, DomeD/XR consent and device observation, context-loss retirement,
persistent privacy-aware cookies/cache, SVG snapshot completion, media-source
lifetime ownership, and script FontFace cleanup. Native BDF/PCF faces can be
constructed without an SFNT/Skrifa face. The shared Skrifa cache retains only
local fonts so canceled web and deleted script faces release their data.

Dependencies resolve with all 17 local patched packages. Stylo is rebased to
`0ab78ce95162eca027c874b505b505ef17a85ac8`; its local SVG property exposure and
workspace patches remain. The local WebRender fork includes the exact 0.70.1
delta and retains its Dome patches. SpiderMonkey is `mozjs 0.27.0` /
`mozjs_sys 153.7.0-0`, with five glue crates at 153.7.0. The pinned Rust toolchain is 1.99.1.

SVG dependency policy review: usvg/resvg 0.50 transitively depends on HarfRust.
The image cache still supplies an empty font database and custom
`SvgFontResolver` callbacks. These import only faces selected and opened by
Dome's bitmap-only `FontContext`; the native FreeType loader independently rejects
non-BDF/PCF and scalable faces. The usvg library does not load system fonts or
embedded font assets. Its CLI-only system-font loading path is not used.
Core `servo-fonts` enables HarfBuzz; HarfRust and outline-fonts remain opt-ins.
Neither bundled FreeType nor an outline fallback is enabled by the Dome feature graph.

Checks completed using Fedora Rust 1.100.0:

- All original conflict markers removed; targeted Rust/Python syntax checks pass.
- Targeted staged source whitespace checks pass.
- Locked Linux dependency metadata and Dome feature-tree checks pass.
- Stylo vendor verification passes for 399 files; HTML parser verification passes
  for 45 files. Two previously ignored, inventoried Cargo.toml.orig files were
  restored from exact package tarballs and hash-verified.
- `cargo check --locked -p servo-fonts -p servo-webgl --features servo-webgl/webxr`
  passes with two jobs and the system bitmap SDK.

- `cargo test --locked -p servo-fonts --features harfrust --test font
  native_bitmap_face_without_opentype_tables -- --exact` passes (1 passed).
  It constructs a BDF without SFNT tables, verifies its native 7-pixel strike and
  baseline fallback, and shapes at the 6-pixel advance through native HarfBuzz
  even when the optional HarfRust backend is compiled.

- `cargo check --locked -p dome-browser` passes for the complete FLTK frontend
  with two jobs and the system bitmap SDK. A staged GStreamer 1.28.9 development
  SDK matches the installed runtime. Native queued wheel events carry their
  modifier snapshot into the new upstream wheel constructor.

- `cargo test --locked -p servo-script-bindings --features
  gamepad,subtlecrypto,webgl,webxr,js/jit --lib
  atom_code_units_preserve_utf16_without_converting_storage` passes (1 passed).
  All four atom representations retain their storage and supplementary Unicode
  characters convert to the correct UTF-16 surrogate pair.

Runtime desktop, physical XR, authentication, media and privacy regressions have
not yet been run against this integration revision. The source has not been installed or published by this
integration worker.
