# frozen_string_literal: true

# Renders the docs/*.json demos through the engine and writes the README assets
# into docs/assets/: a still PNG per demo (byte-compared in CI, so a README image
# can never silently drift from what the engine produces) plus an animated WebP
# for the moving ones.
#
#   ruby scripts/render-readme-assets.rb
#
# Requires the built Ruby gem on the load path; WebP output also needs ffmpeg
# (its absence is non-fatal — only the stills are golden).

require "base64"
require "json"
require "fileutils"
require "tmpdir"

ROOT = File.expand_path("..", __dir__)
$LOAD_PATH.unshift(File.join(ROOT, "ruby", "lib"))
require "kine"

ASSETS = File.join(ROOT, "docs", "assets")
FONT = File.join(ROOT, "crate", "testdata", "font.ttf")

FileUtils.mkdir_p(ASSETS)
Kine.register_font(File.binread(FONT))

def doc(name)
  File.read(File.join(ROOT, "docs", "#{name}.json"))
end

def still(name, json, t, width, height, signals = {})
  path = File.join(ASSETS, "#{name}.png")
  File.binwrite(path, Kine.render_document(json, t: t, signals: signals, width: width, height: height))
  puts "wrote docs/assets/#{name}.png"
end

# Renders `frames` evenly across a `seconds`-long loop and muxes an animated,
# infinitely looping WebP. `at.(phase)` returns [t, signals] for phase 0..1.
def animate(name, json, width, height, seconds:, frames:, fps:, quality: 80, &at)
  Dir.mktmpdir("kine-#{name}") do |dir|
    frames.times do |i|
      t, signals = at.call(i.to_f / frames)
      png = Kine.render_document(json, t: t, signals: signals || {}, width: width, height: height)
      File.binwrite(format("%s/f%03d.png", dir, i), png)
    end
    unless system("command -v ffmpeg >/dev/null 2>&1")
      warn "ffmpeg not found — skipping #{name}.webp (still written)"
      return
    end
    webp = File.join(ASSETS, "#{name}.webp")
    ok = system(
      "ffmpeg", "-y", "-loglevel", "error",
      "-framerate", fps.to_s, "-i", "#{dir}/f%03d.png",
      "-loop", "0", "-lossless", "0", "-quality", quality.to_s, webp
    )
    abort("ffmpeg failed for #{name}") unless ok
    puts "wrote docs/assets/#{name}.webp"
  end
end

# All assets render at 2x their on-page display size (the README pins each <img>
# to the 1x width) so they stay sharp on retina/HiDPI. Because the browser then
# downscales by half, WebP compression artifacts vanish — the animations use a
# lower quality to offset the pixel-count bump. The engine scales the document
# uniformly to whatever output size we request (render.rs), so 2x is distortion-
# free. Dimensions below are 2x; the display width is in each comment.

# --- hero: karaoke title card (docs/hero.json) — displays at 800w ------------
# Still is fully lit, mid idle-wave; the WebP is one fill sweep over the wave.
hero = doc("hero")
still("social", hero, 0.6, 1600, 520, { progress: 1.0 })
animate("hero", hero, 1600, 520, seconds: 2, frames: 60, fps: 30, quality: 72) do |phase|
  [phase * 2.4, { progress: [phase * 1.3, 1.0].min }]
end

# --- palette: one accent → a derived ramp (docs/palette.json) — displays at 900w
# Purely a function of the seed colors — no time axis, so the still is the demo.
still("palette", doc("palette"), 0.0, 1800, 496)

# --- pulse: all-vector motion mark (docs/pulse.json) — displays at 320w -------
# Spring-scaled core (period 2s) over a linear orbit (period 4s); the loop closes
# seamlessly at 4s — two pulses per revolution. 30fps, and the still catches the
# core near the top of its spring overshoot.
pulse = doc("pulse")
still("pulse", pulse, 0.56, 640, 640)
animate("pulse", pulse, 640, 640, seconds: 4, frames: 120, fps: 30, quality: 60) do |phase|
  [phase * 4.0, {}]
end

# --- sticker demos: a third-party animated GIF (Klipy) composited with kine's
# own text + motion in one document — the whole point of the image-asset feature.
# The GIF travels base64 inside the document; the loop length is an integer
# multiple of the sticker's own GIF loop (stars 0.66s, follow 3.3s) and the kine
# animator periods divide it, so the WebP closes seamlessly. Rendered at the 760
# design size, displayed at 380w (2x retina).

def sticker(name)
  Base64.strict_encode64(File.binread(File.join(ROOT, "docs", "stickers", "#{name}.gif")))
end

