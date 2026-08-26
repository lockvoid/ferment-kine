# ferment-kine

Ruby FFI bindings to [kine](../README.md), a Rust render core for motion
documents. The gem is a thin wrapper; the native library is compiled from the
sibling `../crate` at install time by `ext/kine/extconf.rb`.

```ruby
require "kine"

Kine.register_font(File.binread("Inter.ttf"))
png = Kine.render_document(doc, t: 0.5, signals: { progress: 0.5 },
                           width: 512, height: 512)   # binary PNG String
Kine.probe(doc)  # => { "version" => 1, "size" => {...}, "inputs" => [...], "roles" => [...],
                 #      "assets" => [...], "fonts" => [...], "missingFonts" => [...] }
```

## Install

As a git-sourced gem (bundler builds the extension — unlike `path:` gems):

```ruby
gem "ferment-kine",
    git: "https://github.com/lockvoid/ferment-kine.git",
    branch: "main", glob: "ruby/*.gemspec"
```

`bundle install` compiles the crate via `cargo` (needs the Rust toolchain from
`../crate/rust-toolchain.toml`) and stages `libkine` next to the FFI loader.

For local development against a checkout:

```sh
bundle config local.ferment-kine /path/to/ferment-kine
```

Run the gem tests directly after building the library once:

```sh
ruby ext/kine/extconf.rb && ruby -Ilib test/kine_test.rb
```
