import Foundation
import KineCore

#if canImport(CoreGraphics)
    import CoreGraphics
#endif

/// The installed sink, read by the C trampoline. Written once at startup
/// (`Kine.setLogSink`) before any render — no lock by contract.
private nonisolated(unsafe) var kineLogSink: (@Sendable (Kine.LogLevel, String) -> Void)?

private func kineLogTrampoline(level: Int32, message: UnsafePointer<CChar>?) {
    guard let message, let sink = kineLogSink else { return }
    sink(Kine.LogLevel(rawValue: level) ?? .error, String(cString: message))
}

/// Thin Swift wrapper over the kine C ABI (KineCore / kine.h). Deliberately no
/// schema modeling in Swift — documents are authored as JSON and validated by
/// the core.
public enum Kine {
    /// A failure from the core, carrying the core's own words: which node,
    /// which key, what was expected.
    ///
    /// `LocalizedError` as well as `CustomStringConvertible`, because
    /// `Error.localizedDescription` — what almost every caller reaches for,
    /// and what an agent hands back to the model that wrote the document —
    /// ignores `description` and answers a bare "The operation couldn't be
    /// completed. (KineError error 0.)". The diagnosis this type exists to
    /// deliver was reaching the log sink and nothing else.
    public enum KineError: Error, CustomStringConvertible, LocalizedError {
        case failed(String)

        public var description: String {
            switch self { case .failed(let message): return message }
        }

        public var errorDescription: String? { description }

        /// The core's thread-local last error, or `fallback` if none is set.
        static func current(_ fallback: String) -> KineError {
            if let pointer = kine_last_error() {
                return .failed(String(cString: pointer))
            }
            return .failed(fallback)
        }
    }

    public enum LogLevel: Int32, Sendable {
        case info = 0
        case warn = 1
        case error = 2
    }

    /// Install the process-wide log sink. The core reports every failure
    /// (the `kine_last_error` content) and every font registration through
    /// it — nothing in kine is allowed to fail silently once a sink is set.
    /// Install ONCE at startup, before any render; the callback fires on
    /// whatever thread the failing call runs on.
    public static func setLogSink(_ sink: @escaping @Sendable (LogLevel, String) -> Void) {
        kineLogSink = sink
        kine_set_log_callback(kineLogTrampoline)
    }

    /// Register a font (TTF/OTF) into the process-global, bundled-only
    /// collection. Idempotent; safe to call concurrently with renders.
    public static func registerFont(_ data: Data) throws {
        let code = data.withUnsafeBytes { raw -> Int32 in
            kine_register_font(raw.bindMemory(to: UInt8.self).baseAddress, raw.count)
        }
        if code != 0 { throw KineError.current("font registration failed") }
    }

    /// Declare the render-fallback family: a document naming an unregistered
    /// family renders in it (WARNED once per family through the log sink)
    /// instead of failing the frame. Empty string clears back to strict.
    /// The name must already resolve — the embedded "Inter" always does.
    /// The write seams (probe `missingFonts`) stay strict either way.
    public static func setFallbackFamily(_ name: String) throws {
        let code = name.withCString { kine_set_fallback_family($0) }
        if code != 0 { throw KineError.current("fallback family rejected") }
    }

    /// Crate + schema version, e.g. "kine 0.1.0 (schema v1)".
    public static var version: String { String(cString: kine_version()) }

    /// A document's declared interface (from `probe`).
    public struct ProbeResult: Decodable, Sendable {
        public struct Size: Decodable, Sendable {
            public let width: Double
            public let height: Double
        }

        public struct Input: Decodable, Sendable {
            public let key: String
            public let type: String
        }

        /// One embedded raster asset from the document manifest (§4). `kind` is
        /// currently always "image"; `mime` is the container form
        /// ("image/png"|"image/jpeg"|"image/webp"|"image/gif"|"image/apng");
        /// `animated` is true when the decoded asset has more than one frame.
        public struct Asset: Decodable, Sendable {
            public let key: String
            public let kind: String
            public let mime: String
            public let animated: Bool
        }

