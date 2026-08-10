#!/usr/bin/env bash
# Build swift/KineCore.xcframework from the crate: static libs for device iOS,
# iOS simulator, and macOS (the macOS slice lets `swift test` run on the Mac host
# with no simulator). Produces one xcframework the Swift package consumes as a
# local binaryTarget.
#
# Requires the Rust toolchain and Xcode. The xcframework is a build artifact
# (gitignored) — regenerate it after any crate change.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CRATE="$ROOT/crate"
OUT="$ROOT/swift/KineCore.xcframework"
STAGE="$ROOT/swift/.build/xcframework"

TARGETS=(aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin)

# Add the targets for the crate's pinned toolchain (rust-toolchain.toml), not the
# default one — cargo builds below use the pinned toolchain.
TOOLCHAIN="$(cd "$CRATE" && rustup show active-toolchain | awk '{print $1}')"
for target in "${TARGETS[@]}"; do
  rustup target add --toolchain "$TOOLCHAIN" "$target" >/dev/null
done

rm -rf "$OUT" "$STAGE"
# Namespace the header + modulemap under a `KineCore/` subdir. Two static-library
# xcframeworks whose modulemaps both sit at the Headers root collide when linked
# side-by-side (Xcode copies each `Headers/module.modulemap` to the shared
# `$BUILT_PRODUCTS_DIR/include/module.modulemap` — "Multiple commands produce").
# The subdir makes it `include/KineCore/module.modulemap`, so Kine coexists
# with any other static-library xcframework a host links. `import KineCore`
# still resolves (`header "kine.h"` is relative to the modulemap).
mkdir -p "$STAGE/Headers/KineCore"
cp "$CRATE/include/kine.h" "$STAGE/Headers/KineCore/"
cat > "$STAGE/Headers/KineCore/module.modulemap" <<'EOF'
module KineCore {
    header "kine.h"
    export *
}
EOF

# --features gpu: the Apple slices carry the vello_hybrid/wgpu raster flavor.
# Deliberately NOT the default feature set — the ruby gem builds this same crate
# without it and must never link the wgpu tree (see crate/Cargo.toml).
args=()
for target in "${TARGETS[@]}"; do
  ( cd "$CRATE" && cargo build --release --locked --features gpu --target "$target" )
  args+=(-library "$CRATE/target/$target/release/libkine.a" -headers "$STAGE/Headers")
done

xcodebuild -create-xcframework "${args[@]}" -output "$OUT"
echo "built $OUT"
