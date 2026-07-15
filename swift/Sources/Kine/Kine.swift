import Foundation
import KineCore

#if canImport(CoreGraphics)
    import CoreGraphics
#endif

/// Thin Swift wrapper over the kine C ABI (KineCore / kine.h). Deliberately no
/// schema modeling in Swift — documents are authored as JSON and validated by
/// the core.
public enum Kine {
    public enum KineError: Error, CustomStringConvertible {
        case failed(String)

        public var description: String {
            switch self { case .failed(let message): return message }
        }

        /// The core's thread-local last error, or `fallback` if none is set.
        static func current(_ fallback: String) -> KineError {
            if let pointer = kine_last_error() {
                return .failed(String(cString: pointer))
            }
            return .failed(fallback)
        }
    }

    /// Register a font (TTF/OTF) into the process-global, bundled-only
    /// collection. Idempotent; safe to call concurrently with renders.
    public static func registerFont(_ data: Data) throws {
        let code = data.withUnsafeBytes { raw -> Int32 in
            kine_register_font(raw.bindMemory(to: UInt8.self).baseAddress, raw.count)
        }
        if code != 0 { throw KineError.current("font registration failed") }
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

        public let version: Int
        public let size: Size
        public let inputs: [Input]
        public let roles: [String]
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
    /// the underlying handle is freed on `deinit`.
    public final class Document {
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
