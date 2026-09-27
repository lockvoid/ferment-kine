package run.ferment.kine

import java.awt.image.BufferedImage
import java.awt.image.DataBufferInt
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import javax.imageio.ImageIO
import kotlin.math.abs
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertTrue
import org.junit.BeforeClass
import org.junit.Test

/**
 * Port of `swift/Tests/KineTests/KineTests.swift` (the CPU
 * half). The GPU classes there — `KineGPUTests`, `KineGPUMemoryProbe` — have no
 * counterpart: their flavor is vello_hybrid over Metal, and the builds
 * `kotlin/build.sh` produces carry no `kine_gpu_*` entry point to call.
 */
class KineTests {

    companion object {
        private fun resource(name: String): ByteArray =
            KineTests::class.java.getResourceAsStream("/$name")?.use { it.readBytes() }
                ?: error("missing test resource $name")

        val fontData: ByteArray = resource("font.ttf")
        val cardJSON: String = resource("test_card.json").decodeToString()

        /** Names a family nothing registers: a hard error under strict, a fallback render under one. */
        val unregisteredFamilyJSON: String = """
            { "version": 1, "size": { "width": 64, "height": 64 },
              "root": { "kind": "text", "key": "t", "content": "X",
                "frame": { "x": 0, "y": 0, "width": 64, "height": 64 },
                "style": { "fontFamily": "Nope Sans", "size": 20, "fill": "#FFFFFF" } } }
            """.trimIndent()

        /** Process-global and idempotent, so once per JVM rather than once per test. */
        @BeforeClass
        @JvmStatic
        fun registerFont() {
            Kine.registerFont(fontData)
        }
    }

    /** kill: misspell the symbol in `KineNative` (`kine_versionn`) — `UnsatisfiedLinkError`, and this is where a broken load reports first. */
    @Test
    fun version() {
        assertTrue(Kine.version.startsWith("kine "), Kine.version)
        assertTrue(Kine.version.contains("schema v1"), Kine.version)
    }

    /** kill: decode the admission as a `ProbeResult` — the document and its repairs never reach the host. */
    @Test
    fun admitRepairsWhatHasOneReadingAndSaysSo() {
        val windowed = """
            { "version": 1, "size": { "width": 64, "height": 64 },
              "inputs": [ { "key": "inProgress", "type": "unit", "default": 0 } ],
              "root": { "kind": "shape", "key": "dot",
                "geometry": { "kind": "ellipse", "cx": 32, "cy": 32, "rx": 20, "ry": 20 },
                "fill": { "kind": "solid", "color": "#FF0000" } },
              "animators": [ { "target": "dot", "property": "opacity", "driver": "inProgress",
                "keyframes": [ { "at": 0.2, "value": 0 }, { "at": 0.6, "value": 1 } ] } ] }
            """.trimIndent()
        val admission = Kine.admit(windowed)
        assertEquals(listOf("keyframes-span", "settled-envelope"), admission.repairs.map { it.rule })
        assertEquals(64.0, admission.probe.size.width)
        assertEquals(listOf("inProgress"), Kine.Document(admission.document).use { it.probe() }.inputs.map { it.key })
    }

    @Test
    fun admitHandsAValidDocumentBackAsWritten() {
        val admission = Kine.admit(cardJSON)
        assertEquals(cardJSON, admission.document)
        assertTrue(admission.repairs.isEmpty())
    }

    @Test
    fun admitRefusesInTheCoresWords() {
        val pixels = """
            { "version": 1, "size": { "width": 1080, "height": 1920 },
              "root": { "kind": "group", "key": "root", "transform": { "anchorX": 540 }, "children": [] } }
            """.trimIndent()
        val error = assertFailsWith<Kine.KineError> { Kine.admit(pixels) }
        assertTrue(error.message!!.contains("root.transform.anchorX: 540 is outside 0..1"), error.message)
    }

