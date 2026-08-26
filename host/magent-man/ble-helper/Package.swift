// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "ble-helper",
    platforms: [
        .macOS(.v11)
    ],
    products: [
        .executable(
            name: "ble-helper",
            targets: ["BLEHelper"]
        )
    ],
    dependencies: [],
    targets: [
        .executableTarget(
            name: "BLEHelper",
            dependencies: [],
            path: "Sources"
        )
    ]
)
