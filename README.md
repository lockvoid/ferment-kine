# kine

A Rust render core for **motion documents** — typed, versioned JSON scenes that
are pure functions `(inputs) → pixels`, rendered over the
[Vello](https://github.com/linebender/vello) CPU renderer with
[parley](https://github.com/linebender/parley) text layout.

A motion document has no intrinsic duration, no state, and no logic: all
variation over time enters through declared inputs, and all motion is expressed
as animators mapping inputs to property values. The same document renders
identically on every platform, forever — no randomness at render, no clocks, no
environment reads. See [`crate/docs/SCHEMA.md`](crate/docs/SCHEMA.md) for the
normative v1 spec.

## Status

**Pre-1.0 — the API breaks freely.** Nothing in the Linebender stack this builds
on is 1.0 yet (`vello_cpu`/`vello_common` 0.0.x, `parley` 0.11), so versions are
pinned exactly and `Cargo.lock` is committed. The C ABI in
[`crate/include/kine.h`](crate/include/kine.h) is the one surface intended to be
stable within a version.

## Using it

The only supported surface is the extern-C boundary
([`crate/include/kine.h`](crate/include/kine.h)). The crate builds as both a
`cdylib` (for FFI language bindings) and a `staticlib` (for native app
embedding):

```sh
cd crate
cargo build --release --locked        # → target/release/libkine.{dylib,so,a}
cargo test --locked                    # schema, evaluator, and golden render tests
```

Register fonts (bundled only — no system fonts are ever loaded), then render a
document to PNG or raw RGBA. Every entry point is panic-proof: bad input is
reported through `kine_last_error()`, never a crash across the FFI boundary.

## Determinism

Renders are deterministic for a given (document, signals, size). Golden PNGs in
`crate/tests/goldens` are byte-exact on the architecture they were generated on
(currently macOS arm64 / NEON). Cross-architecture byte-parity is not yet
promised — see the schema's evaluation semantics and the cross-arch note in the
test suite.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Bundled test fonts are under the SIL Open Font License.
