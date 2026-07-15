// swift-tools-version:5.9
import PackageDescription

// KineCore.xcframework is built by scripts/build-xcframework.sh (a gitignored
// build artifact). Run that before `swift build` / `swift test`.
let package = Package(
    name: "Kine",
    platforms: [.macOS(.v11), .iOS(.v14)],
    products: [
        .library(name: "Kine", targets: ["Kine"])
    ],
    targets: [
        .binaryTarget(name: "KineCore", path: "KineCore.xcframework"),
        .target(name: "Kine", dependencies: ["KineCore"]),
        .testTarget(
            name: "KineTests",
            dependencies: ["Kine"],
            resources: [.copy("Resources")]
        ),
    ]
)
