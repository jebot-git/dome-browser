# Pinned HTML parser patch

Dome uses the existing html5ever 0.40.1 and markup5ever 0.40.0 package versions.
The copies here preserve the published package files and licenses. Their original
SHA-256 file inventories are in `html-parser-upstream.json`; the exact local delta
is `html-parser-dome.patch`. Cargo's local overrides replace the registry sources
without changing package versions or the production dependency graph.

The patch adds an optional TreeSink contextual-creation method. Existing sinks
keep their original creation behavior. The tree builder supplies the insertion
parent or sibling/foster-parent fallback before element creation, including foreign
content and adoption-agency reconstruction. Servo resolves this context on the
DOM thread, selects the owner document and registry before constructors run, and
carries opaque context IDs through its optional asynchronous parser.

Registry-aware fragment insertion keeps null/scoped/global selections through the
internal parser-document handoff. Other inert/sanitizing parser callers retain
their existing entry point. Temporary parser documents are excluded from scoped
registry document associations. Public association order is retained for upgrades.

Verification:

```sh
python3 etc/verify_html_parser_vendor.py
```

The browser fixtures and unchanged custom-element WPT suites are the runtime
validation gates. Package library tests can run in an isolated workspace with
these exact source copies; the dependency packages are not root workspace members.