def plate(top, bot)
  { kind: "shape", key: "plate",
    geometry: { kind: "roundedRect", x: 8, y: 8, width: 744, height: 244, radius: 28 },
    fill: { kind: "linearGradient", x1: 0, y1: 0, x2: 0, y2: 260,
            stops: [{ at: 0, color: { color: top } }, { at: 1, color: { color: bot } }] },
    stroke: { color: { color: "hair" }, width: 1 } }
end

star_repo = {
  version: 1, size: { width: 760, height: 260 },
  inputs: [{ key: "time", type: "time", default: 0 }],
  colors: [
    { key: "bg", value: "#14161F" }, { key: "gold", value: "#FFC53D" },
    { key: "ptop", value: { fn: "mix", a: "bg", b: "gold", t: 0.10 } },
    { key: "pbot", value: { fn: "mix", a: "bg", b: "#000000", t: 0.35 } },
    { key: "hair", value: { fn: "alpha", of: "gold", amount: 0.22 } },
  ],
  assets: [{ key: "st", kind: "image", mime: "image/gif", data: sticker("stars") }],
  root: { kind: "group", key: "r", children: [
    plate("ptop", "pbot"),
    { kind: "image", key: "stars", asset: "st", frame: { x: 40, y: 46, width: 168, height: 168 }, fit: "contain" },
    { kind: "text", key: "head", content: "STAR THE REPO",
      frame: { x: 230, y: 40, width: 500, height: 180 },
      style: { fontFamily: "Bebas Neue", size: 88, align: "left", valign: "center", letterSpacing: 2, fill: { color: "gold" } } },
  ] },
  animators: [
    { target: "stars", property: "scale", driver: "time", period: 1.98, keyframes: [
      { at: 0, value: 0.9 }, { at: 0.32, value: 1.08, ease: { spring: { bounce: 0.45, duration: 0.5 } } },
      { at: 0.65, value: 1.0, ease: "easeOut" }, { at: 1, value: 0.9, ease: "easeIn" }] },
    { target: "head.glyphs", property: "translateY",
      weight: { stagger: { driver: "time", period: 0.99, total: 0.7, from: "start", ease: "easeInOut" } },
      keyframes: [{ at: 0, value: 0 }, { at: 0.5, value: -9, ease: "easeInOut" }, { at: 1, value: 0, ease: "easeInOut" }] },
  ],
}.to_json

subscribe = {
  version: 1, size: { width: 760, height: 260 },
  inputs: [{ key: "time", type: "time", default: 0 }],
  colors: [
    { key: "bg", value: "#14161F" }, { key: "pink", value: "#FF4D8D" }, { key: "ink", value: "#F4F4FB" },
    { key: "ptop", value: { fn: "mix", a: "bg", b: "pink", t: 0.12 } },
    { key: "pbot", value: { fn: "mix", a: "bg", b: "#000000", t: 0.35 } },
    { key: "hair", value: { fn: "alpha", of: "pink", amount: 0.22 } },
  ],
  assets: [{ key: "fl", kind: "image", mime: "image/gif", data: sticker("follow") }],
  root: { kind: "group", key: "r", children: [
    plate("ptop", "pbot"),
    { kind: "image", key: "follow", asset: "fl", frame: { x: 36, y: 34, width: 192, height: 192 }, fit: "contain" },
    { kind: "text", key: "head", content: "NEW REELS WEEKLY",
      frame: { x: 250, y: 40, width: 480, height: 180 },
      style: { fontFamily: "Bebas Neue", size: 80, align: "left", valign: "center", letterSpacing: 2,
               fill: { color: "ink" }, activeFill: { color: "pink" } } },
  ] },
  animators: [
    { target: "follow", property: "scale", driver: "time", period: 1.65, keyframes: [
      { at: 0, value: 0.85 }, { at: 0.3, value: 1.06, ease: { spring: { bounce: 0.5, duration: 0.5 } } },
      { at: 0.62, value: 1.0, ease: "easeOut" }, { at: 1, value: 0.85, ease: "easeIn" }] },
    { target: "head.glyphs", property: "color",
      weight: { stagger: { driver: "time", period: 1.65, total: 0.85, from: "start" } } },
  ],
}.to_json

still("star_repo", star_repo, 0.5, 760, 260)
animate("star_repo", star_repo, 760, 260, seconds: 1.98, frames: 60, fps: 60.0 / 1.98, quality: 68) do |phase|
  [phase * 1.98, {}]
end

still("subscribe", subscribe, 1.0, 760, 260)
animate("subscribe", subscribe, 760, 260, seconds: 3.3, frames: 99, fps: 30, quality: 64) do |phase|
  [phase * 3.3, {}]
end
