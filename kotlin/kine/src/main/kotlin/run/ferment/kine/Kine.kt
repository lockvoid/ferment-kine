package run.ferment.kine

import com.sun.jna.NativeLong
import java.lang.ref.Cleaner
import java.lang.ref.Reference
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.doubleOrNull

/**
 * The installed sink, read by the C trampoline. Written once at startup
 * (`Kine.setLogSink`) before any render — no lock by contract.
 */
@Volatile
private var kineLogSink: ((Kine.LogLevel, String) -> Unit)? = null

/**
 * A file-level `val` so the core's function pointer is backed by an object that
 * lives as long as the process: a callback the JVM collects is a dangling
 * pointer the next log line jumps to.
 */
private val kineLogTrampoline = KineNative.LogCallback { level, message ->
    val sink = kineLogSink
    if (message == null || sink == null) return@LogCallback
    sink(Kine.LogLevel.fromRaw(level) ?: Kine.LogLevel.ERROR, message)
}

/** Unknown keys are ignored, as `JSONDecoder` ignores them for `ProbeResult`. */
private val kineJson = Json { ignoreUnknownKeys = true }

/** The `deinit` of [Kine.Document]: frees a handle whose wrapper became unreachable. */
private val kineCleaner: Cleaner = Cleaner.create()

/**
 * Thin Kotlin wrapper over the kine C ABI (`KineNative` / kine.h). Deliberately
 * no schema modeling in Kotlin — documents are authored as JSON and validated
 * by the core.
 */
object Kine {
    sealed class KineError(message: String) : Exception(message) {
        class Failed(message: String) : KineError(message)

        companion object {
            /** The core's thread-local last error, or [fallback] if none is set. */
            internal fun current(fallback: String): KineError =
                Failed(KineNative.INSTANCE.kine_last_error() ?: fallback)
        }
    }

    enum class LogLevel(val rawValue: Int) {
        INFO(0),
        WARN(1),
        ERROR(2);

        companion object {
            fun fromRaw(rawValue: Int): LogLevel? = entries.firstOrNull { it.rawValue == rawValue }
        }
    }

    /**
     * `CGRect`'s seat: an axis-aligned box, in design units or in fractions of a
     * canvas depending on who returned it.
     */
    data class Rect(val x: Double, val y: Double, val width: Double, val height: Double)

    /**
     * Install the process-wide log sink. The core reports every failure (the
     * `kine_last_error` content) and every font registration through it —
     * nothing in kine is allowed to fail silently once a sink is set. Install
     * ONCE at startup, before any render; the callback fires on whatever thread
     * the failing call runs on.
     */
    fun setLogSink(sink: (LogLevel, String) -> Unit) {
        kineLogSink = sink
        KineNative.INSTANCE.kine_set_log_callback(kineLogTrampoline)
    }

    /**
     * Register a font (TTF/OTF) into the process-global, bundled-only
     * collection. Idempotent; safe to call concurrently with renders.
     */
    fun registerFont(data: ByteArray) {
        val code = KineNative.INSTANCE.kine_register_font(data, NativeLong(data.size.toLong()))
        if (code != 0) throw KineError.current("font registration failed")
    }

    /**
     * Declare the render-fallback family: a document naming an unregistered
     * family renders in it (WARNED once per family through the log sink)
     * instead of failing the frame. Empty string clears back to strict. The
     * name must already resolve — the embedded "Inter" always does. The write
     * seams (probe `missingFonts`) stay strict either way.
     */
    fun setFallbackFamily(name: String) {
        val code = KineNative.INSTANCE.kine_set_fallback_family(name)
        if (code != 0) throw KineError.current("fallback family rejected")
    }

    /** Crate + schema version, e.g. "kine 0.1.0 (schema v1)". */
    val version: String get() = KineNative.INSTANCE.kine_version()