    /** kill: read the `layout_size` buffer in `probe()` — the two entry points return different JSON through the same `consume`, and only the decode catches the swap. */
    @Test
    fun probeReportsInterface() {
        Kine.Document(cardJSON).use { document ->
            val probe = document.probe()
            assertEquals(1, probe.version)
            assertEquals(512.0, probe.size.width, 0.0)
            assertEquals(listOf("card", "text"), probe.roles)
            assertEquals(
                listOf(
                    "time", "progress", "activations", "font",
                    "foreground", "background", "accent", "borderColor",
                ),
                probe.inputs.map { it.key },
            )
        }
    }

    /**
     * The card's text binds `fontFamily` to the "font" input, whose default is
     * the registered test face — referenced, nothing missing.
     *
     * kill: swap the `@SerialName`s of `fonts` and `missingFonts` — both lists
     * stay plausible and only this test's two assertions disagree. (Halving the
     * `size_t` length in `registerFont` also goes red, but through the class
     * fixture: the core rejects the truncated face outright.)
     */
    @Test
    fun probeManifestsReferencedAndMissingFonts() {
        Kine.Document(cardJSON).use { document ->
            val probe = document.probe()
            assertEquals(listOf("Bebas Neue"), probe.fonts)
            assertEquals(emptyList(), probe.missingFonts)
        }

        val restyled = cardJSON.replace("Bebas Neue", "Missing Grotesk")
        Kine.Document(restyled).use { document ->
            val missing = document.probe()
            assertEquals(listOf("Missing Grotesk"), missing.fonts)
            assertEquals(listOf("Missing Grotesk"), missing.missingFonts)
        }
    }

    /**
     * The iOS twin varies `t` AND `progress`, so it stays green on a binding
     * that drops signals entirely (measured: it does). One `t`, two `progress`
     * values is the same test with only the claimed variable moving.
     *
     * kill: return `"{}"` from `signalsJSON` regardless of the map — both
     * renders fall back to the declared default and come back identical.
     */
    @Test
    fun renderRGBADiffersBySignals() {
        Kine.Document(cardJSON).use { document ->
            val a = document.renderRGBA(t = 0.5, signals = mapOf("progress" to 0.0), width = 128, height = 128)
            val b = document.renderRGBA(t = 0.5, signals = mapOf("progress" to 0.8), width = 128, height = 128)
            assertEquals(128 * 128 * 4, a.data.size)
            assertEquals(128 * 4, a.stride)
            assertFalse(a.data.contentEquals(b.data), "different signals produced identical pixels")
        }
    }

    /**
     * kill: bind `kine_last_error` as anything but a `const char*` (or forget it
     * on the create path) and the message is the wrapper's own fallback instead
     * of the core's parse error.
     */
    @Test
    fun malformedDocumentThrows() {
        val error = assertFailsWith<Kine.KineError> { Kine.Document("{ not json") }
        val message = assertNotNull(error.message, "error should carry the core's message")
        assertTrue(message.isNotEmpty(), "error should carry the core's message")
        assertFalse(message == "invalid document", "fallback text means kine_last_error never crossed: $message")
    }

    /** kill: swallow the null-buffer sentinel in `consume` and a failed render returns an empty frame instead of throwing. */
    @Test
    fun unregisteredFontIsRenderError() {
        Kine.Document(unregisteredFamilyJSON).use { document ->
            val error = assertFailsWith<Kine.KineError> {
                document.renderRGBA(t = 0.0, width = 64, height = 64)
            }
            assertTrue(assertNotNull(error.message).contains("not registered"), "${error.message}")
        }
    }

