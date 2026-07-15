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
