# Kine for Ruby

The `ferment-kine` gem wraps the shared Rust renderer through FFI. It renders
motion documents to PNG and exposes font registration, probing, and admission.
The [schema](../crate/docs/SCHEMA.md) defines the document format.

## Install

Use Ruby 3.0 or later and the pinned Rust toolchain from
[crate/rust-toolchain.toml](../crate/rust-toolchain.toml). In your Gemfile:

```ruby
gem "ferment-kine",
    git: "https://github.com/lockvoid/ferment-kine.git",
    branch: "main", glob: "ruby/*.gemspec"
```

`bundle install` compiles the native core from the sibling `crate/` directory
and stages the library beside the FFI loader. A git-sourced gem runs the native
extension build; a Bundler `path:` dependency does not build it automatically.

## Render a frame

After installation, run this from a checkout's root, or adapt the resource paths
for your application:

```ruby
require "kine"

Kine.register_font(File.binread("crate/testdata/font.ttf"))
document = File.read("docs/hero.json")
png = Kine.render_document(document, t: 0.5, signals: { progress: 0.5 },
                           width: 800, height: 260)
File.binwrite("/tmp/kine-card.png", png)
```

`png` is a binary Ruby string. JSON arguments may be strings or Ruby hashes.
`Kine.probe(document)` returns interface metadata; `Kine.admit(document)` applies
only the authoring repairs defined by the schema and returns the admitted JSON,
repairs, and interface. Native errors raise `Kine::Error`.

## Develop and test

From `ruby/`, build the native extension and run the tests:

```sh
ruby ext/kine/extconf.rb
ruby -Ilib test/kine_test.rb
```

The extension builds the CPU renderer; it does not enable GPU dependencies.
Rebuild it after changing the core. Source licensing and font notices are in
the [root README](../README.md#license).