    /**
     * The crate's own answer, byte-pinned: `card_t05_p05.png` is the golden the
     * Rust suite asserts against, and the host dylib this binding loads is the
     * same build that produced it.
     *
     * Two gates. The Swift file's mean tolerance, ported as it stands, absorbs
     * the PNG (straight) → premultiplied round trip; measured, that round trip
     * costs nothing here (mean 0.0) and the tolerance is loose enough to pass a
     * render with `progress` missing (mean 1.16). So the crate's own parity
     * gate — no more than 0.05% of pixels off by more than 8/255, the number
     * kine.h states for GPU-vs-CPU — carries the weight.
     *
     * kill: lose `progress` in the signal encoder: 4.1% of the pixels move,
     * eighty times the gate.
     */
    @Test
    fun matchesCrateGolden() {
        Kine.Document(cardJSON).use { document ->
            val frame = document.renderRGBA(
                t = 0.5, signals = mapOf("progress" to 0.5), width = 512, height = 512,
            )
            val golden = goldenPremultipliedRGBA("card_t05_p05.png", width = 512, height = 512)
            assertEquals(golden.size, frame.data.size)

            var total = 0.0
            for (i in golden.indices) {
                total += abs((frame.data[i].toInt() and 0xFF) - (golden[i].toInt() and 0xFF))
            }
            val meanAbsDiff = total / golden.size
            assertTrue(meanAbsDiff < 2.0, "Kotlin render diverged from crate golden (mean |Δ| = $meanAbsDiff)")

            var off = 0
            for (i in golden.indices step 4) {
                var worst = 0
                for (k in 0 until 4) {
                    worst = maxOf(worst, abs((frame.data[i + k].toInt() and 0xFF) - (golden[i + k].toInt() and 0xFF)))
                }
                if (worst > 8) off++
            }
            val offPercent = off * 100.0 / (512.0 * 512.0)
            assertTrue(offPercent <= 0.05, "$offPercent% of pixels diverged from the crate golden by more than 8/255")
        }
    }

    /** Decode a golden PNG into premultiplied RGBA8 — the CoreGraphics step of the Swift test. */
    private fun goldenPremultipliedRGBA(name: String, width: Int, height: Int): ByteArray {
        val decoded = KineTests::class.java.getResourceAsStream("/$name")?.use { ImageIO.read(it) }
            ?: error("cannot decode golden $name")
        val premultiplied = BufferedImage(width, height, BufferedImage.TYPE_INT_ARGB_PRE)
        val graphics = premultiplied.createGraphics()
        graphics.drawImage(decoded, 0, 0, null)
        graphics.dispose()

        val pixels = (premultiplied.raster.dataBuffer as DataBufferInt).data
        val bytes = ByteArray(width * height * 4)
        for (i in pixels.indices) {
            val argb = pixels[i]
            bytes[i * 4] = (argb ushr 16).toByte()
            bytes[i * 4 + 1] = (argb ushr 8).toByte()
            bytes[i * 4 + 2] = argb.toByte()
            bytes[i * 4 + 3] = (argb ushr 24).toByte()
        }
        return bytes
    }

    // MARK: - The binding's own contracts (no Swift twin: `deinit`, the C module
    // and the Metal flavor cover these on iOS)

    /**
     * `close` is the seat of the Swift `deinit`: it frees, twice is a no-op, and
     * the core answers a freed handle with an error rather than a crash.
     *
     * kill: make `close` a no-op (`override fun close() = Unit`) — the render
     * after it succeeds and the freed-handle assertion never fires. Freeing
     * directly instead of through `Cleanable.clean` does NOT turn it red: the
     * core's free is idempotent, so the double free is invisible here and only
     * the cleaner's at-most-once contract keeps it that way.
     */
    @Test
    fun useAfterCloseReportsAnError() {
        val document = Kine.Document(cardJSON)
        assertEquals(64 * 64 * 4, document.renderRGBA(t = 0.0, width = 64, height = 64).data.size)

        document.close()
        document.close()

        val error = assertFailsWith<Kine.KineError> {
            document.renderRGBA(t = 0.0, width = 64, height = 64)
        }
        assertTrue(assertNotNull(error.message).contains("freed"), "${error.message}")
    }

    /**
     * The ABI's promise: every failure recorded in `kine_last_error` is also
     * reported to the host sink (the crate asserts the same from Rust). What
     * makes it a binding test is the callback's lifetime — a JNA `Callback` the
     * JVM collects leaves the core holding a dangling function pointer.
     *
     * kill: make `kineLogTrampoline` a local (or a `by lazy` released after
     * install) — the sink stops firing once GC runs, and this fails.
     */
    @Test
    fun logSinkReceivesFailures() {
        val lines = CopyOnWriteArrayList<Pair<Kine.LogLevel, String>>()
        Kine.setLogSink { level, message -> lines += level to message }

        Kine.Document(cardJSON).use { document ->
            assertFailsWith<Kine.KineError> { document.renderRGBA(t = 0.0, width = 0, height = 0) }
        }

        assertTrue(
            lines.any { (level, message) -> level == Kine.LogLevel.ERROR && message.contains("non-zero") },
            "the core's failure never reached the sink: $lines",
        )
    }

