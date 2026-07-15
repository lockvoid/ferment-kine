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
mkdir -p "$STAGE/Headers"
cp "$CRATE/include/kine.h" "$STAGE/Headers/"
cat > "$STAGE/Headers/module.modulemap" <<'EOF'
module KineCore {
    header "kine.h"
    export *
}
EOF

args=()
for target in "${TARGETS[@]}"; do
  ( cd "$CRATE" && cargo build --release --locked --target "$target" )
  args+=(-library "$CRATE/target/$target/release/libkine.a" -headers "$STAGE/Headers")
done

xcodebuild -create-xcframework "${args[@]}" -output "$OUT"
echo "built $OUT"
