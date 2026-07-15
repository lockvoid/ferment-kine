# Changelog

All notable changes are recorded here. Pre-1.0: breaking changes may land in any
release; the schema version (see `kine_version()`) marks wire-format compatibility.

## Unreleased

- Initial public release. Schema v1: inputs, colors (`alpha`/`contrast`/`mix`),
  nodes (group/shape/text), animators (node + per-glyph/word/line, springs), Oklab
  color. C ABI (one-shot + document handles + RGBA), Ruby and Swift bindings.