    /**
     * kine.h on `kine_document_render_rgba_viewport`: "(0, doc height) is
     * exactly the plain render". Byte-identity in the crate by construction, so
     * what this grades is the marshalling — six arguments, two of them doubles
     * after two ints.
     *
     * kill: swap `viewY`/`viewHeight` at the call site, or declare either as
     * `Float` in `KineNative` — the viewport frame stops matching the plain one.
     */
    @Test
    fun viewportOverTheDocCanvasIsThePlainRender() {
        Kine.Document(cardJSON).use { document ->
            val signals = mapOf("progress" to 0.5)
            val plain = document.renderRGBA(t = 0.25, signals = signals, width = 128, height = 128)
            val viewport = document.renderRGBAViewport(
                t = 0.25, signals = signals, width = 128, height = 128,
                viewY = 0.0, viewHeight = 512.0,
            )
            assertTrue(
                plain.data.contentEquals(viewport.data),
                "(0, doc height) must be exactly the plain render",
            )
        }
    }

    /**
     * A 64×64 document holding one 48×48 rect at (8, 8): nothing to overflow, so
     * the grown canvas IS the design box, and the ink covers the middle 75% of
     * both axes. The numbers come from the document, not from a previous run.
     *
     * kill: read `canvasY` into `canvas.height` (or `y` into `x`) in
     * `inkUnionInCanvas` — every field lands somewhere plausible and only the
     * geometry catches it.
     */
    @Test
    fun inkUnionMeasuresTheDrawnRectInTheDocCanvas() {
        val json = """
            { "version": 1, "size": { "width": 64, "height": 64 },
              "root": { "kind": "shape", "key": "box",
                "geometry": { "kind": "rect", "x": 8, "y": 8, "width": 48, "height": 48 },
                "fill": { "kind": "solid", "color": "#FFFFFF" } } }
            """.trimIndent()
        Kine.Document(json).use { document ->
            val layout = document.layoutSize()
            assertEquals(64.0, layout.width, 0.0)
            assertEquals(64.0, layout.height, 0.0)
            assertEquals(0.0, layout.y, 0.0)

            val union = assertNotNull(document.inkUnionInCanvas(), "a drawn rect is not blank")
            assertEquals(layout.width, union.canvas.width, 0.0)
            assertEquals(layout.height, union.canvas.height, 0.0)
            assertEquals(layout.y, union.canvas.y, 0.0)

            // One probe pixel of slack (the raster is 160×160) and no more.
            val slack = 1.0 / 160.0
            assertEquals(0.125, union.ink.x, slack)
            assertEquals(0.125, union.ink.y, slack)
            assertEquals(0.75, union.ink.width, slack)
            assertEquals(0.75, union.ink.height, slack)
            assertEquals(union.ink, document.inkUnion())
        }
    }

    /**
     * A blank document has no ink — the empty (len 0, non-null pointer) buffer
     * the ABI returns for it, which is not the `{NULL, 0}` failure sentinel.
     *
     * kill: treat a zero-length buffer as failure in `consume` — a blank
     * document throws instead of answering "no ink", and the sticker/preview
     * trimming above it breaks on exactly the input it exists to handle.
     */
    @Test
    fun blankDocumentHasNoInk() {
        val json = """
            { "version": 1, "size": { "width": 64, "height": 64 },
              "root": { "kind": "group", "key": "empty", "children": [] } }
            """.trimIndent()
        Kine.Document(json).use { document ->
            assertEquals(null, document.inkUnion())
            assertEquals(null, document.inkUnionInCanvas())
        }
    }