        public let version: Int
        public let size: Size
        public let inputs: [Input]
        public let roles: [String]
        public let assets: [Asset]
        /// Font families the document references (literal `style.fontFamily`
        /// values + the defaults of `fontFamily` inputs a style binds to), in
        /// document order, deduped.
        public let fonts: [String]
        /// Of `fonts`, the families the process registry cannot serve at probe
        /// time — fetch and register exactly these before rendering.
        public let missingFonts: [String]

        private enum CodingKeys: String, CodingKey {
            case version, size, inputs, roles, assets, fonts, missingFonts
        }

        public init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            version = try container.decode(Int.self, forKey: .version)
            size = try container.decode(Size.self, forKey: .size)
            inputs = try container.decode([Input].self, forKey: .inputs)
            roles = try container.decode([String].self, forKey: .roles)
            // Optional-safe: docs probed by an older core (pre-manifest) omit it.
            assets = try container.decodeIfPresent([Asset].self, forKey: .assets) ?? []
            fonts = try container.decodeIfPresent([String].self, forKey: .fonts) ?? []
            missingFonts = try container.decodeIfPresent([String].self, forKey: .missingFonts) ?? []
        }
    }

    /// An author's document admitted (SCHEMA §10): the text the host stores,
    /// what the door repaired, and the document's interface.
    public struct Admission: Decodable, Sendable {
        public struct Repair: Decodable, Sendable, Equatable {
            public let path: String
            public let rule: String
            public let message: String
        }

        public let document: String
        public let repairs: [Repair]
        public let interface: ProbeResult
    }

    /// Repair what has one reading, then validate (SCHEMA §10). A refusal
    /// throws the core's own words.
    public static func admit(_ json: String) throws -> Admission {
        let data = try json.withCString { try consume(kine_admit($0), "admission") }
        return try JSONDecoder().decode(Admission.self, from: data)
    }

    /// One rendered frame: RGBA8, premultiplied alpha, sRGB, row-major.
    public struct RenderedFrame: Sendable {
        public let data: Data
        public let width: Int
        public let height: Int
        public var stride: Int { width * 4 }

        #if canImport(CoreGraphics)
            /// A CGImage backed by the premultiplied RGBA bytes.
            public func cgImage() -> CGImage? {
                guard let provider = CGDataProvider(data: data as CFData) else { return nil }
                let info = CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)
                return CGImage(
                    width: width, height: height,
                    bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: stride,
                    space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: info,
                    provider: provider, decode: nil,
                    shouldInterpolate: false, intent: .defaultIntent)
            }
        #endif
    }

    /// A parsed, validated document. Renders every frame without re-parsing;
    /// the underlying handle is freed on `deinit`. `@unchecked Sendable`: the
    /// handle is immutable after `init` and the core is internally thread-safe
    /// (concurrent renders, RwLock-guarded font registry) — so the wrapper
    /// carries no additional shared mutable state.
    public final class Document: @unchecked Sendable {
        private let handle: Int64

        public init(json: String) throws {
            let handle = json.withCString { kine_document_create($0) }
            guard handle != 0 else { throw KineError.current("invalid document") }
            self.handle = handle
        }

        deinit { kine_document_free(handle) }

        public func probe() throws -> ProbeResult {
            let data = try consume(kine_document_probe(handle), "probe")
            return try JSONDecoder().decode(ProbeResult.self, from: data)
        }

        /// Render at time `t` with `signals` (JSON-encodable). `t` is sugar for
        /// the document's `time` input; an explicit `time` signal wins.
        /// Union ink rect across `samples` frames over `[0, span]` seconds at a
        /// small probe raster — design-box fractions. `nil` = fully blank.
        /// The crate's one definition of visual bounds (selection borders,
        /// sticker normalization, preview trimming all read THIS).
        public func inkUnion(
            signals: [String: Any] = [:], samples: Int = 1, span: Double = 0,
            probeWidth: Int = 160, probeHeight: Int = 160
        ) throws -> CGRect? {
            try inkUnionInCanvas(
                signals: signals, samples: samples, span: span,
                probeWidth: probeWidth, probeHeight: probeHeight
            )?.ink
        }

        /// Ink probe payload: `ink` in fractions of the PROBE CANVAS, and
        /// the probe canvas itself in design units — the doc box when
        /// everything fits, the GROWN box (`layoutSize`) when text
        /// overflows. Map on-screen boxes through `canvas`, never the raw
        /// doc size: the pixels render on the same grown canvas.
        public struct InkUnion {
            public let ink: CGRect
            /// Design-unit canvas the fractions speak: `origin.y ≤ 0` when
            /// the box grew upward; equals `(0, 0, docW, docH)` unfitted.
            public let canvas: CGRect
        }

        public func inkUnionInCanvas(
            signals: [String: Any] = [:], samples: Int = 1, span: Double = 0,
            probeWidth: Int = 160, probeHeight: Int = 160
        ) throws -> InkUnion? {
            let json = try signalsJSON(signals)
            let buffer = json.withCString {
                kine_document_ink_union(
                    handle, $0, UInt32(max(1, samples)), span,
                    UInt32(probeWidth), UInt32(probeHeight)
                )
            }
            let data = try consume(buffer, "ink probe")
            guard !data.isEmpty,
                  let object = try JSONSerialization.jsonObject(with: data) as? [String: Double],
                  let x = object["x"], let y = object["y"],
                  let width = object["width"], let height = object["height"],
                  let canvasWidth = object["canvasWidth"],
                  let canvasHeight = object["canvasHeight"],
                  let canvasY = object["canvasY"]
            else { return nil }
            return InkUnion(
                ink: CGRect(x: x, y: y, width: width, height: height),
                canvas: CGRect(x: 0, y: canvasY, width: canvasWidth, height: canvasHeight)
            )
        }

        public func renderRGBA(
            t: Double, signals: [String: Any] = [:], width: Int, height: Int
        ) throws -> RenderedFrame {
            let json = try signalsJSON(signals)
            let buffer = json.withCString {
                kine_document_render_rgba(handle, t, $0, UInt32(width), UInt32(height))
            }
            let data = try consume(buffer, "render")
            return RenderedFrame(data: data, width: width, height: height)
        }

        /// The grown canvas the document's TEXT content needs with these
        /// signals, in design units: `width` = doc width (lines wrap),
        /// `height ≥` doc height, and `y ≤ 0` = the grown canvas's top in
        /// doc coordinates. Doc-sized rect at y 0 = everything fits. The
        /// crate's one definition of text overflow — size render targets
        /// and selection boxes from THIS, then render through
        /// `renderRGBAViewport(y:, height:)`.
        public func layoutSize(signals: [String: Any] = [:]) throws -> CGRect {
            let json = try signalsJSON(signals)
            let buffer = json.withCString { kine_document_layout_size(handle, $0) }
            let data = try consume(buffer, "layout size")
            guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Double],
                  let width = object["width"], let height = object["height"], let y = object["y"]
            else { throw KineError.current("layout size decode failed") }
            return CGRect(x: 0, y: y, width: width, height: height)
        }

        /// `renderRGBA` over a vertical design-space viewport — pass
        /// `layoutSize()`'s `origin.y`/`height` (with a matching-aspect
        /// target) to render text that overflows the doc canvas instead of
        /// cropping it.
        public func renderRGBAViewport(
            t: Double, signals: [String: Any] = [:], width: Int, height: Int,
            viewY: Double, viewHeight: Double
        ) throws -> RenderedFrame {
            let json = try signalsJSON(signals)
            let buffer = json.withCString {
                kine_document_render_rgba_viewport(
                    handle, t, $0, UInt32(width), UInt32(height), viewY, viewHeight
                )
            }
            let data = try consume(buffer, "viewport render")
            return RenderedFrame(data: data, width: width, height: height)
        }
    }
}

