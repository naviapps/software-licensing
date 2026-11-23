import Foundation
import SoftwareLicensing
import SoftwareLicensingKeychain
import Synchronization

private final class InMemoryLicenseStorage: LicenseStorage {
  private struct Contents {
    var record: Data?
    var reads = 0
    var writes = 0
  }
  private let contents = Mutex(Contents())

  var counts: (Int, Int) { contents.withLock { ($0.reads, $0.writes) } }

  func load() throws -> Data? {
    contents.withLock {
      $0.reads += 1
      return $0.record
    }
  }

  func save(_ value: Data) throws {
    contents.withLock {
      $0.writes += 1
      $0.record = value
    }
  }
}

@main
private struct CoreLifecycleConsumer {
  @MainActor
  static func main() async throws {
    let clock = Mutex((now: Date(timeIntervalSince1970: 1_000), reads: 0))
    let accessChecks = Mutex(0)
    let readClock: @Sendable () -> Date = {
      clock.withLock {
        $0.reads += 1
        return $0.now
      }
    }
    let configuration = try CallbackLicenseProvider.Configuration(
      source: LicenseSource(identifier: "com.example.standalone")!, planIdentifier: "paid")
    let provider = CallbackLicenseProvider(
      configuration: configuration,
      checkAccess: {
        accessChecks.withLock { $0 += 1 }
        return true
      },
      clock: readClock)
    let storage = InMemoryLicenseStorage()

    do {
      let manager = try LicenseManager(provider: provider, storage: storage, clock: readClock)
      precondition(manager.initialRestoreError == nil && !manager.state.isLicensed)
      let activated = try await manager.activate(.automatic)
      precondition(activated.isLicensed && activated.status == .active)
      precondition(activated.source == configuration.source && activated.plan.identifier == "paid")
      let savedRecord = try storage.load()
      precondition(savedRecord != nil)
    }

    do {
      let reopened = try LicenseManager(provider: provider, storage: storage, clock: readClock)
      let restored = reopened.state
      precondition(reopened.initialRestoreError == nil && restored.isLicensed)
      precondition(
        restored.status == .active && restored.source == configuration.source
          && restored.plan.identifier == "paid" && reopened.plan == restored.plan)
      guard let deadline = restored.nextRefreshAt else {
        preconditionFailure("Expected a refresh deadline")
      }
      precondition(!reopened.needsRefresh())
      let clockReads = clock.withLock { $0.reads }
      let providerReads = accessChecks.withLock { $0 }
      let storageCounts = storage.counts
      precondition(!reopened.needsRefresh(now: deadline.addingTimeInterval(-0.25)))
      precondition(reopened.needsRefresh(now: deadline))
      let unsupportedTime = Date(timeIntervalSince1970: 253_402_300_800)
      precondition(reopened.needsRefresh(now: unsupportedTime))
      precondition(clock.withLock { $0.reads } == clockReads)
      precondition(accessChecks.withLock { $0 } == providerReads && storage.counts == storageCounts)
      clock.withLock { $0.now = deadline }
      let valid = reopened.state
      let existing = reopened.states()
      clock.withLock { $0.now = unsupportedTime }
      let unavailable = reopened.state
      precondition(!unavailable.isLicensed && unavailable.status == .unlicensed)
      precondition(reopened.plan == unavailable.plan && reopened.needsRefresh())
      var existingObserver = existing.makeAsyncIterator()
      let failed = await existingObserver.next()
      precondition(failed == unavailable, "Existing observer kept a stale grant")
      var unavailableObserver = reopened.states().makeAsyncIterator()
      let initial = await unavailableObserver.next()
      precondition(initial == unavailable, "New observer did not start unlicensed")
      let recovery = reopened.states()
      clock.withLock { $0.now = deadline }
      precondition(reopened.state == valid, "Restoring the clock did not recover the saved license")
      var recoveryObserver = recovery.makeAsyncIterator()
      let recovered = await recoveryObserver.next()
      precondition(recovered == valid, "Observer lost the unchanged grant after clock recovery")
      precondition(accessChecks.withLock { $0 } == providerReads && storage.counts == storageCounts)
      let refreshed = try await reopened.refresh()
      precondition(refreshed.outcome == .validated && refreshed.state.isLicensed)
      let deactivated = try await reopened.deactivate()
      precondition(!deactivated.isLicensed && !deactivated.hasActivation)
    }

    let released = try LicenseManager(provider: provider, storage: storage, clock: readClock)
    precondition(
      released.initialRestoreError == nil && !released.state.isLicensed
        && !released.state.hasActivation)

    let denied = CallbackLicenseProvider(
      configuration: configuration, checkAccess: { false }, clock: readClock)
    let deniedManager = try LicenseManager(
      provider: denied, storage: InMemoryLicenseStorage(), clock: readClock)
    do {
      try await deniedManager.activate(.automatic)
      preconditionFailure("Denied callback activation must fail")
    } catch let error as LicenseError {
      precondition(error.code == .invalidLicense && error.mutationOutcome == .notApplied)
    }
    precondition(!deniedManager.state.isLicensed && !deniedManager.state.hasPendingActivation)

    func identitySnapshot(source: String, plan: String) async throws -> LicenseState {
      let identity = try CallbackLicenseProvider.Configuration(
        source: LicenseSource(identifier: source)!, planIdentifier: plan)
      let callback = CallbackLicenseProvider(
        configuration: identity, checkAccess: { true }, clock: readClock)
      let manager = try LicenseManager(
        provider: callback, storage: InMemoryLicenseStorage(), clock: readClock)
      return try await manager.activate(.automatic)
    }

    let composed = "caf\u{00e9}"
    let decomposed = "cafe\u{0301}"
    let original = try await identitySnapshot(source: composed, plan: composed)
    let sourceChange = try await identitySnapshot(source: decomposed, plan: composed)
    let planChange = try await identitySnapshot(source: composed, plan: decomposed)
    precondition(
      original.source != sourceChange.source && original.plan == sourceChange.plan,
      "The snapshot merged distinct provider source spellings")
    precondition(
      original.source == planChange.source && original.plan != planChange.plan,
      "The snapshot merged distinct plan identifier spellings")
    precondition(
      Set([original, sourceChange, planChange]).count == 3,
      "Snapshot equality merged distinct Rust identifiers")

    // Construct the second public product without reading or mutating the Keychain.
    _ = try KeychainLicenseStorage(
      service: "com.example.standalone.consumer", account: "fixture")
    print("SoftwareLicensing external consumer passed")
  }
}