    /** A document's declared interface (from `probe`). */
    @Serializable
    data class ProbeResult(
        val version: Int,
        val size: Size,
        val inputs: List<Input>,
        val roles: List<String>,
        // Defaulted, not required: docs probed by an older core (pre-manifest) omit them.
        val assets: List<Asset> = emptyList(),
        /**
         * Font families the document references (literal `style.fontFamily`
         * values + the defaults of `fontFamily` inputs a style binds to), in
         * document order, deduped.
         */
        val fonts: List<String> = emptyList(),
        /**
         * Of [fonts], the families the process registry cannot serve at probe
         * time — fetch and register exactly these before rendering.
         */
        val missingFonts: List<String> = emptyList(),
    ) {
        @Serializable
        data class Size(val width: Double, val height: Double)

        @Serializable
        data class Input(val key: String, val type: String)

        /**
         * One embedded raster asset from the document manifest (§4). [kind] is
         * currently always "image"; [mime] is the container form
         * ("image/png"|"image/jpeg"|"image/webp"|"image/gif"|"image/apng");
         * [animated] is true when the decoded asset has more than one frame.
         */
        @Serializable
        data class Asset(val key: String, val kind: String, val mime: String, val animated: Boolean)
    }

    /**
     * An author's document admitted (SCHEMA §10): the text the host stores,
     * what the door repaired, and the document's interface (the probe's).
     */
    @Serializable
    data class Admission(
        val document: String,
        val repairs: List<Repair>,
        @SerialName("interface") val probe: ProbeResult,
    ) {
        @Serializable
        data class Repair(val path: String, val rule: String, val message: String)
    }

    /** Repair what has one reading, then validate (SCHEMA §10). A refusal throws the core's own words. */
    fun admit(json: String): Admission {
        val data = consume(KineNative.INSTANCE.kine_admit(json), "admission")
        return kineJson.decodeFromString(Admission.serializer(), data.decodeToString())
    }

    /**
     * One rendered frame: RGBA8, premultiplied alpha, sRGB, row-major.
     *
     * Not a `data class`: the iOS struct declares no equality, and a generated
     * `equals` over a [ByteArray] compares identity — a value type whose `==`
     * silently means `===` is worse than none.
     */
    class RenderedFrame(val data: ByteArray, val width: Int, val height: Int) {
        val stride: Int get() = width * 4
    }

    /**
     * A parsed, validated document. Renders every frame without re-parsing.
     * Thread-safe: the handle is immutable after construction and the core is
     * internally thread-safe (concurrent renders, RwLock-guarded font
     * registry) — so the wrapper carries no additional shared mutable state.
     *
     * [close] frees the handle and is idempotent; a use-after-close reports the
     * core's error rather than crashing. A document that becomes unreachable
     * without being closed is freed by a [Cleaner] — Kotlin's stand-in for the
     * Swift `deinit`, and non-deterministic, so `use { }` remains the door.
     */
    class Document(json: String) : AutoCloseable {
        private val handle: Long
        private val cleanable: Cleaner.Cleanable

        init {
            val handle = KineNative.INSTANCE.kine_document_create(json)
            if (handle == 0L) throw KineError.current("invalid document")
            this.handle = handle
            cleanable = kineCleaner.register(this, Free(handle))
        }

        /** Holds the handle alone: a cleaning action that captured the [Document] would pin it forever. */
        private class Free(private val handle: Long) : Runnable {
            override fun run() = KineNative.INSTANCE.kine_document_free(handle)
        }

        override fun close() = cleanable.clean()

        /**
         * Swift gets "`self` outlives the method body" from ARC; the JVM
         * promises no such thing. Once [handle] has been read this wrapper can
         * be unreachable, its [Cleaner] runs, and a render already in flight
         * comes back "invalid or freed document handle" — measured at iteration
         * ~300 of a hot loop with a collector running. Every native call that
         * takes the handle is made in here.
         */
        private inline fun <T> callingNative(body: () -> T): T =
            try {
                body()
            } finally {
                Reference.reachabilityFence(this)
            }

