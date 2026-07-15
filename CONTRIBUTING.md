# Contributing

Open source, not open governance. Issues and small, focused PRs are welcome.

- **Discuss before building anything large.** Open an issue first; unsolicited
  large PRs may not be merged.
- **The schema is maintainer-driven.** `crate/docs/SCHEMA.md` is normative;
  changes to the wire format are decided by the maintainers, not by PR.
- **No roadmap promises.** Features land when they're needed by a real consumer.
- **Keep it green.** `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test` must pass; goldens are byte-exact on the same architecture (see the
  cross-arch note in the README). Regenerate goldens only on pinned-version bumps
  with `KINE_REGEN_GOLDENS=1`.
- **Determinism is the contract.** No clocks, randomness (beyond seeded+baked), or
  environment reads at render.

By contributing you agree to license your work under the repo's dual
MIT/Apache-2.0 terms.