    /**
     * Strict is the default: an unregistered family is a hard render error
     * (`unregisteredFontIsRenderError`). With a fallback declared, the same
     * document renders and the miss is WARNED through the sink instead.
     *
     * kill: bind `kine_set_fallback_family` as returning `void` and a rejected
     * family passes silently, leaving the process in strict mode while the
     * caller believes otherwise.
     */
    @Test
    fun fallbackFamilyRendersInsteadOfFailing() {
        Kine.Document(unregisteredFamilyJSON).use { document ->
            try {
                Kine.setFallbackFamily("Inter")
                val frame = document.renderRGBA(t = 0.0, width = 64, height = 64)
                assertEquals(64 * 64 * 4, frame.data.size)
                assertTrue(frame.data.any { it != 0.toByte() }, "the fallback face drew nothing")
            } finally {
                Kine.setFallbackFamily("")
            }

            // Back to strict, which is the state every other test assumes.
            assertFailsWith<Kine.KineError> { document.renderRGBA(t = 0.0, width = 64, height = 64) }
        }
        assertFailsWith<Kine.KineError> { Kine.setFallbackFamily("Nope Sans") }
    }

    /**
     * Every signal shape a real caller sends. iOS builds them in
     * `KineSignals.swift` and they are exactly four: a number, a string, a
     * colour (also a string) and a unit array — and the card declares an input
     * of each. Until this landed, the whole suite passed two shapes, `{}` and
     * one `Double`, so the encoder's other branches were carried by nothing.
     *
     * kill (proven): delete the `is String` or `is Iterable` branch of
     * `toJsonElement` — the signal that used it hits `else` and throws; drop
     * the `finite` guard and the NaN message becomes the core's parse error;
     * accept any key via `toString()` and the numeric key stops being refused.
     */
    @Test
    fun everySignalShapeReachesTheCore() {
        Kine.Document(cardJSON).use { document ->
            fun frame(signals: Map<String, Any?>) =
                document.renderRGBA(t = 0.5, signals = signals, width = 96, height = 96).data

            val base = frame(mapOf("progress" to 0.5))
            assertFalse(
                base.contentEquals(frame(mapOf("progress" to 0.5, "accent" to "#00FF00"))),
                "a String signal never reached the core",
            )
            assertFalse(
                base.contentEquals(frame(mapOf("progress" to 0.5, "activations" to listOf(1.0, 1.0, 1.0)))),
                "a List<Double> signal never reached the core",
            )

            // Non-finite numbers are refused HERE, as JSONSerialization refuses them on iOS —
            // the message is the assertion, because without the guard the core rejects the
            // malformed JSON instead and the caller still gets a KineError, just a worse one
            // ("invalid signals JSON: expected value at line 1 column 13").
            for (bad in listOf(Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY)) {
                val error = assertFailsWith<Kine.KineError> { frame(mapOf("progress" to bad)) }
                assertTrue(
                    assertNotNull(error.message).contains("not a finite number"),
                    "$bad was passed to the core instead of being refused: ${error.message}",
                )
            }
            val inArray = assertFailsWith<Kine.KineError> { frame(mapOf("activations" to listOf(Double.NaN))) }
            assertTrue(assertNotNull(inArray.message).contains("not a finite number"), "${inArray.message}")

            // A value JSON has no shape for, and a key that is not a String.
            assertFailsWith<Kine.KineError> { frame(mapOf("progress" to java.io.File("/tmp"))) }
            @Suppress("UNCHECKED_CAST")
            val numericKey = mapOf(1 to 0.5) as Map<String, Any?>
            assertFailsWith<Kine.KineError> { frame(numericKey) }
        }
    }

    /**
     * `kine_last_error` is thread-local: the message belongs to the thread whose
     * call failed. Two failure classes are interleaved across eight threads, and
     * each thread must read the message for ITS OWN failure — a shared last
     * error would hand back whichever failure landed most recently.
     *
     * kill (proven, 3 lines): route the FFI call through a single-thread
     * executor while reading the error on the caller's thread — a plausible
     * "make the native library safe" move. Measured: 200 of 200 messages cross,
     * all of them degraded to the wrapper's `"render failed"` fallback, because
     * the failure happened on a thread nobody reads.
     */
    @Test
    fun concurrentFailuresDoNotCrossThreads() {
        Kine.Document(cardJSON).use { document ->
            val pool = Executors.newFixedThreadPool(8)
            // Futures, not fire-and-forget: an assertion that fails on a pool
            // thread is captured by its Future and would never reach the runner.
            val runs = (0 until 200).map { i ->
                val zeroSized = i % 2 == 0
                pool.submit {
                    val error = assertFailsWith<Kine.KineError> {
                        if (zeroSized) document.renderRGBA(t = 0.0, width = 0, height = 8)
                        else document.renderRGBA(t = 0.0, width = 100_000, height = 100_000)
                    }
                    val expected = if (zeroSized) "non-zero" else "exceeds"
                    val message = assertNotNull(error.message)
                    assertTrue(message.contains(expected), "got '$message', wanted '$expected'")
                }
            }
            runs.forEach { it.get(60, TimeUnit.SECONDS) }
            pool.shutdown()
        }
    }

