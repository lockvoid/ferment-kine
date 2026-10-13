# Kine for Swift

The `Kine` Swift package wraps the shared native renderer. It supports iOS 14
and macOS 11 or later and exposes document handles, interface probing, and
premultiplied RGBA frames.

## Build the native core

From the repository root, on an Apple Silicon Mac with Xcode and rustup:

```sh
./scripts/build-xcframework.sh
```

The script builds the pinned Rust core with Metal support for arm64 iOS devices,
iOS simulators, and macOS. It creates `swift/KineCore.xcframework`, a generated
artifact excluded from Git. Rebuild it after changing the core or native header.

## Add the package

Add the `swift/` directory as a local Swift package in Xcode, then link its
`Kine` library product. In another Swift package, use a local dependency:

```swift
.package(path: "../ferment-kine/swift")
```

The native XCFramework must exist before SwiftPM can resolve this checkout.
The checked-in package does not download a prebuilt binary.

## Render a document

The following paths assume a tool running from the repository root; an app
would load the same resources from its bundle:

```swift
import Foundation
import Kine

try Kine.registerFont(Data(contentsOf: URL(fileURLWithPath: "crate/testdata/font.ttf")))
let json = try String(contentsOfFile: "docs/hero.json", encoding: .utf8)
let document = try Kine.Document(json: json)
let frame = try document.renderRGBA(
    t: 0.5, signals: ["progress": 0.5], width: 800, height: 260
)
let image = frame.cgImage()
```

Keep the document for repeated rendering. `probe()` describes its inputs and
resources; the [schema](../crate/docs/SCHEMA.md) defines the document format.
Missing font families are errors unless you configure a fallback family.

## Test

After building the XCFramework:

```sh
cd swift
swift test
```

Source licensing and font notices are listed in the [root README](../README.md#license).
