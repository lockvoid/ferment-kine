#!/usr/bin/env bash
# Build the kine core for the Kotlin modules:
#
#   host     macOS arm64 cdylib → kine/build/host/libkine.dylib, loaded by JNA
#            in `./gradlew :kine:test` (jna.library.path points at that dir).
#   android  arm64-v8a cdylib → kine-android/src/main/jniLibs/arm64-v8a/
#            libkine.so, packaged by :kine-android.
#
# Both flavors are CPU-only: `--features gpu` is the vello_hybrid/wgpu Metal
# tree (scripts/build-xcframework.sh builds it for the Apple slices), so
# kine_gpu_available() answers 0 here and no kine_gpu_* symbol exists to bind.
#
# The crate's rust-toolchain.toml governs; --locked means its Cargo.lock is
# law. Build products are gitignored — regenerate after any crate change.
#
# Usage: kotlin/build.sh [host|android]   (default: both)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$(cd "$HERE/../crate" && pwd)"

build_host() {
  local target=aarch64-apple-darwin
  # Run from the crate: its rust-toolchain.toml is what governs the toolchain,
  # for `rustup target add` exactly as for `cargo build`.
  ( cd "$CRATE" && rustup target add "$target" >/dev/null &&
    cargo build --release --locked --target "$target" )
  mkdir -p "$HERE/kine/build/host"
  cp "$CRATE/target/$target/release/libkine.dylib" "$HERE/kine/build/host/libkine.dylib"
  echo "host: $HERE/kine/build/host/libkine.dylib"
}

build_android() {
  : "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to an NDK r28 or newer}"
  # 16 KB page alignment (Android 15+, NDK r28 default) comes from the NDK's
  # linker, so cargo-ndk must see it — hence the ANDROID_NDK_HOME requirement.
  ( cd "$CRATE" && rustup target add aarch64-linux-android >/dev/null &&
    cargo ndk -t arm64-v8a -o "$HERE/kine-android/src/main/jniLibs" build --release --locked )
  echo "android: $HERE/kine-android/src/main/jniLibs/arm64-v8a/libkine.so"
}

case "${1:-all}" in
  host) build_host ;;
  android) build_android ;;
  all) build_host; build_android ;;
  *) echo "usage: $0 [host|android]" >&2; exit 2 ;;
esac