    /**
     * The one-shot family — `kine_probe`, `kine_render_document`,
     * `kine_render_document_rgba`, `kine_ink_union` — is declared because
     * `KineNative` mirrors the header, and used by nothing: the Swift wrapper
     * takes the handle path for all four. Declared and never executed is how a
     * wrong signature waits for the next lane.
     *
     * Every comparison is against the handle path, which the rest of the suite
     * pins, and every raster is NON-SQUARE over ASYMMETRIC ink — with a square
     * one, swapping `width` and `height` in a declaration changes neither the
     * byte count nor the answer, and the test proves only that the symbol
     * resolves (measured: it did, three times).
     *
     * kill (proven): swap `width`/`height` in the `kine_render_document_rgba`
     * declaration, or `samples`/`width` in `kine_ink_union` — the one-shot
     * stops matching the handle path.
     */
    @Test
    fun theOneShotFamilyMatchesTheHandlePath() {
        val native = KineNative.INSTANCE
        // Ink off-centre in both axes, so x/y and width/height are all distinguishable.
        val json = """
            { "version": 1, "size": { "width": 64, "height": 64 },
              "root": { "kind": "shape", "key": "bar",
                "geometry": { "kind": "rect", "x": 8, "y": 24, "width": 48, "height": 16 },
                "fill": { "kind": "solid", "color": "#FFFFFF" } } }
            """.trimIndent()

        assertTrue(consumeForTest(native.kine_probe(json)).decodeToString().contains("\"missingFonts\""))

        val png = consumeForTest(native.kine_render_document(json, 0.0, "{}", 48, 80))
        assertEquals(
            listOf(0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A),
            png.take(8).map { it.toInt() and 0xFF },
            "kine_render_document must return PNG where its _rgba twin returns raw pixels",
        )

        Kine.Document(json).use { document ->
            val oneShotPixels = consumeForTest(native.kine_render_document_rgba(json, 0.0, "{}", 48, 80))
            val handlePixels = document.renderRGBA(t = 0.0, width = 48, height = 80).data
            assertTrue(
                oneShotPixels.contentEquals(handlePixels),
                "the one-shot render disagrees with the handle path",
            )

            val oneShotInk = consumeForTest(native.kine_ink_union(json, "{}", 1, 0.0, 40, 80)).decodeToString()
            val union = assertNotNull(
                document.inkUnionInCanvas(samples = 1, span = 0.0, probeWidth = 40, probeHeight = 80),
            )
            for ((key, value) in listOf(
                "x" to union.ink.x, "y" to union.ink.y,
                "width" to union.ink.width, "height" to union.ink.height,
            )) {
                assertTrue(
                    oneShotInk.contains("\"$key\":$value"),
                    "one-shot ink $key missing or different: $oneShotInk vs $union",
                )
            }
        }
    }

    /** The wrapper's own `consume` is file-private; the ABI tests need the same two steps. */
    private fun consumeForTest(buffer: KineBuf): ByteArray {
        val pointer = assertNotNull(buffer.ptr, "null sentinel: ${KineNative.INSTANCE.kine_last_error()}")
        val length = buffer.len.toLong().toInt()
        return try {
            if (length == 0) ByteArray(0) else pointer.getByteArray(0, length)
        } finally {
            KineNative.INSTANCE.kine_buf_free(buffer)
        }
    }

