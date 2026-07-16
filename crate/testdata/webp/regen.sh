#!/usr/bin/env bash
#
# Regenerates the animated-WebP compositing regression fixtures.
#
# Pure Rust cannot ENCODE animated WebP, so these are built with libwebp's CLI
# (cwebp + webpmux). Each fixture exercises a blend/dispose class that broke in
# image-webp 0.2.4 (pinned away in crate/Cargo.toml); committing them means a
# future patch-rev bump that reintroduced a bug would fail our own tests.
#
# Requirements: libwebp CLI (`brew install webp`, tested with 1.6.0) and python3
# (stdlib only). Frame pixels are emitted EXACTLY by the PNG writer below — no
# color management, no premultiply — so the fixtures are deterministic. cwebp
# re-encodes from decoded pixels, so the throwaway PNG byte layout is irrelevant;
# rerunning on the same libwebp version yields byte-identical .webp files.
#
# Frame layouts (see src/tests.rs for the matching assertions):
#   a_dispose_ghost.webp   6x6  f0=red 4x4 @(0,0) dispose=BG; f1=blue 4x4 @(2,2)
#                                dispose=BG. The disposed, non-overlapped part of
#                                f0 must be transparent at f1 (no ghosting).
#   b_dispose_lossy.webp   8x8  f0=red 8x8 @(0,0) dispose=BG; f1=green 4x4 @(2,2)
#                                LOSSY (VP8, no alpha) no-blend. Disposed region
#                                cleared, neighbours untouched, f1 pixels correct.
#   c_blend_modes.webp     8x8  f0=red@128 8x8; f1=green@200 4x4 @(0,0) NO_BLEND;
#                                f2=blue@128 4x4 @(0,4) ALPHA_BLEND. Replace vs
#                                over-blend against existing canvas alpha.
#
set -euo pipefail
cd "$(dirname "$0")"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# mkpng W H R G B A OUT — a solid WxH straight-alpha RGBA8 PNG (exact bytes).
mkpng() {
  python3 - "$@" <<'PY'
import sys, zlib, struct
w, h, r, g, b, a = (int(x) for x in sys.argv[1:7])
out = sys.argv[7]
def chunk(typ, data):
    body = typ + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xffffffff)
row = bytes((r, g, b, a)) * w
raw = bytearray()
for _ in range(h):
    raw.append(0)          # filter: none
    raw.extend(row)
png = b"\x89PNG\r\n\x1a\n"
png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))   # 8-bit RGBA
png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
png += chunk(b"IEND", b"")
open(out, "wb").write(png)
PY
}

ll() { cwebp -quiet -lossless -exact "$1" -o "$2"; }   # lossless, preserve exact RGBA
lossy() { cwebp -quiet -q 90 "$1" -o "$2"; }           # lossy VP8 (drops alpha)

# --- (a) dispose=BACKGROUND ghosting ----------------------------------------
# Blocks are alpha<255 so the animation carries an alpha channel — required for
# the transparent background these dispose bugs need (an all-opaque animation
# has no alpha plane, so "background" can never be transparent).
mkpng 4 4 255 0 0 200 "$tmp/a0.png"; ll "$tmp/a0.png" "$tmp/a0.webp"
mkpng 4 4 0 0 255 200 "$tmp/a1.png"; ll "$tmp/a1.png" "$tmp/a1.webp"
webpmux -frame "$tmp/a0.webp" +100+0+0+1-b \
        -frame "$tmp/a1.webp" +100+2+2+1-b \
        -bgcolor 0,0,0,0 -loop 0 -o a_dispose_ghost.webp

# --- (b) dispose=BACKGROUND then a lossy no-alpha frame ----------------------
# f0 carries alpha (so the canvas is alpha-capable and disposes to transparent);
# f1 is the alpha-LESS lossy frame that follows — the #178 corruption trigger.
mkpng 8 8 255 0 0 200 "$tmp/b0.png"; ll "$tmp/b0.png" "$tmp/b0.webp"
mkpng 4 4 0 255 0 255 "$tmp/b1.png"; lossy "$tmp/b1.png" "$tmp/b1.webp"
webpmux -frame "$tmp/b0.webp" +100+0+0+1-b \
        -frame "$tmp/b1.webp" +100+2+2+0-b \
        -bgcolor 0,0,0,0 -loop 0 -o b_dispose_lossy.webp

# --- (c) NO_BLEND vs ALPHA_BLEND over existing canvas alpha ------------------
mkpng 8 8 255 0 0 128 "$tmp/c0.png"; ll "$tmp/c0.png" "$tmp/c0.webp"
mkpng 4 4 0 220 0 200 "$tmp/c1.png"; ll "$tmp/c1.png" "$tmp/c1.webp"
mkpng 4 4 0 0 255 128 "$tmp/c2.png"; ll "$tmp/c2.png" "$tmp/c2.webp"
webpmux -frame "$tmp/c0.webp" +100+0+0+0-b \
        -frame "$tmp/c1.webp" +100+0+0+0-b \
        -frame "$tmp/c2.webp" +100+0+4+0+b \
        -bgcolor 0,0,0,0 -loop 0 -o c_blend_modes.webp

# The committed fixtures were built with the libwebp below; a different version
# (esp. cwebp's lossy encoder for b_dispose_lossy) may produce non-byte-identical
# .webp. Print it so a mismatch isn't silent.
echo "built with: $(cwebp -version 2>&1 | head -1) (cwebp)  $(webpmux -version 2>&1 | head -1) (webpmux)"
echo "wrote:"
for f in a_dispose_ghost b_dispose_lossy c_blend_modes; do
  printf "  %-22s %s\n" "$f.webp" "$(webpmux -info "$f.webp" | grep -iE 'Canvas size' | tr -s ' ')"
done
