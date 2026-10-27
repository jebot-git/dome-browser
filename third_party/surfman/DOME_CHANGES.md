Surfman 0.14.0, copied from the crates.io package pinned by the browser lockfile.
Upstream: https://github.com/servo/surfman
Package checksum: 7fe194717def845ecb64e7aeda3e2108c4b3b1584a01aa6e545e702cedca2a00
All original source and license files are retained. Cargo cache marker files are
omitted. The workspace patches this package without changing dependency versions.

Dome additions:
- Opt-in RESET_NOTIFICATION and ROBUST_ACCESS context flags on EGL. Default
  context creation attributes are unchanged. Requested unsupported attributes
  return an error; callers must implement their own deliberate fallback.
- EGL 1.5 uses core attributes for both OpenGL and GLES. On EGL 1.4, KHR
  robustness attributes are restricted to desktop OpenGL; EXT attributes are
  restricted to GLES. Exact advertised extension tokens are required.
- Actual GL attributes are captured while initializing each context. Descriptor
  lookup subsequently uses that context's cached descriptor, even if another
  context, or no context, is current. Shared creation retains the actual strategy.
- Borrowed native contexts are temporarily bound while capturing attributes;
  the previous binding, including its absence, is restored afterward.
- BindAPI failure returns a creation error rather than asserting.
- CGL/WGL reject the new, currently EGL-specific flags.

Validation entry point (Linux, cached dependencies, optimized standalone build):
  python3 ports/dome-fltk/tests/graphics-context-creation.py \
    --cargo /usr/bin/cargo --output /absolute/path/to/new-report-directory

This runs the production attribute unit-test module and an offscreen Surfman
example on default/native and software renderers. It checks actual attributes,
descriptor lookup with another/no current context, borrowed native imports,
compatible/incompatible sharing, and clear/readback pixels. It does not reset
the GPU. GLES attribute selection is unit-tested; native GLES/CGL/WGL runtime
coverage has not been performed on the local Linux machine.

Browser loss notification, pending-command completion, automatic restoration,
frontend renderer/adapter recovery and native XR recovery remain separate work.
The browser does not yet enable the new creation flags.

References:
https://registry.khronos.org/EGL/specs/eglspec.1.5.pdf
https://registry.khronos.org/EGL/extensions/KHR/EGL_KHR_create_context.txt
https://registry.khronos.org/EGL/extensions/EXT/EGL_EXT_create_context_robustness.txt
https://registry.khronos.org/OpenGL/extensions/KHR/KHR_robustness.txt
