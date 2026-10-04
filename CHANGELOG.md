# Changelog

All notable changes are recorded here. Pre-1.0: breaking changes may land in any
release; the schema version (see `kine_version()`) marks wire-format compatibility.

## Unreleased

- vello 0.3.0 (breaking for pixels): `vello_cpu` 0.0.9 → 0.3.0, and the GPU
  flavor moves from `vello_hybrid` 0.0.9 to its renamed successor `vello_gpu`
  0.3.0 on wgpu 30. Its image atlas is plain 2D pages made on demand — no
  texture array, which wgpu's GLES backend mis-binds — and a glyph vello cannot
  draw still renders blank, now reported once through the log sink.
- GLES GPU flavor (`--features gpu-gles`, Android): the Metal flavor's
  vello_gpu raster over wgpu's GLES backend on the host's current EGL context —
  `kine_gpu_gles_engine_create`, `kine_gpu_gles_render_document` (+
  `_viewport`) into a host-owned GL texture; the context comes back at GLES
  defaults for everything kine binds or enables, and a GL error kine raises
  fails its render instead of reaching the host. `kotlin/build.sh android`
  builds it; Kotlin `Kine.GPUEngine` and `Document.render(…, engine, texture)`.
- Kotlin binding lives here now (`kotlin/`): `:kine` (JVM — the JNA mirror of
  `kine.h`, with its tests) and `:kine-android` (the arm64 `.so` payload);
  `kotlin/build.sh` builds the core for both. It lived in the Android app's
  `native/kine`; the app includes the two modules from `KINE_ROOT`.
- `kine_admit` — the author's door (SCHEMA §10): repairs what has one reading
  (`keyframes-span`, `settled-envelope`, `host-signal`), then validates; returns
  the stored document, its repairs and the probe interface. Ruby `Kine.admit`,
  Swift `Kine.admit(_:)`.
- Validation (breaking): an anchor is a fraction — a literal inside [0,1] or a
  `unit` input; `inProgress`/`outProgress` are `unit` envelopes at their
  settled default (1/0). Documents that break either are refused at load.
- Validation reports an `ease` on a first keyframe before a missing endpoint.
- Initial public release. Schema v1: inputs, colors (`alpha`/`contrast`/`mix`),
  nodes (group/shape/text), animators (node + per-glyph/word/line, springs), Oklab
  color. C ABI (one-shot + document handles + RGBA), Ruby and Swift bindings.
