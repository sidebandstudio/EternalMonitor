// swift-tools-version:5.9
// The iPad's transport v3 core: the reconnect handshake, sealed records, the
// replay window, the reconnect state machine and the key rule. Phase 3a of
// the transport security design. Not yet a dependency of the app target.
import PackageDescription

let package = Package(
    name: "EternalTransportV3",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "EternalTransportV3", targets: ["EternalTransportV3"]),
    ],
    targets: [
        .target(name: "EternalTransportV3"),
        .testTarget(name: "EternalTransportV3Tests", dependencies: ["EternalTransportV3"]),
    ]
)
