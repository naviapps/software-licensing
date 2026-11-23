// swift-tools-version: 6.4
import PackageDescription

let package = Package(
  name: "SoftwareLicensingConsumer",
  platforms: [.macOS(.v26)],
  dependencies: [.package(name: "software-licensing", path: "../sdk")],
  targets: [
    .executableTarget(
      name: "CoreLifecycleConsumer",
      dependencies: [
        .product(name: "SoftwareLicensing", package: "software-licensing"),
        .product(name: "SoftwareLicensingKeychain", package: "software-licensing"),
      ])
  ]
)
