# frozen_string_literal: true

# Builds the kine cdylib from the sibling crate and stages it into the gem's
# lib/kine/ for FFI. Git-sourced gems get their `spec.extensions` built by
# bundler (unlike path: gems — bundler#1679), so this runs on every machine at
# `bundle install`; there is no hand-rolled build step to maintain.
#
# Not an mkmf C extension — we compile Rust and emit a no-op Makefile so bundler's
# `make && make install` succeeds.

require "fileutils"
require "rbconfig"

GEM_ROOT = File.expand_path("../..", __dir__)          # ferment-kine/ruby
CRATE_DIR = File.expand_path("../../../crate", __dir__) # ferment-kine/crate
LIB_DIR = File.join(GEM_ROOT, "lib", "kine")

ext = RbConfig::CONFIG["host_os"] =~ /darwin/ ? "dylib" : "so"
staged = File.join(LIB_DIR, "libkine.#{ext}")

def die(message)
  warn "extconf: #{message}"
  exit(1)
end

die("cargo not found — install Rust (https://rustup.rs)") unless system("command -v cargo >/dev/null 2>&1")

unless File.file?(File.join(CRATE_DIR, "Cargo.toml"))
  die("kine crate not found at #{CRATE_DIR}")
end

Dir.chdir(CRATE_DIR) do
  die("cargo build failed") unless system("cargo build --release --locked")
end

built = File.join(CRATE_DIR, "target", "release", "libkine.#{ext}")
die("build produced no libkine.#{ext}") unless File.file?(built)

FileUtils.mkdir_p(LIB_DIR)
FileUtils.cp(built, staged)
warn "extconf: staged #{staged}"

# No-op Makefile: the artifact is already staged above.
File.write(File.join(__dir__, "Makefile"), <<~MAKE)
  all:
  \t@true
  install:
  \t@true
  clean:
  \t@true
MAKE
