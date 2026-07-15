# frozen_string_literal: true

require_relative "lib/kine/version"

Gem::Specification.new do |spec|
  spec.name = "ferment-kine"
  spec.version = Kine::VERSION
  spec.authors = ["LockVoid Labs ~●~"]
  spec.summary = "Ruby FFI bindings to Kine, a Rust render core for motion documents (Vello/parley)."
  spec.description = "Renders motion documents to PNG via the kine Rust core, built over vello_cpu + " \
                     "parley. This gem is the thin Ruby FFI wrapper; the native library is compiled " \
                     "from the crate at install time by ext/kine/extconf.rb."
  spec.homepage = "https://github.com/lockvoid/ferment-kine"
  spec.license = "MIT OR Apache-2.0"
  spec.required_ruby_version = ">= 3.0"

  # ruby/lib + ruby/ext, plus the sibling crate the extension compiles.
  spec.files = Dir["lib/**/*.rb", "ext/kine/extconf.rb"] +
               Dir["../crate/**/*"].reject { |p| p.include?("/target/") }
  spec.require_paths = ["lib"]

  # Git-sourced gems build extensions via bundler (path: gems do not — bundler#1679).
  # extconf.rb compiles the crate and stages libkine into lib/kine/.
  spec.extensions = ["ext/kine/extconf.rb"]

  spec.add_dependency "ffi", "~> 1.15"
end
