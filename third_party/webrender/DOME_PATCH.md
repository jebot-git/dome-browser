This is the pinned WebRender 0.70.0 crate release. `upstream.dome.json`
records its archive digest and upstream Git revision. The original source
headers and Cargo manifests are preserved. The MPL 2.0 license is available
in the repository root `LICENSE`.

Dome's change is in `src/hit_test.rs`: query a pipeline's viewport coordinates
outside its clipped hit regions, and obtain that coordinate and the ordinary
hit result from the same rendered scene snapshot. This lets captured native
pen input retain correct frame coordinates while leaving the frame or page.
Missing, singular and non-finite transforms return no coordinate.

The paired `../webrender_api` crate adds the corresponding `ApiHitTester`
methods. Five focused tests in `pipeline_point_tests` cover unclipped points,
translation, scale, rotation and invalid transforms. Browser integration
tests live under `ports/dome-fltk/tests/native-pen-frames.*`.

Root-renderer loss cleanup adds `Renderer::deinit_after_context_loss`, for callers
that have stopped the render backend and established permanent native context
loss. CPU GL handles share device-local abandonment state, including handles
that outlive the renderer, so dropping them needs no GL deletion. Profiling query
arrays are cleared without GL calls; profiling markers retain the loss state so
late scope destruction cannot pop groups on another current context. Normal `deinit` retains its current-context
contract. Native context destruction and embedder-owned external-image/compositor
cleanup remain the embedder's responsibility; this does not provide GPU recovery.

The native tests in `components/paint/renderer_retirement_tests.rs` construct real
offscreen renderers and peer contexts. They check normal shutdown, retry after a
failed bind, and known-loss cleanup while preserving the peer's texture and query
objects. They do not reset the GPU or operate on a desktop session.