        fun probe(): ProbeResult {
            val data = callingNative {
                consume(KineNative.INSTANCE.kine_document_probe(handle), "probe")
            }
            return kineJson.decodeFromString(ProbeResult.serializer(), data.decodeToString())
        }

        /**
         * Union ink rect across [samples] frames over `[0, span]` seconds at a
         * small probe raster — design-box fractions. `null` = fully blank.
         * The crate's one definition of visual bounds (selection borders,
         * sticker normalization, preview trimming all read THIS).
         */
        fun inkUnion(
            signals: Map<String, Any?> = emptyMap(), samples: Int = 1, span: Double = 0.0,
            probeWidth: Int = 160, probeHeight: Int = 160,
        ): Rect? = inkUnionInCanvas(
            signals = signals, samples = samples, span = span,
            probeWidth = probeWidth, probeHeight = probeHeight,
        )?.ink

        /**
         * Ink probe payload: [ink] in fractions of the PROBE CANVAS, and the
         * probe canvas itself in design units — the doc box when everything
         * fits, the GROWN box ([layoutSize]) when text overflows. Map on-screen
         * boxes through [canvas], never the raw doc size: the pixels render on
         * the same grown canvas.
         */
        data class InkUnion(
            val ink: Rect,
            /**
             * Design-unit canvas the fractions speak: `y <= 0` when the box grew
             * upward; equals `(0, 0, docW, docH)` unfitted.
             */
            val canvas: Rect,
        )

        fun inkUnionInCanvas(
            signals: Map<String, Any?> = emptyMap(), samples: Int = 1, span: Double = 0.0,
            probeWidth: Int = 160, probeHeight: Int = 160,
        ): InkUnion? {
            val json = signalsJSON(signals)
            val data = callingNative {
                consume(
                    KineNative.INSTANCE.kine_document_ink_union(
                        handle, json, maxOf(1, samples), span, probeWidth, probeHeight,
                    ),
                    "ink probe",
                )
            }
            if (data.isEmpty()) return null
            val fields = kineJson.parseToJsonElement(data.decodeToString()) as? JsonObject ?: return null
            val x = fields.double("x") ?: return null
            val y = fields.double("y") ?: return null
            val width = fields.double("width") ?: return null
            val height = fields.double("height") ?: return null
            val canvasWidth = fields.double("canvasWidth") ?: return null
            val canvasHeight = fields.double("canvasHeight") ?: return null
            val canvasY = fields.double("canvasY") ?: return null
            return InkUnion(
                ink = Rect(x = x, y = y, width = width, height = height),
                canvas = Rect(x = 0.0, y = canvasY, width = canvasWidth, height = canvasHeight),
            )
        }

        /**
         * Render at time [t] with [signals] (JSON-encodable). [t] is sugar for
         * the document's `time` input; an explicit `time` signal wins.
         */
        fun renderRGBA(
            t: Double, signals: Map<String, Any?> = emptyMap(), width: Int, height: Int,
        ): RenderedFrame {
            val json = signalsJSON(signals)
            val data = callingNative {
                consume(KineNative.INSTANCE.kine_document_render_rgba(handle, t, json, width, height), "render")
            }
            return RenderedFrame(data = data, width = width, height = height)
        }

        /**
         * The grown canvas the document's TEXT content needs with these
         * signals, in design units: `width` = doc width (lines wrap),
         * `height >=` doc height, and `y <= 0` = the grown canvas's top in doc
         * coordinates. Doc-sized rect at y 0 = everything fits. The crate's one
         * definition of text overflow — size render targets and selection boxes
         * from THIS, then render through [renderRGBAViewport].
         */
        fun layoutSize(signals: Map<String, Any?> = emptyMap()): Rect {
            val json = signalsJSON(signals)
            val data = callingNative {
                consume(KineNative.INSTANCE.kine_document_layout_size(handle, json), "layout size")
            }
            val fields = kineJson.parseToJsonElement(data.decodeToString()) as? JsonObject
            val width = fields?.double("width")
            val height = fields?.double("height")
            val y = fields?.double("y")
            if (width == null || height == null || y == null) {
                throw KineError.current("layout size decode failed")
            }
            return Rect(x = 0.0, y = y, width = width, height = height)
        }

