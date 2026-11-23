import Foundation
import SoftwareLicensing
import SoftwareLicensingKeychain
import Synchronization

private final class MemoryStorage: LicenseStorage {
  private let record = Mutex<Data?>(nil)

  func load() throws -> Data? { record.withLock { $0 } }
  func save(_ value: Data) throws { record.withLock { $0 = value } }
}

@main
private struct CoreLifecycleConsumer {
  @MainActor
  static func main() async throws {
    let configuration = try CallbackLicenseProvider.Configuration(
      source: LicenseSource(identifier: "com.example.standalone")!, planIdentifier: "paid")
    let clock: @Sendable () -> Date = { Date(timeIntervalSince1970: 1_000) }
    let provider = CallbackLicenseProvider(
      configuration: configuration, checkAccess: { true }, clock: clock)
    let storage = MemoryStorage()

    do {
      let manager = try LicenseManager(provider: provider, storage: storage, clock: clock)
      precondition(manager.initialRestoreError == nil && !manager.state.isLicensed)
      let activated = try await manager.activate(.automatic)
      precondition(activated.isLicensed && activated.plan.identifier == "paid")
    }

    let reopened = try LicenseManager(provider: provider, storage: storage, clock: clock)
    precondition(
      reopened.initialRestoreError == nil && reopened.state.isLicensed
        && reopened.state.source == configuration.source)
    let refreshed = try await reopened.refresh()
    precondition(refreshed.outcome == .validated && refreshed.state.isLicensed)
    let released = try await reopened.deactivate()
    precondition(!released.isLicensed && !released.hasActivation)

    // Check both public products without accessing a Keychain item.
    _ = try KeychainLicenseStorage(service: "com.example.standalone.consumer", account: "fixture")
    print("SoftwareLicensing external consumer passed")
  }
}
