# Changelog

All notable changes are recorded here. Pre-1.0: breaking changes may land in any
release; the schema version (see `kine_version()`) marks wire-format compatibility.

## Unreleased

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
