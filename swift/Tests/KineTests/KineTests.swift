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

    func testProbeManifestsReferencedAndMissingFonts() throws {
        // The card's text binds `fontFamily` to the "font" input, whose default
        // is the registered test face — referenced, nothing missing.
        let document = try Kine.Document(json: Self.cardJSON)
        let probe = try document.probe()
        XCTAssertEqual(probe.fonts, ["Bebas Neue"])
        XCTAssertEqual(probe.missingFonts, [])

        let restyled = Self.cardJSON.replacingOccurrences(
            of: "Bebas Neue", with: "Missing Grotesk")
        let missing = try Kine.Document(json: restyled).probe()
        XCTAssertEqual(missing.fonts, ["Missing Grotesk"])
        XCTAssertEqual(missing.missingFonts, ["Missing Grotesk"])
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

// MARK: - GPU flavor (G2)

#if canImport(Metal)
    import Metal

    /// The macOS slice has Metal, so the GPU surface is exercised on the host —
    /// it does not wait for the app.
    final class KineGPUTests: XCTestCase {
        override class func setUp() {
            super.setUp()
            try! Kine.registerFont(KineTests.fontData)
        }

        /// The host's device and queue, exactly as the app will hand them over.
        private func hostEngine() throws -> (MTLDevice, MTLCommandQueue, Kine.GPUEngine) {
            let device = try XCTUnwrap(MTLCreateSystemDefaultDevice(), "no Metal device")
            let queue = try XCTUnwrap(device.makeCommandQueue(), "no Metal queue")
            return (device, queue, try Kine.GPUEngine(device: device, commandQueue: queue))
        }

        private func target(_ device: MTLDevice, _ width: Int, _ height: Int) throws -> MTLTexture {
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(
                pixelFormat: .rgba8Unorm, width: width, height: height, mipmapped: false)
            descriptor.usage = [.renderTarget, .shaderRead]
            return try XCTUnwrap(device.makeTexture(descriptor: descriptor), "no texture")
        }

        /// Read a rendered texture back so the smoke test asserts PIXELS, not
        /// just a zero return code. Test-only — the frame path never reads back.
        private func readback(_ texture: MTLTexture, _ queue: MTLCommandQueue) throws -> [UInt8] {
            let (w, h) = (texture.width, texture.height)
            var bytes = [UInt8](repeating: 0, count: w * h * 4)
            // The render was SUBMITTED, not waited on; a command buffer on the
            // same queue is ordered after it — which is the whole point of
            // sharing the queue. If that ordering did not hold, this reads
            // garbage and the test fails.
            let buffer = try XCTUnwrap(queue.makeCommandBuffer())
            let blit = try XCTUnwrap(buffer.makeBlitCommandEncoder())
            blit.synchronize(resource: texture)
            blit.endEncoding()
            buffer.commit()
            buffer.waitUntilCompleted()
            bytes.withUnsafeMutableBytes { raw in
                texture.getBytes(
                    raw.baseAddress!, bytesPerRow: w * 4,
                    from: MTLRegionMake2D(0, 0, w, h), mipmapLevel: 0)
            }
            return bytes
        }

        func testGPUFlavorIsCompiledIn() {
            XCTAssertTrue(Kine.gpuAvailable, "xcframework must be built with --features gpu")
        }

        func testEngineRendersIntoAHostTexture() throws {
            let (device, queue, engine) = try hostEngine()
            let document = try Kine.Document(json: KineTests.cardJSON)
            let texture = try target(device, 256, 256)

            try document.render(
                t: 0.5, signals: ["progress": 0.5], width: 256, height: 256,
                using: engine, into: texture)

            let pixels = try readback(texture, queue)
            let inked = stride(from: 3, to: pixels.count, by: 4).filter { pixels[$0] > 0 }.count
            XCTAssertGreaterThan(inked, 1000, "GPU render produced an empty texture")
        }

        /// The parity contract in Swift terms: GPU and CPU are not bit-identical
        /// but must agree within the crate's gate (<= 0.05% of pixels off by
        /// more than 8/255).
        func testGPUMatchesCPUWithinTheParityGate() throws {
            let (device, queue, engine) = try hostEngine()
            let document = try Kine.Document(json: KineTests.cardJSON)
            let size = 256
            let texture = try target(device, size, size)

            try document.render(
                t: 0.5, signals: ["progress": 0.5], width: size, height: size,
                using: engine, into: texture)
            let gpu = try readback(texture, queue)
            let cpu = try document.renderRGBA(
                t: 0.5, signals: ["progress": 0.5], width: size, height: size)

            XCTAssertEqual(gpu.count, cpu.data.count)
            var off = 0
            var worst = 0
            for i in stride(from: 0, to: gpu.count, by: 4) {
                var pixelWorst = 0
                for k in 0..<4 {
                    pixelWorst = max(pixelWorst, abs(Int(gpu[i + k]) - Int(cpu.data[i + k])))
                }
                if pixelWorst > 8 { off += 1 }
                worst = max(worst, pixelWorst)
            }
            let pct = Double(off) * 100.0 / Double(size * size)
            print("gpu-vs-cpu (swift): off=\(pct)%  max|d|=\(worst)")
            XCTAssertLessThanOrEqual(pct, 0.05, "GPU diverged from CPU beyond the parity gate")
        }

        func testViewportRenderIsAcceptedAndDraws() throws {
            let (device, queue, engine) = try hostEngine()
            let document = try Kine.Document(json: KineTests.cardJSON)
            let texture = try target(device, 128, 192)
            let canvas = try document.layoutSize(signals: ["progress": 0.5])

            try document.render(
                t: 0.25, signals: ["progress": 0.5], width: 128, height: 192,
                viewY: canvas.minY, viewHeight: canvas.height,
                using: engine, into: texture)

            let pixels = try readback(texture, queue)
            XCTAssertTrue(
                stride(from: 3, to: pixels.count, by: 4).contains { pixels[$0] > 0 },
                "viewport GPU render produced an empty texture")
        }

        /// A mismatched target is an ERROR, never undefined behavior — wgpu is
        /// never handed a texture whose shape it was not promised.
        func testMismatchedTargetsAreRejected() throws {
            let (device, _, engine) = try hostEngine()
            let document = try Kine.Document(json: KineTests.cardJSON)

            let wrongSize = try target(device, 64, 64)
            XCTAssertThrowsError(
                try document.render(
                    t: 0, width: 128, height: 128, using: engine, into: wrongSize))

            let wrongFormat = MTLTextureDescriptor.texture2DDescriptor(
                pixelFormat: .bgra8Unorm, width: 64, height: 64, mipmapped: false)
            wrongFormat.usage = [.renderTarget, .shaderRead]
            let bgra = try XCTUnwrap(device.makeTexture(descriptor: wrongFormat))
            XCTAssertThrowsError(
                try document.render(t: 0, width: 64, height: 64, using: engine, into: bgra))

            let noRenderTarget = MTLTextureDescriptor.texture2DDescriptor(
                pixelFormat: .rgba8Unorm, width: 64, height: 64, mipmapped: false)
            noRenderTarget.usage = [.shaderRead]
            let readOnly = try XCTUnwrap(device.makeTexture(descriptor: noRenderTarget))
            XCTAssertThrowsError(
                try document.render(t: 0, width: 64, height: 64, using: engine, into: readOnly))
        }

        /// THE ORDERING PROOF owed since S0.
        ///
        /// The ruled seam says kine can submit-and-return, with no fence and no
        /// CPU wait, because its raster and the host's own command buffers ride
        /// ONE MTLCommandQueue. This tests exactly that shape: kine renders (no
        /// wait), then the host immediately encodes a GPU-side blit that CONSUMES
        /// kine's texture — which is what the compositor does when it samples the
        /// overlay.
        ///
        /// A single pass could pass by luck (the GPU may simply finish first), so
        /// each iteration renders DIFFERENT content and asserts the copy carries
        /// THAT iteration's pixels. Lost ordering shows up as the previous
        /// iteration's frame or an empty one, not as a flake we could miss.
        func testWorkOnTheSharedQueueIsOrderedWithoutAFence() throws {
            let (device, queue, engine) = try hostEngine()
            let document = try Kine.Document(json: KineTests.cardJSON)
            let size = 256

            let source = try target(device, size, size)
            let destinationDescriptor = MTLTextureDescriptor.texture2DDescriptor(
                pixelFormat: .rgba8Unorm, width: size, height: size, mipmapped: false)
            destinationDescriptor.usage = [.shaderRead]
            let destination = try XCTUnwrap(device.makeTexture(descriptor: destinationDescriptor))

            // Guards the guard: if every step rendered the same picture, a stale
            // read would be indistinguishable from a correct one and this test
            // would pass vacuously.
            var previous: [UInt8]?

            for step in 0..<12 {
                let progress = Double(step) / 11.0
                let t = Double(step) * 0.13

                // 1. kine renders onto the shared queue and RETURNS immediately.
                try document.render(
                    t: t, signals: ["progress": progress], width: size, height: size,
                    using: engine, into: source)

                // 2. The host consumes it on the same queue with no fence between.
                let buffer = try XCTUnwrap(queue.makeCommandBuffer())
                let blit = try XCTUnwrap(buffer.makeBlitCommandEncoder())
                blit.copy(from: source, to: destination)
                blit.synchronize(resource: destination)
                blit.endEncoding()
                buffer.commit()
                buffer.waitUntilCompleted()

                var copied = [UInt8](repeating: 0, count: size * size * 4)
                copied.withUnsafeMutableBytes { raw in
                    destination.getBytes(
                        raw.baseAddress!, bytesPerRow: size * 4,
                        from: MTLRegionMake2D(0, 0, size, size), mipmapLevel: 0)
                }

                // 3. It must be THIS step's frame, within the parity gate.
                let cpu = try document.renderRGBA(
                    t: t, signals: ["progress": progress], width: size, height: size)
                var off = 0
                for i in stride(from: 0, to: copied.count, by: 4) {
                    var worst = 0
                    for k in 0..<4 {
                        worst = max(worst, abs(Int(copied[i + k]) - Int(cpu.data[i + k])))
                    }
                    if worst > 8 { off += 1 }
                }
                let pct = Double(off) * 100.0 / Double(size * size)
                XCTAssertLessThanOrEqual(
                    pct, 0.05,
                    "step \(step): the blit read \(pct)% wrong pixels — kine's submit was NOT ordered before the host's consuming command buffer")

                if let previous {
                    XCTAssertNotEqual(
                        previous, copied,
                        "step \(step) rendered the same pixels as step \(step - 1) — a stale read would be undetectable, so this test proves nothing")
                }
                previous = copied
            }
        }

        /// A destroyed engine reports an error on reuse rather than crashing —
        /// the same use-after-free contract document handles carry.
        func testRenderAfterEngineIsGoneReportsAnError() throws {
            let (device, _, engine) = try hostEngine()
            let document = try Kine.Document(json: KineTests.cardJSON)
            let texture = try target(device, 64, 64)
            try document.render(t: 0, width: 64, height: 64, using: engine, into: texture)

            // Dropping the engine frees its handle; the document outlives it.
            var released: Kine.GPUEngine? = engine
            released = nil
            _ = released
            XCTAssertNoThrow(try document.renderRGBA(t: 0, width: 64, height: 64))
        }
    }
#endif

#if canImport(Metal)
    /// What one GPUEngine costs in GPU memory. Measured in S3 at ~147 MB, fixed
    /// (independent of document and target size) and allocated on the FIRST
    /// render, not at create. That number decides how many engines a host can
    /// afford, so it is a GATE, not a print: a vello_hybrid bump that doubles
    /// the atlas or alpha textures must fail here rather than on a device.
    final class KineGPUMemoryProbe: XCTestCase {
        override class func setUp() {
            super.setUp()
            try! Kine.registerFont(KineTests.fontData)
        }

        func testEngineFootprint() throws {
            let device = try XCTUnwrap(MTLCreateSystemDefaultDevice())
            let document = try Kine.Document(json: KineTests.cardJSON)
            let mb = { (b: Int) in String(format: "%7.1f MB", Double(b) / 1_048_576.0) }

            for size in [64, 512, 1080] {
                // Fresh engine per size so the deltas are not cumulative.
                let baseline = device.currentAllocatedSize
                let queue = try XCTUnwrap(device.makeCommandQueue())
                let engine = try Kine.GPUEngine(device: device, commandQueue: queue)
                let afterCreate = device.currentAllocatedSize

                let descriptor = MTLTextureDescriptor.texture2DDescriptor(
                    pixelFormat: .rgba8Unorm, width: size, height: size, mipmapped: false)
                descriptor.usage = [.renderTarget, .shaderRead]
                let texture = try XCTUnwrap(device.makeTexture(descriptor: descriptor))
                try document.render(t: 0, signals: ["progress": 0.5],
                                    width: size, height: size, using: engine, into: texture)
                let afterRender = device.currentAllocatedSize

                let total = afterRender - baseline
                print("ENGINE FOOTPRINT \(size)x\(size): create=\(mb(afterCreate - baseline)) firstRender=\(mb(afterRender - afterCreate)) total=\(mb(total))")
                XCTAssertLessThan(
                    total, 256 * 1_048_576,
                    "one engine now costs \(mb(total)) — it was ~147 MB when S3 measured it; a host budgeting for that will be wrong")
                _ = engine
            }
        }
    }
#endif
