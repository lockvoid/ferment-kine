# Contributing

Issues, bug reports, and focused pull requests are welcome. For a larger change,
open an issue first so we can agree on scope before you spend time implementing it.

## Changes to the format

[crate/docs/SCHEMA.md](crate/docs/SCHEMA.md) is the normative specification.
Maintainers decide schema changes; discuss a new field or behavior before
changing the format. Include a concrete use case and describe how existing
documents would behave.

Rendering must remain deterministic: no clocks, unseeded randomness, or
environment reads during frame evaluation. Seeded animation is baked at load.

## Validate a change

Run these commands from `crate/`:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Run the affected binding tests when changing the C ABI or a language wrapper.
The binding guides describe the required native builds.

Golden images are exact comparisons on the reference architecture. Regenerate
them only for an agreed pinned-version change, with `KINE_REGEN_GOLDENS=1`.
Explain any pixel changes in the pull request; do not regenerate images just
to make a failed test pass. The [README](README.md#rendering-and-reproducibility)
explains the cross-architecture and GPU comparison limits.

Contributions are licensed under the repository's MIT/Apache-2.0 terms.
