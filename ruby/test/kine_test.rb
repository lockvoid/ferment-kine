# frozen_string_literal: true

# Runs standalone (no Rails needed):
#   ruby -Ilib vendor/ferment-kine/test/kine_test.rb
#
# Exercises the real FFI seams against the v1 schema: probe interface, document
# rendering with signals, schema errors with paths, thread safety (blocking
# render releases the GVL), and buffer freeing.

require "minitest/autorun"
require "json"
require "kine"

class KineTest < Minitest::Test
  PNG_SIGNATURE = "\x89PNG\r\n\x1a\n".b

  # Self-contained fixtures (copied from the kine crate): a test font (family
  # "Bebas Neue", the document's default fontFamily) and a valid v1 document.
  # The gem does not depend on the crate's on-disk location.
  FONT_PATH = File.expand_path("fixtures/font.ttf", __dir__)
  FIXTURE = File.read(File.expand_path("fixtures/test_card.json", __dir__))

  def setup
    Kine.register_font(File.binread(FONT_PATH))
  end

  def assert_png(bytes)
    assert_kind_of String, bytes
    assert_equal Encoding::ASCII_8BIT, bytes.encoding
    assert bytes.start_with?(PNG_SIGNATURE), "not a PNG (#{bytes[0, 8].inspect})"
  end

  def render(t:, progress:, width: 256, height: 256)
    Kine.render_document(FIXTURE, t: t, signals: { progress: progress }, width: width, height: height)
  end

  def test_renders_valid_png
    assert_png render(t: 0.0, progress: 0.0)
  end

  def test_t_and_signals_are_plumbed_through
    a = render(t: 0.0, progress: 0.0)
    b = render(t: 1.3, progress: 0.8)
    assert_png a
    assert_png b
    refute_equal a, b, "different t/progress produced identical bytes"
  end

  def test_probe_returns_the_declared_interface
    interface = Kine.probe(FIXTURE)
    assert_equal 1, interface["version"]
    assert_equal %w[time progress activations font foreground background accent borderColor],
                 interface["inputs"].map { |i| i["key"] }
    assert_nil interface["colors"], "the color table stays private (probe is seed-only)"
    assert_equal %w[card text], interface["roles"]
    assert_equal({ "width" => 512, "height" => 512 }, interface["size"])
  end

  def test_probe_manifests_referenced_and_missing_fonts
    interface = Kine.probe(FIXTURE)
    assert_equal ["Bebas Neue"], interface["fonts"]
    assert_equal [], interface["missingFonts"]

    restyled = JSON.parse(FIXTURE)
    restyled["inputs"].find { |i| i["key"] == "font" }["default"] = "Missing Grotesk"
    interface = Kine.probe(restyled)
    assert_equal ["Missing Grotesk"], interface["fonts"]
    assert_equal ["Missing Grotesk"], interface["missingFonts"]
  end

  def test_schema_error_carries_the_failing_path
    invalid = JSON.parse(FIXTURE)
    invalid["animators"][0]["target"] = "nope.glyphs"
    error = assert_raises(Kine::Error) do
      Kine.render_document(invalid, t: 0.0, width: 64, height: 64)
    end
    assert_match(/animators\[0\]\.target/, error.message)
    assert_match(/nope/, error.message)
  end

  def test_empty_document_raises
    error = assert_raises(Kine::Error) do
      Kine.render_document({}, t: 0.0, width: 64, height: 64)
    end
    assert_match(/missing field/, error.message)
  end

  def test_concurrent_renders_do_not_crash
    errors = Queue.new
    threads = Array.new(4) do |n|
      Thread.new do
        25.times do |i|
          assert_png render(t: n + i * 0.01, progress: (i % 10) / 10.0)
        rescue StandardError => e
          errors << e
        end
      end
    end
    threads.each(&:join)
    assert errors.empty?, "concurrent renders raised: #{errors.pop if !errors.empty?}"
  end

  def test_no_leak_over_many_renders
    render(t: 0.0, progress: 0.0) # warm up allocations
    GC.start
    before = rss_kb
    500.times { |i| render(t: i * 0.01, progress: (i % 100) / 100.0) }
    GC.start
    growth_mb = (rss_kb - before) / 1024.0
    assert_operator growth_mb, :<, 80, "RSS grew #{growth_mb.round}MB over 500 renders (leak?)"
  end

  private

  def rss_kb
    `ps -o rss= -p #{Process.pid}`.to_i
  end
end
