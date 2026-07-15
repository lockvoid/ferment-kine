import Foundation
import XCTest

#if canImport(CoreGraphics)
    import CoreGraphics
    import ImageIO
    import UniformTypeIdentifiers
#endif

@testable import Kine

final class KineTests: XCTestCase {
    static let fontData = try! Data(contentsOf: resource("font", "ttf"))
    static let cardJSON = try! String(contentsOf: resource("test_card", "json"), encoding: .utf8)

    static func resource(_ name: String, _ ext: String) -> URL {
        // `.copy("Resources")` preserves the folder, so look inside it.
        guard
            let url = Bundle.module.url(
                forResource: name, withExtension: ext, subdirectory: "Resources")
        else {
            fatalError("missing test resource \(name).\(ext)")
        }
        return url
    }

    override class func setUp() {
        super.setUp()
        try! Kine.registerFont(fontData)
    }

    func testVersion() {
        XCTAssertTrue(Kine.version.hasPrefix("kine "), Kine.version)
        XCTAssertTrue(Kine.version.contains("schema v1"))
    }

    func testProbeReportsInterface() throws {
        let document = try Kine.Document(json: Self.cardJSON)
        let probe = try document.probe()
        XCTAssertEqual(probe.version, 1)
        XCTAssertEqual(probe.size.width, 512)
        XCTAssertEqual(probe.roles, ["card", "text"])
        XCTAssertEqual(
            probe.inputs.map(\.key),
            ["time", "progress", "activations", "font", "foreground", "background", "accent", "borderColor"])
    }

    func testRenderRGBADiffersBySignals() throws {
        let document = try Kine.Document(json: Self.cardJSON)
        let a = try document.renderRGBA(t: 0, signals: ["progress": 0.0], width: 128, height: 128)
        let b = try document.renderRGBA(t: 0.5, signals: ["progress": 0.8], width: 128, height: 128)
        XCTAssertEqual(a.data.count, 128 * 128 * 4)
        XCTAssertEqual(a.stride, 128 * 4)
        XCTAssertNotEqual(a.data, b.data, "different signals produced identical pixels")
        #if canImport(CoreGraphics)
            let image = a.cgImage()
            XCTAssertNotNil(image)
            XCTAssertEqual(image?.width, 128)
        #endif
    }

    func testMalformedDocumentThrows() {
        XCTAssertThrowsError(try Kine.Document(json: "{ not json")) { error in
            XCTAssertFalse("\(error)".isEmpty, "error should carry the core's message")
        }
    }

    func testUnregisteredFontIsRenderError() throws {
        let json = """
            { "version": 1, "size": { "width": 64, "height": 64 },
              "root": { "kind": "text", "key": "t", "content": "X",
                "frame": { "x": 0, "y": 0, "width": 64, "height": 64 },
                "style": { "fontFamily": "Nope Sans", "size": 20, "fill": "#FFFFFF" } } }
            """
        let document = try Kine.Document(json: json)
        XCTAssertThrowsError(try document.renderRGBA(t: 0, width: 64, height: 64)) { error in
            XCTAssertTrue("\(error)".contains("not registered"), "\(error)")
        }
    }

    func testHandleLifecycleLeakLoop() throws {
        // 500 create/render/free cycles: the handle is freed on deinit at the end
        // of each iteration. A leak or use-after-free would crash the process.
        for i in 0..<500 {
            let document = try Kine.Document(json: Self.cardJSON)
            let frame = try document.renderRGBA(t: Double(i) * 0.001, width: 64, height: 64)
            XCTAssertEqual(frame.data.count, 64 * 64 * 4)
        }
    }

    #if canImport(CoreGraphics)
        func testMatchesCrateGolden() throws {
            // The macOS-arm64 xcframework slice and the crate share NEON codegen,
            // so the Swift render should match the committed golden. Compared with
            // a small tolerance to absorb the PNG (straight) → premultiplied
            // round-trip through CoreGraphics.
            let document = try Kine.Document(json: Self.cardJSON)
            let frame = try document.renderRGBA(
                t: 0.5, signals: ["progress": 0.5], width: 512, height: 512)
            let golden = try goldenPremultipliedRGBA("card_t05_p05", width: 512, height: 512)
            XCTAssertEqual(frame.data.count, golden.count)

            var total = 0.0
            for (rendered, expected) in zip(frame.data, golden) {
                total += abs(Double(rendered) - Double(expected))
            }
            let meanAbsDiff = total / Double(golden.count)
            XCTAssertLessThan(meanAbsDiff, 2.0, "Swift render diverged from crate golden")
        }

        /// Decode a golden PNG into premultiplied RGBA8 via CoreGraphics.
        private func goldenPremultipliedRGBA(_ name: String, width: Int, height: Int) throws -> [UInt8]
        {
            let url = Self.resource(name, "png")
            guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
                let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
            else { throw Kine.KineError.failed("cannot decode golden \(name)") }

            var pixels = [UInt8](repeating: 0, count: width * height * 4)
            let info = CGImageAlphaInfo.premultipliedLast.rawValue
            guard
                let context = CGContext(
                    data: &pixels, width: width, height: height,
                    bitsPerComponent: 8, bytesPerRow: width * 4,
                    space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: info)
            else { throw Kine.KineError.failed("cannot make context") }
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return pixels
        }
    #endif
}
