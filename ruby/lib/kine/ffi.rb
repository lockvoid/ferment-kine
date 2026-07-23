# frozen_string_literal: true

require "ffi"

module Kine
  # 1:1 binding to the extern-C surface in crate/include/kine.h. Everything of
  # substance is in the Rust core; this only loads the library and mirrors the
  # ABI. Higher-level ergonomics live in Kine (lib/kine.rb).
  module FFI
    extend ::FFI::Library

    # Mirror of `kine_buf` — {NULL, 0} signals an error.
    class Buf < ::FFI::Struct
      layout :ptr, :pointer,
             :len, :size_t
    end

    # Resolution order: explicit env override, then the library staged next to
    # this file by ext/kine/extconf.rb (built at gem install), then the loader.
    def self.candidate_paths
      here = __dir__ # lib/kine
      [
        ENV["KINE_LIB"],
        File.join(here, "libkine.dylib"),
        File.join(here, "libkine.so"),
        "kine",
      ].compact
    end

    resolved = candidate_paths.find do |path|
      ffi_lib(path)
      true
    rescue LoadError
      false
    end
    unless resolved
      raise LoadError, "libkine not found — set KINE_LIB or reinstall the gem " \
                       "(tried: #{candidate_paths.join(', ')})"
    end
    LIBRARY_PATH = resolved

    attach_function :kine_register_font, [:pointer, :size_t], :int32
    # blocking: renders can be long; release the GVL so other Ruby threads run.
    attach_function :kine_render_document,
                    [:pointer, :double, :pointer, :uint32, :uint32],
                    Buf.by_value, blocking: true
    attach_function :kine_ink_union,
                    [:pointer, :pointer, :uint32, :double, :uint32, :uint32],
                    Buf.by_value
    attach_function :kine_probe, [:pointer], Buf.by_value
    attach_function :kine_buf_free, [Buf.by_value], :void
    attach_function :kine_last_error, [], :string
  end
end