        /**
         * [renderRGBA] over a vertical design-space viewport — pass
         * [layoutSize]'s `y`/`height` (with a matching-aspect target) to render
         * text that overflows the doc canvas instead of cropping it.
         */
        fun renderRGBAViewport(
            t: Double, signals: Map<String, Any?> = emptyMap(), width: Int, height: Int,
            viewY: Double, viewHeight: Double,
        ): RenderedFrame {
            val json = signalsJSON(signals)
            val data = callingNative {
                consume(
                    KineNative.INSTANCE.kine_document_render_rgba_viewport(
                        handle, t, json, width, height, viewY, viewHeight,
                    ),
                    "viewport render",
                )
            }
            return RenderedFrame(data = data, width = width, height = height)
        }
    }

    /**
     * Whether this build carries the GPU raster flavor. False here by
     * construction: the flavor is vello_hybrid over wgpu/Metal, so the host and
     * Android builds `kotlin/build.sh` produces are CPU-only and carry no
     * `kine_gpu_*` entry point beyond this one. Read through the ABI rather
     * than hard-coded because the header promises exactly that.
     */
    val gpuAvailable: Boolean get() = KineNative.INSTANCE.kine_gpu_available() == 1
}

/** Copy a `kine_buf` into a [ByteArray] and free it. Throws the core's last error on the null sentinel. */
private fun consume(buffer: KineBuf, what: String): ByteArray {
    val pointer = buffer.ptr ?: throw Kine.KineError.current("$what failed")
    val length = buffer.len.toLong().toInt()
    return try {
        // A zero-length buffer (a blank document's ink union) carries a dangling
        // non-null pointer by Rust's empty-slice rule — nothing to read from it.
        if (length == 0) ByteArray(0) else pointer.getByteArray(0, length)
    } finally {
        KineNative.INSTANCE.kine_buf_free(buffer)
    }
}

private fun signalsJSON(signals: Map<String, Any?>): String =
    if (signals.isEmpty()) "{}" else signals.toJsonObject().toString()

private fun Map<*, *>.toJsonObject(): JsonObject = JsonObject(
    entries.associate { (key, value) ->
        val name = key as? String ?: throw Kine.KineError.Failed("signal keys must be strings, got $key")
        name to value.toJsonElement()
    },
)

/**
 * The encoding `JSONSerialization` gives the Swift wrapper: numbers as numbers,
 * no NaN, no infinity, and anything it cannot represent is an error rather than
 * a silently mangled signal.
 */
private fun Any?.toJsonElement(): JsonElement = when (this) {
    null -> JsonNull
    is Boolean -> JsonPrimitive(this)
    is Number -> JsonPrimitive(finite(this))
    is String -> JsonPrimitive(this)
    is Map<*, *> -> toJsonObject()
    is Iterable<*> -> JsonArray(map { it.toJsonElement() })
    is Array<*> -> JsonArray(map { it.toJsonElement() })
    else -> throw Kine.KineError.Failed("signal value of type ${this::class.qualifiedName} is not JSON")
}

/**
 * The number is encoded through its own `toString`, not widened: a `Float`
 * signal must reach the core as `0.1`, the way `NSNumber` writes it on iOS, not
 * as the `0.10000000149011612` a widening to `Double` produces.
 */
private fun finite(value: Number): Number {
    if (!value.toDouble().isFinite()) {
        throw Kine.KineError.Failed("signal value $value is not a finite number")
    }
    return value
}

private fun JsonObject.double(key: String): Double? = (this[key] as? JsonPrimitive)?.doubleOrNull