    /**
     * A document nobody holds must survive its own render. Swift's ARC keeps
     * `self` alive for the method body; the JVM does not — once the handle has
     * been read the wrapper is collectable, its [java.lang.ref.Cleaner] runs,
     * and the render already in flight comes back "invalid or freed document
     * handle". This is the hazard the `Cleaner` itself introduced.
     *
     * Probabilistic by nature: it needs a hot enough loop for the JIT to see
     * the wrapper die at the handle read. Measured with the fence removed, it
     * fails at iteration 261, 274, 287, 294, 307, 343 and 501 over seven runs,
     * so 2 000 carries a 4× margin; passing costs ~10 s, the price of the only
     * test here that guards a use-after-free. The collector's `sleep` throttles
     * a GC loop — it synchronizes nothing.
     *
     * kill (proven, both directions): drop `Reference.reachabilityFence(this)`
     * from `callingNative` and this goes red 3 of 3; restore it and 3 × 20 000
     * iterations pass clean.
     */
    @Test
    fun aDocumentNobodyHoldsSurvivesItsOwnRender() {
        val stop = AtomicBoolean(false)
        val collector = Thread {
            while (!stop.get()) {
                System.gc()
                Thread.sleep(0, 200_000)
            }
        }
        collector.isDaemon = true
        collector.start()
        try {
            for (i in 0 until 2_000) {
                // Deliberately unheld: the Document is unreachable the moment
                // renderRGBA has read its handle.
                assertEquals(
                    16 * 16 * 4,
                    Kine.Document(cardJSON).renderRGBA(t = 0.0, width = 16, height = 16).data.size,
                    "render $i",
                )
            }
        } finally {
            stop.set(true)
            collector.join(5_000)
        }
    }

    /**
     * The `Cleaner` is this port's stand-in for Swift's `deinit`, and the only
     * mechanism here with no line-for-line iOS counterpart — so it owes a
     * demonstration that it actually frees. Handles are read back through
     * `kine_document_probe`, which answers a freed id with the null sentinel;
     * reflection is the only way to learn the ids, since the field is private
     * exactly as it is in Swift.
     *
     * This is what replaces the iOS `testHandleLifecycleLeakLoop`, ported and
     * then removed: 500 create/render/close cycles never went red for a
     * lifecycle defect — a cleaner that frees nothing, and one that frees the
     * wrong id, both left it green (measured) — because the core's free is
     * idempotent and its ids are never reused.
     *
     * kill (proven): make `Free.run` a no-op and all 2 000 handles are still
     * live after the collection.
     */
    @Test
    fun documentsNobodyClosedAreFreedWhenCollected() {
        val handleField = Kine.Document::class.java.getDeclaredField("handle").apply { isAccessible = true }
        val ids = ArrayList<Long>(2_000)
        var documents: MutableList<Kine.Document>? = ArrayList()
        repeat(2_000) {
            val document = Kine.Document(cardJSON)
            ids += handleField.getLong(document)
            documents!! += document
        }

        fun liveHandles() = ids.count { id ->
            val buffer = KineNative.INSTANCE.kine_document_probe(id)
            val live = buffer.ptr != null
            if (live) KineNative.INSTANCE.kine_buf_free(buffer)
            live
        }

        assertEquals(ids.size, liveHandles(), "handles must be live while their wrappers are held")

        documents = null
        // Bounded poll, not a settling sleep: the cleaner runs on its own thread
        // after a collection, so this ends the moment the count reaches zero.
        var live = ids.size
        for (attempt in 0 until 20) {
            System.gc()
            live = liveHandles()
            if (live == 0) break
            Thread.sleep(100)
        }
        assertEquals(0, live, "the Cleaner did not free the handles of unreachable documents")
    }

    /**
     * The flavor `kotlin/build.sh` builds. `kine_gpu_available` is the one
     * `kine_gpu_*` symbol a CPU build carries, which is why the header tells
     * hosts to branch on it rather than probe for the others.
     *
     * kill: add `--features gpu` to build.sh's host lane — the Metal tree links
     * on the Mac, this answers true, and the Android lane (where wgpu/Metal
     * cannot build at all) is the next thing to break.
     */
    @Test
    fun gpuFlavorIsAbsentFromThisBuild() {
        assertFalse(Kine.gpuAvailable, "the CPU flavor must not carry the Metal raster tree")
    }
}
