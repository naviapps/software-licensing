// swift-tools-version: 6.3
import PackageDescription

let package = Package(
  name: "SoftwareLicensing",
  platforms: [.macOS(.v26)],
  products: [
    .library(name: "SoftwareLicensing", targets: ["SoftwareLicensing"]),
    .library(name: "SoftwareLicensingKeychain", targets: ["SoftwareLicensingKeychain"]),
  ],
  targets: [
    .binaryTarget(name: "CSoftwareLicensing", path: "Artifacts/CSoftwareLicensing.xcframework"),
    .target(name: "SoftwareLicensing", dependencies: ["CSoftwareLicensing"]),
    .target(name: "SoftwareLicensingKeychain", dependencies: ["SoftwareLicensing"]),
    .testTarget(name: "SoftwareLicensingTests", dependencies: ["SoftwareLicensing"]),
  ]
)