/// Copy a `kine_buf` into `Data` and free it. Throws the core's last error on
/// the null sentinel.
private func consume(_ buffer: kine_buf, _ what: String) throws -> Data {
    guard let pointer = buffer.ptr else { throw Kine.KineError.current("\(what) failed") }
    let data = Data(bytes: pointer, count: buffer.len)
    kine_buf_free(buffer)
    return data
}

private func signalsJSON(_ signals: [String: Any]) throws -> String {
    if signals.isEmpty { return "{}" }
    let data = try JSONSerialization.data(withJSONObject: signals)
    return String(decoding: data, as: UTF8.self)
}

#if canImport(Metal)
    import Metal

    extension Kine {
        /// Whether this build carries the GPU raster flavor. A CPU-only build of
        /// the core (the ruby gem, the rails server) has none of the GPU entry
        /// points; the xcframework is always built with them.
        public static var gpuAvailable: Bool { kine_gpu_available() == 1 }

        /// The GPU raster venue, built on the HOST's device and queue.
        ///
        /// Sharing the host's `MTLCommandQueue` is the point: kine's raster and
        /// the host's own command buffers are then ordered by Metal, so a render
        /// can submit and return instead of blocking on the GPU. Create ONE per
        /// process — it owns the image atlas and the renderer — and keep it
        /// alive for as long as you render.
        ///
        /// `@unchecked Sendable`: the handle is immutable after `init` and the
        /// core serializes renders on the engine internally.
        public final class GPUEngine: @unchecked Sendable {
            private let handle: Int64

            /// Throws if the GPU flavor is absent, if the device is not the
            /// system default (kine refuses to cross devices rather than
            /// silently rendering on another GPU), or if wgpu cannot be built on
            /// the handles given.
            public init(device: MTLDevice, commandQueue: MTLCommandQueue) throws {
                guard Kine.gpuAvailable else {
                    throw KineError.failed("this kine build has no GPU flavor")
                }
                let handle = kine_gpu_engine_create(
                    Unmanaged.passUnretained(device as AnyObject).toOpaque(),
                    Unmanaged.passUnretained(commandQueue as AnyObject).toOpaque()
                )
                guard handle != 0 else { throw KineError.current("gpu engine creation failed") }
                self.handle = handle
            }

            deinit { kine_gpu_engine_destroy(handle) }

            fileprivate var id: Int64 { handle }
        }
    }

    extension Kine.Document {
        /// Rasterize into a texture YOU own and created. Must be
        /// `.rgba8Unorm`, exactly `width`×`height`, and declare
        /// `.renderTarget` usage — all three are checked by the core.
        ///
        /// SUBMITS AND RETURNS: the GPU work is queued, not waited on. Because
        /// the engine renders on the queue you handed it, your next command
        /// buffer on that queue sees the finished pixels without a fence.
        ///
        /// Throws on any failure — an exhausted image atlas included — and
        /// leaves the texture untouched, so the caller can fall back to
        /// `renderRGBA` for that frame.
        public func render(
            t: Double, signals: [String: Any] = [:], width: Int, height: Int,
            using engine: Kine.GPUEngine, into texture: MTLTexture
        ) throws {
            let json = try signalsJSON(signals)
            let code = json.withCString {
                kine_gpu_render_document(
                    engine.id, handle, t, $0, UInt32(width), UInt32(height),
                    Unmanaged.passUnretained(texture as AnyObject).toOpaque()
                )
            }
            if code != 0 { throw Kine.KineError.current("gpu render failed") }
        }

        /// `render(into:)` over a vertical design-space viewport — pair with
        /// `layoutSize()` exactly as with `renderRGBAViewport`.
        public func render(
            t: Double, signals: [String: Any] = [:], width: Int, height: Int,
            viewY: Double, viewHeight: Double,
            using engine: Kine.GPUEngine, into texture: MTLTexture
        ) throws {
            let json = try signalsJSON(signals)
            let code = json.withCString {
                kine_gpu_render_document_viewport(
                    engine.id, handle, t, $0, UInt32(width), UInt32(height),
                    viewY, viewHeight,
                    Unmanaged.passUnretained(texture as AnyObject).toOpaque()
                )
            }
            if code != 0 { throw Kine.KineError.current("gpu viewport render failed") }
        }
    }
#endif
