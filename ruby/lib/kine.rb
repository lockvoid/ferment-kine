# frozen_string_literal: true

require "json"

require_relative "kine/version"
require_relative "kine/ffi"

# Ruby wrapper over the shared Rust motion render core (crate/). Renders v1
# motion documents (crate/docs/SCHEMA.md): a pure function (inputs) → scene,
# animated through declared signals. See crate/include/kine.h for the ABI.
#
#   Kine.register_font(File.binread("Inter.ttf"))
#   png = Kine.render_document(doc, t: 0.5, signals: { progress: 0.5 },
#                                width: 512, height: 512)
#   Kine.probe(doc)  # => {"version"=>1, "size"=>..., "inputs"=>[...], "roles"=>[...]}
module Kine
  class Error < StandardError; end

  module_function

  # Register a font (TTF/OTF) into the bundled-only collection. Idempotent.
  def register_font(bytes)
    bytes = bytes.b
    buffer = ::FFI::MemoryPointer.new(:uint8, bytes.bytesize)
    buffer.put_bytes(0, bytes)
    code = FFI.kine_register_font(buffer, bytes.bytesize)
    raise Error, last_error || "font registration failed" unless code.zero?

    true
  end

  # Render a document at time `t` with `signals` → binary PNG String.
  # `doc`/`signals` may be a Hash (JSON-encoded here) or a raw JSON String.
  def render_document(doc, t:, width:, height:, signals: {})
    doc_ptr = cstring(encode_json(doc))
    signals_ptr = cstring(encode_json(signals))
    result = FFI.kine_render_document(doc_ptr, t.to_f, signals_ptr, width, height)
    read_buffer(result) || raise(Error, last_error || "render failed")
  end

  # Union INK rect across `samples` frames over [0, span] seconds at a probe
  # raster — {"x","y","width","height"} Hash in design-box fractions, or nil
  # for a fully-blank document. The crate's one definition of visual bounds
  # (sticker normalization + preview trimming read THIS, byte-identical with
  # the iOS selection border).
  def ink_union(doc, signals: {}, samples: 1, span: 0.0, width: 160, height: 160)
    doc_ptr = cstring(encode_json(doc))
    signals_ptr = cstring(encode_json(signals))
    result = FFI.kine_ink_union(doc_ptr, signals_ptr, samples, span.to_f, width, height)
    json = read_buffer(result)
    raise(Error, last_error || "ink probe failed") if json.nil? && last_error
    return nil if json.nil? || json.empty?
    JSON.parse(json)
  end

  # Describe a document's inputs → parsed JSON Hash. Phase 0 returns a stub.
  def probe(doc)
    result = FFI.kine_probe(cstring(encode_json(doc)))
    json = read_buffer(result) || raise(Error, last_error || "probe failed")
    JSON.parse(json)
  end

  def encode_json(value)
    value.is_a?(String) ? value : JSON.generate(value)
  end

  # NUL-terminated native buffer for a Rust `*const c_char`. Held by the caller
  # for the duration of the FFI call (matters for the blocking render).
  def cstring(str)
    str = str.to_s.b
    # MemoryPointer zero-fills, so the extra byte is already a NUL terminator.
    ptr = ::FFI::MemoryPointer.new(:uint8, str.bytesize + 1)
    ptr.put_bytes(0, str)
    ptr
  end

  # Copy a returned buffer to a binary String and free it. nil on the sentinel.
  def read_buffer(buffer)
    return nil if buffer[:ptr].null?

    begin
      buffer[:ptr].read_bytes(buffer[:len])
    ensure
      FFI.kine_buf_free(buffer)
    end
  end

  def last_error
    message = FFI.kine_last_error
    message unless message.nil? || message.empty?
  end
end
