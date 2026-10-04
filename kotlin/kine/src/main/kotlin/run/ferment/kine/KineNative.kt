package run.ferment.kine

import com.sun.jna.Callback
import com.sun.jna.Library
import com.sun.jna.Native
import com.sun.jna.NativeLong
import com.sun.jna.Pointer
import com.sun.jna.Structure

/**
 * Owned byte buffer handed to the caller. `{NULL, 0}` signals failure — call
 * [KineNative.kine_last_error]. Any non-null buffer must be released exactly
 * once with [KineNative.kine_buf_free].
 *
 * Returned BY VALUE across the ABI, hence [Structure.ByValue]; `len` is
 * `size_t`, which is 64-bit on every target this repo builds (macOS arm64,
 * Android arm64-v8a).
 */
@Structure.FieldOrder("ptr", "len")
internal class KineBuf : Structure(), Structure.ByValue {
    @JvmField var ptr: Pointer? = null
    @JvmField var len: NativeLong = NativeLong(0)
}

/**
 * The extern-C boundary of the kine core, 1:1 with `crate/include/kine.h`
 * — the single source of truth every host binding links against. Methods carry
 * the C symbol names because JNA resolves by name.
 *
 * The GPU entry points exist only in a GPU build. The Metal ones
 * (`kine_gpu_engine_create`, `kine_gpu_render_document`,
 * `kine_gpu_render_document_viewport`) are Apple-only and not declared here;
 * the GLES ones are, and only the Android build (`kotlin/build.sh android`)
 * carries them. JNA looks a symbol up on its method's first call, so declaring
 * them costs the host's CPU build nothing as long as callers branch on
 * [kine_gpu_available] first — which is why the header promises that one is
 * ALWAYS present.
 *
 * Threading (the header's contract): document handles are immutable after
 * creation, so concurrent renders from any thread are safe; the font collection
 * is process-global and [kine_register_font] is idempotent; [kine_last_error]
 * is thread-local.
 */
internal interface KineNative : Library {

    /**
     * Host log sink. Levels: 0 info, 1 warn, 2 error. May fire on ANY thread;
     * the message is valid only for the duration of the call, so JNA's copy
     * into a [String] is the only form that outlives it.
     *
     * Nested so JNA reaches this library's [Library.OPTION_STRING_ENCODING]
     * when it decodes `message`: it resolves a callback's encoding by walking
     * the declaring class, and a top-level interface has none — such a callback
     * decodes with the ambient default charset instead of the UTF-8 the ABI
     * promises. Measured under `-Djna.encoding=ISO-8859-1`: the same error text
     * arrived intact through `kine_last_error` and as mojibake through the sink.
     */
    fun interface LogCallback : Callback {
        fun invoke(level: Int, message: String?)
    }

    /** Register a host log sink. NULL unregisters. */
    fun kine_set_log_callback(callback: LogCallback?)

    /** Register a font (TTF/OTF) into the bundled-only collection. 0 on success, -1 on error. */
    fun kine_register_font(bytes: ByteArray, len: NativeLong): Int

    /** Declare the render-fallback family; `""` clears back to strict. 0 on success, -1 on error. */
    fun kine_set_fallback_family(name: String): Int

    /** Crate + schema version, e.g. "kine 0.1.0 (schema v1)". Static; never freed. */
    fun kine_version(): String

    /** One-shot: render a v1 motion document at `t` into PNG bytes (straight alpha). */
    fun kine_render_document(
        doc_json: String, t: Double, signals_json: String?, width: Int, height: Int,
    ): KineBuf

    /** One-shot: render into raw RGBA8 (premultiplied, sRGB, stride = width*4). */
    fun kine_render_document_rgba(
        doc_json: String, t: Double, signals_json: String?, width: Int, height: Int,
    ): KineBuf

    /** One-shot: parse + validate a document and describe its interface as JSON. */
    fun kine_probe(doc_json: String): KineBuf

    /**
     * Admit an AUTHOR's document (SCHEMA §10): repair what has one reading,
     * then validate. `{ "document", "repairs": [{ "path", "rule", "message" }],
     * "interface" }`; the document is the author's own text when nothing was
     * repaired.
     */
    fun kine_admit(doc_json: String): KineBuf

    /** Parse + validate a document and keep it as a reusable handle (> 0), or 0 on error. */
    fun kine_document_create(doc_json: String): Long

    /** Describe a handle's interface (same payload as [kine_probe]). */
    fun kine_document_probe(handle: Long): KineBuf

    /** Render a handle at `t` into raw RGBA8 (premultiplied, sRGB, stride = width*4). */
    fun kine_document_render_rgba(
        handle: Long, t: Double, signals_json: String?, width: Int, height: Int,
    ): KineBuf

    /** The grown canvas the document's TEXT content needs — JSON `{"width","height","y"}`. */
    fun kine_document_layout_size(handle: Long, signals_json: String?): KineBuf

    /** [kine_document_render_rgba] over a vertical design-space viewport. */
    fun kine_document_render_rgba_viewport(
        handle: Long, t: Double, signals_json: String?, width: Int, height: Int,
        view_y: Double, view_h: Double,
    ): KineBuf

    /** Union INK rect across `samples` frames over `[0, span]` seconds; empty buffer = fully blank. */
    fun kine_document_ink_union(
        handle: Long, signals_json: String?, samples: Int, span: Double, width: Int, height: Int,
    ): KineBuf

    /** One-shot ink union for hosts without the handle lifecycle — compiles per call. */
    fun kine_ink_union(
        doc_json: String, signals_json: String?, samples: Int, span: Double, width: Int, height: Int,
    ): KineBuf

    /** Free a document handle. Idempotent; a use-after-free reports an error. */
    fun kine_document_free(handle: Long)

    /** 1 when this build carries the GPU flavor, 0 otherwise. Always present. */
    fun kine_gpu_available(): Int

    /** GLES flavor: the engine on the calling thread's current EGL context (> 0), or 0 on error. */
    fun kine_gpu_gles_engine_create(): Long

    /** Destroy a GPU engine, its context current. Idempotent. GPU builds only. */
    fun kine_gpu_engine_destroy(engine: Long)

    /** GLES flavor: render a handle at `t` into a host-owned GL texture. 0 on success, -1 on error. */
    fun kine_gpu_gles_render_document(
        engine: Long, document: Long, t: Double, signals_json: String?, width: Int, height: Int,
        gl_texture: Int,
    ): Int

    /** [kine_gpu_gles_render_document] over a vertical design-space viewport. */
    fun kine_gpu_gles_render_document_viewport(
        engine: Long, document: Long, t: Double, signals_json: String?, width: Int, height: Int,
        view_y: Double, view_h: Double, gl_texture: Int,
    ): Int

    /** Release a buffer returned by any `kine_*` function above. */
    fun kine_buf_free(buf: KineBuf)

    /** Last error message on the calling thread, or null. */
    fun kine_last_error(): String?

    companion object {
        /**
         * `libkine.dylib` on the host (found through `jna.library.path`, which
         * `:kine`'s test task points at `kine/build/host`), `libkine.so`
         * from the APK on Android. UTF-8 is pinned rather than inherited: the
         * ABI's JSON arguments are NUL-terminated UTF-8 by contract.
         */
        val INSTANCE: KineNative = Native.load(
            "kine",
            KineNative::class.java,
            mapOf(Library.OPTION_STRING_ENCODING to "UTF-8"),
        )
    }
}
