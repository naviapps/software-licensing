import Foundation
import Synchronization
import Testing

@testable import SoftwareLicensing

private final class MemoryStorage: @unchecked Sendable, LicenseStorage {
  var data: Data?
  var fails = false
  var saveCount = 0
  var loadCount = 0
  var failOnSave: Int?
  var beforeSave: (() -> Void)?
  var onSaveCompletion: ((Int) -> Void)?
  var beforeLoad: (() -> Void)?
  func load() throws -> Data? {
    loadCount += 1
    let record = data
    let callback = beforeLoad
    beforeLoad = nil
    callback?()
    return record
  }
  func save(_ record: Data) throws {
    saveCount += 1
    defer { onSaveCompletion?(saveCount) }
    if fails || failOnSave == saveCount { throw LicenseError(.storageFailure) }
    let callback = beforeSave
    beforeSave = nil
    callback?()
    data = record
  }
}

private final class ValidationClock: Sendable {
  private let state = Mutex((now: Date(timeIntervalSince1970: 2000), reads: 0))
  var reads: Int { state.withLock { $0.reads } }
  func read() -> Date {
    state.withLock {
      $0.reads += 1
      return $0.now
    }
  }
  func set(_ seconds: TimeInterval) {
    state.withLock { $0.now = Date(timeIntervalSince1970: seconds) }
  }
}

@MainActor private final class ValidationProvider: LicenseProvider {
  var result: LicenseValidationResult?
  var expiresAt: Date?
  var validations = 0
  func activate(_: LicenseActivationRequest) async throws -> LicenseActivation {
    LicenseActivation(
      source: LicenseSource(identifier: "test")!, planIdentifier: "paid",
      activatedAt: Date(timeIntervalSince1970: 1000), licenseKey: "TEST-KEY",
      activationIdentifier: "test-instance", expiresAt: expiresAt)!
  }
  func validate(_: LicenseActivation) async throws -> LicenseValidationResult {
    validations += 1
    guard let result else { throw LicenseError(.transportFailure) }
    return result
  }
  func deactivate(_: LicenseActivation) async throws {}
}

@MainActor private final class NativeProviderSession: LicenseSessionDriver {
  private let core = LicenseSession()
  private let onProvider: () -> Void
  private let activation: LicenseActivation
  private(set) var requests: [UInt64] = []
  private(set) var completions: [UInt64] = []
  private(set) var completionWasCancelled = false

  init(activation: LicenseActivation, onProvider: @escaping () -> Void) {
    self.activation = activation
    self.onProvider = onProvider
  }

  func dispatch(_ command: Data) throws -> Data {
    let command = try #require(JSONSerialization.jsonObject(with: command) as? [String: Any])
    // Host-owned drivers may contain an older Rust session.
    if let kind = command["command"] as? String,
      kind == "completeEffect" || kind == "resumeCompletion"
    {
      throw LicenseError(.invalidData)
    }
    let reply = try core.dispatch(command)
    var object = try #require(JSONSerialization.jsonObject(with: reply) as? [String: Any])
    guard let effect = object["effect"] as? [String: Any], effect["kind"] as? String == "provider"
    else { return reply }
    object["effect"] = ["kind": "provider", "id": try #require(effect["id"])]
    return try JSONSerialization.data(withJSONObject: object)
  }

  func stageStoredRecord(_ record: Data) throws { try core.stageStoredRecord(record) }

  func executeProviderEffect(_ id: UInt64) async throws {
    requests.append(id)
    onProvider()
  }

  func completeProviderEffect(_ id: UInt64, now: TimeInterval, cancelled: Bool) throws -> Data {
    completions.append(id)
    completionWasCancelled = cancelled
    return try core.dispatch([
      "command": "provider", "id": id, "now": now,
      "result": ["kind": "activated", "activation": LicensingNative.object(activation)],
    ])
  }
}

@Test @MainActor private func nativeProviderCompletionSurvivesCancellationAndInvalidClock()
  async throws
{
  let activation = try await ValidationProvider().activate(.licenseKey("TEST-KEY"))
  let clock = ValidationClock()
  let storage = MemoryStorage()
  var initialDriver: NativeProviderSession?
  let manager = try LicenseManager(
    sessionFactory: {
      let driver = NativeProviderSession(activation: activation) {
        #expect(storage.saveCount == 1)
        withUnsafeCurrentTask { $0?.cancel() }
        clock.set(.nan)
      }
      initialDriver = driver
      return driver
    }, storage: storage, clock: clock.read)
  let driver = try #require(initialDriver)
  let attempt = Task { @MainActor in try await manager.activate(.licenseKey("TEST-KEY")) }
  await #expect(throws: LicenseError(.invalidData)) { try await attempt.value }
  #expect(driver.requests.count == 1 && driver.completions.isEmpty)

  clock.set(2001)
  try await manager.retryPersistence()
  #expect(driver.completions == driver.requests && driver.completionWasCancelled)
  #expect(manager.state.isLicensed && storage.saveCount == 2)
}

@Test(arguments: [Double.nan, .infinity, -.infinity, 253_402_300_800]) @MainActor
func invalidClockReadsFailClosedWithoutChangingTheSavedActivation(_ invalidTime: Double)
  async throws
{
  let clock = ValidationClock()
  let storage = MemoryStorage()
  let provider = ValidationProvider()
  let manager = try LicenseManager(provider: provider, storage: storage, clock: { clock.read() })
  try await manager.activate(.licenseKey("TEST-KEY"))
  let saved = storage.data
  let writes = storage.saveCount
  let loads = storage.loadCount
  let deadline = try #require(manager.state.nextRefreshAt)
  let reads = clock.reads
  #expect(!manager.needsRefresh(now: deadline.addingTimeInterval(-0.25)))
  #expect(manager.needsRefresh(now: deadline))
  #expect(manager.needsRefresh(now: Date(timeIntervalSince1970: invalidTime)))
  #expect(clock.reads == reads)

  clock.set(deadline.timeIntervalSince1970)
  let available = manager.state
  #expect(available.isLicensed)
  var observer = manager.states().makeAsyncIterator()
  clock.set(invalidTime)
  let unavailable = manager.state
  #expect(unavailable.status == .unlicensed && manager.plan == unavailable.plan)
  #expect(manager.needsRefresh())
  #expect(await observer.next() == unavailable)
  var recovery = manager.states().makeAsyncIterator()
  #expect(await recovery.next() == unavailable)
  var bufferedRecovery = manager.states().makeAsyncIterator()
  await #expect(throws: LicenseError(.invalidData)) { try await manager.refresh() }
  clock.set(deadline.timeIntervalSince1970)
  #expect(manager.state == available)
  #expect(await bufferedRecovery.next() == available)
  #expect(storage.data == saved && storage.saveCount == writes && provider.validations == 0)
  #expect(storage.loadCount == loads && !manager.state.hasPendingPersistence)
}

@Test @MainActor func snapshotsPreserveTheExactSpellingOfSourceAndPlan() throws {
  let time = Date(timeIntervalSince1970: 2000)
  let manager = try LicenseManager(
    provider: ValidationProvider(), storage: MemoryStorage(), clock: { time })
  func snapshot(_ source: String, _ plan: String) throws -> LicenseState {
    let identity = try #require(LicenseSource(identifier: source))
    return try manager.adoptExternallyVerifiedActivation(
      #require(
        LicenseActivation(source: identity, planIdentifier: plan, activatedAt: time)))
  }
  let composed = "caf\u{00e9}"
  let decomposed = "cafe\u{0301}"
  let original = try snapshot(composed, composed)
  let sourceChange = try snapshot(decomposed, composed)
  let planChange = try snapshot(composed, decomposed)
  #expect(original.source != sourceChange.source && original.plan == sourceChange.plan)
  #expect(original.source == planChange.source && original.plan != planChange.plan)
  #expect(Set([original, sourceChange, planChange]).count == 3)
}

@Test(arguments: [false, true]) @MainActor
func failedValidationWritePreservesSavedBytesAndRetriesWithoutCallingProvider(
  _ authoritativelyRejected: Bool
) async throws {
  let provider = ValidationProvider()
  if authoritativelyRejected { provider.result = LicenseValidationResult(isValid: false)! }
  let storage = MemoryStorage()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  try await manager.activate(.licenseKey("TEST-KEY"))
  let saved = try #require(storage.data)
  let expectedStatus: LicenseStatus = authoritativelyRejected ? .invalid : .gracePeriod
  storage.fails = true
  await #expect(throws: LicenseError(.storageFailure)) { try await manager.refresh() }
  #expect(storage.data == saved)
  #expect(manager.state.isLicensed == !authoritativelyRejected)
  #expect(manager.state.status == expectedStatus)
  #expect(manager.state.hasActivation && manager.state.hasPendingPersistence)
  storage.fails = false
  try await manager.retryPersistence()
  #expect(manager.state.status == expectedStatus && !manager.state.hasPendingPersistence)
  #expect(provider.validations == 1 && storage.data != saved)
}

@Test @MainActor func failedStorageCannotGrantAnUncommittedRenewal() async throws {
  let provider = ValidationProvider()
  provider.expiresAt = Date(timeIntervalSince1970: 3000)
  provider.result = LicenseValidationResult(
    isValid: true, expiresAt: Date(timeIntervalSince1970: 5000))!
  let storage = MemoryStorage()
  let clock = ValidationClock()
  let manager = try LicenseManager(provider: provider, storage: storage, clock: clock.read)
  try await manager.activate(.licenseKey("TEST-KEY"))
  clock.set(2100)
  storage.fails = true
  await #expect(throws: LicenseError(.storageFailure)) { try await manager.refresh() }
  #expect(manager.state.plan.expiresAt == Date(timeIntervalSince1970: 3000))
  clock.set(3500)
  #expect(!manager.state.isLicensed)
  storage.fails = false
  try await manager.retryPersistence()
  #expect(manager.state.isLicensed && provider.validations == 1)
  #expect(manager.state.plan.expiresAt == Date(timeIntervalSince1970: 5000))
}

@MainActor private final class ReleaseProvider: LicenseProvider {
  var releaseCount = 0
  var activationCount = 0
  var validationCount = 0
  var onActivation: (() -> Void)?
  var onRelease: (() -> Void)?
  var onValidation: (() -> Void)?
  var beforeCompletion: (@MainActor () async -> Void)?
  var activationFailure: (any Error)?
  var activationResult: LicenseActivation?
  var releaseFailure: LicenseError?
  var validationFailure: LicenseError?
  var validationResult: LicenseValidationResult?
  func activate(_ request: LicenseActivationRequest) async throws -> LicenseActivation {
    activationCount += 1
    onActivation?()
    await beforeCompletion?()
    if let activationFailure { throw activationFailure }
    if let activationResult { return activationResult }
    return try await Provider(failsDeactivation: false).activate(request)
  }
  func validate(_: LicenseActivation) async throws -> LicenseValidationResult {
    validationCount += 1
    onValidation?()
    await beforeCompletion?()
    if let validationResult { return validationResult }
    throw validationFailure ?? LicenseError(.transportFailure)
  }
  func deactivate(_: LicenseActivation) async throws {
    releaseCount += 1
    onRelease?()
    await beforeCompletion?()
    if let releaseFailure { throw releaseFailure }
  }
}

private enum ClockCompletionPhase: CaseIterable, Sendable {
  case activationIntent, activationResult, validationResult, releaseIntent, releaseResult

  var startsLicensed: Bool {
    self != .activationIntent && self != .activationResult
  }
  var releases: Bool { self == .releaseIntent || self == .releaseResult }
  var writeOffset: Int { self == .activationResult || self == .releaseResult ? 2 : 1 }

  @MainActor func run(_ manager: LicenseManager) async throws {
    switch self {
    case .activationIntent, .activationResult:
      _ = try await manager.activate(.licenseKey("TEST-KEY"))
    case .validationResult:
      _ = try await manager.refresh()
    case .releaseIntent, .releaseResult:
      _ = try await manager.deactivate()
    }
  }
}

private final class ReloadClock: Sendable {
  private let invalidRead = Mutex<Int?>(nil)
  private let beforeRead = Mutex<(@MainActor @Sendable () -> Void)?>(nil)
  func fail(onRead number: Int) { invalidRead.withLock { $0 = number } }
  func clear() { invalidRead.withLock { $0 = nil } }
  func onNextRead(_ callback: @escaping @MainActor @Sendable () -> Void) {
    beforeRead.withLock { $0 = callback }
  }
  func read() -> Date {
    let callback = beforeRead.withLock { value in
      let callback = value
      value = nil
      return callback
    }
    if let callback { MainActor.assumeIsolated { callback() } }
    return invalidRead.withLock { remaining in
      guard let number = remaining else { return Date(timeIntervalSince1970: 2000) }
      remaining = number == 1 ? nil : number - 1
      return Date(timeIntervalSince1970: number == 1 ? .nan : 2000)
    }
  }
}

@Test(
  arguments: [ClockCompletionPhase.activationResult, .validationResult, .releaseResult],
  [false, true])
@MainActor private func reloadCannotReplaceAnOutstandingProviderOperation(
  _ phase: ClockCompletionPhase, _ invalidBusyFlagSample: Bool
) async throws {
  let clock = ReloadClock()
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  provider.validationResult = LicenseValidationResult(isValid: true)!
  let manager = try LicenseManager(provider: provider, storage: storage, clock: clock.read)
  if phase.startsLicensed { try await manager.activate(.licenseKey("TEST-KEY")) }
  let (entered, notify) = AsyncStream<Void>.makeStream()
  var completion: CheckedContinuation<Void, Never>?
  provider.beforeCompletion = {
    await withCheckedContinuation {
      completion = $0
      notify.yield(())
    }
  }
  let task = Task { @MainActor in try await phase.run(manager) }
  var iterator = entered.makeAsyncIterator()
  _ = await iterator.next()
  let bytes = storage.data
  let loads = storage.loadCount
  let saves = storage.saveCount
  if invalidBusyFlagSample {
    // Exercise the old separate isActivating/isRefreshing/isDeactivating reads.
    clock.fail(onRead: phase == .activationResult ? 1 : phase == .validationResult ? 2 : 3)
  }
  let expected: LicenseError.Code =
    invalidBusyFlagSample && phase == .activationResult ? .invalidData : .operationInProgress
  #expect(throws: LicenseError(expected)) { try manager.reload() }
  clock.clear()
  #expect(storage.data == bytes && storage.loadCount == loads && storage.saveCount == saves)
  completion!.resume()
  notify.finish()
  try await task.value
  #expect(manager.state.isLicensed == !phase.releases)
  #expect(!manager.state.hasPendingActivation && !manager.state.hasPendingDeactivation)
  #expect(!manager.state.hasPendingPersistence)
  #expect(provider.activationCount == 1)
  #expect(provider.validationCount == (phase == .validationResult ? 1 : 0))
  #expect(provider.releaseCount == (phase.releases ? 1 : 0))
}

@Test @MainActor private func reloadCannotDiscardARetryableWriteAfterAnInvalidClockRead()
  async throws
{
  let clock = ReloadClock()
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let manager = try LicenseManager(provider: provider, storage: storage, clock: clock.read)
  storage.failOnSave = 1
  await #expect(throws: LicenseError(.storageFailure)) {
    try await manager.activate(.licenseKey("TEST-KEY"))
  }
  let bytes = storage.data
  let loads = storage.loadCount
  let saves = storage.saveCount
  clock.fail(onRead: 4)
  #expect(throws: LicenseError(.operationInProgress)) { try manager.reload() }
  clock.clear()
  #expect(storage.data == bytes && storage.loadCount == loads && storage.saveCount == saves)
  #expect(manager.state.hasPendingPersistence)
  storage.failOnSave = nil
  try await manager.retryPersistence()
  #expect(manager.state.isLicensed && !manager.state.hasPendingPersistence)
  #expect(provider.activationCount == 1 && storage.saveCount == 3)
}

@Test @MainActor private func reloadRejectsInvalidSnapshotTimeBeforeLoadingStorage() throws {
  let clock = ReloadClock()
  let storage = MemoryStorage()
  let manager = try LicenseManager(provider: ReleaseProvider(), storage: storage, clock: clock.read)
  let loads = storage.loadCount
  clock.fail(onRead: 1)
  #expect(throws: LicenseError(.invalidData)) { try manager.reload() }
  #expect(storage.loadCount == loads && storage.saveCount == 0)
  try manager.reload()
  #expect(storage.loadCount == loads + 1 && storage.saveCount == 0)
  #expect(manager.initialRestoreError == nil && !manager.state.hasActivation)
}

@Test(arguments: [false, true], [false, true]) @MainActor
private func reloadOwnsClockAndStorageCallbacks(_ fromClock: Bool, _ nestedReload: Bool) throws {
  let clock = ReloadClock()
  let storage = MemoryStorage()
  let manager = try LicenseManager(provider: ReleaseProvider(), storage: storage, clock: clock.read)
  let activation = LicenseActivation(
    source: LicenseSource(identifier: "test")!, planIdentifier: "paid",
    activatedAt: Date(timeIntervalSince1970: 1000), licenseKey: "TEST-KEY",
    activationIdentifier: "verified-instance")!
  let loads = storage.loadCount
  let callback: @MainActor @Sendable () -> Void = {
    #expect(throws: LicenseError(.operationInProgress)) {
      if nestedReload {
        try manager.reload()
      } else {
        _ = try manager.adoptExternallyVerifiedActivation(activation)
      }
    }
  }
  if fromClock { clock.onNextRead(callback) } else { storage.beforeLoad = { callback() } }
  try manager.reload()
  #expect(storage.loadCount == loads + 1 && storage.saveCount == 0 && storage.data == nil)
  #expect(!manager.state.hasActivation && !manager.state.hasPendingPersistence)
  // Ownership ends even on failure, so ordinary adoption and reload still work.
  try manager.adoptExternallyVerifiedActivation(activation)
  try manager.reload()
  #expect(manager.state.isLicensed && storage.saveCount == 1)
}

@Test(arguments: [false, true]) @MainActor
private func reloadCallbacksCanObserveTheCurrentCommittedState(_ fromClock: Bool) async throws {
  let clock = ReloadClock()
  let storage = MemoryStorage()
  let manager = try LicenseManager(provider: ReleaseProvider(), storage: storage, clock: clock.read)
  try manager.adoptExternallyVerifiedActivation(
    LicenseActivation(
      source: LicenseSource(identifier: "test")!, planIdentifier: "paid",
      activatedAt: Date(timeIntervalSince1970: 1000))!)
  var stream: AsyncStream<LicenseState>?
  let callback: @MainActor @Sendable () -> Void = {
    #expect(manager.state.isLicensed && !manager.needsRefresh())
    stream = manager.states()
  }
  if fromClock { clock.onNextRead(callback) } else { storage.beforeLoad = { callback() } }
  try manager.reload()
  var iterator = try #require(stream).makeAsyncIterator()
  #expect(await iterator.next()?.isLicensed == true)
  #expect(manager.state.isLicensed)
}

@Test(arguments: [false, true], [false, true]) @MainActor
private func reloadCannotRestoreAccessAfterObservedExpiryAndClockRollback(
  _ duringLoad: Bool, _ subsecond: Bool
) async throws {
  let clock = ValidationClock()
  let storage = MemoryStorage()
  let provider = ValidationProvider()
  let observed = 3500.0
  let rollback = subsecond ? observed - 0.5 : 2500.0
  provider.expiresAt = Date(timeIntervalSince1970: subsecond ? observed - 0.25 : 3000)
  let manager = try LicenseManager(provider: provider, storage: storage, clock: clock.read)
  try await manager.activate(.licenseKey("TEST-KEY"))
  clock.set(observed)
  #expect(manager.state.status == .expired)
  let loads = storage.loadCount
  if duringLoad { storage.beforeLoad = { clock.set(rollback) } } else { clock.set(rollback) }
  let bytes = storage.data
  #expect(throws: LicenseError(.clockRollback)) { try manager.reload() }
  #expect(!manager.state.isLicensed && storage.data == bytes)
  #expect(storage.loadCount == loads + (duringLoad ? 1 : 0))
  clock.set(observed)
  try manager.reload()
  #expect(manager.state.status == .expired)
}

@Test(arguments: ClockCompletionPhase.allCases, [false, true]) @MainActor
private func completedSavesRecoverAfterTheClockBecomesValid(
  _ phase: ClockCompletionPhase, _ failedSave: Bool
) async throws {
  let clock = ValidationClock()
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  provider.validationResult = LicenseValidationResult(isValid: true)!
  let manager = try LicenseManager(provider: provider, storage: storage, clock: clock.read)
  if phase.startsLicensed { try await manager.activate(.licenseKey("TEST-KEY")) }
  let initialWrites = storage.saveCount
  let targetWrite = initialWrites + phase.writeOffset
  storage.onSaveCompletion = { count in if count == targetWrite { clock.set(.nan) } }
  if failedSave { storage.failOnSave = targetWrite }
  await #expect(throws: LicenseError(.invalidData)) { try await phase.run(manager) }
  #expect(!manager.state.isLicensed)
  clock.set(2001)
  let pending = manager.state
  let bytes = storage.data
  let writes = storage.saveCount
  let calls = [provider.activationCount, provider.validationCount, provider.releaseCount]
  #expect(throws: LicenseError(.operationInProgress)) { try manager.reload() }
  let cancelled = Task { @MainActor in
    withUnsafeCurrentTask { $0?.cancel() }
    try await manager.retryPersistence()
  }
  await #expect(throws: CancellationError.self) { try await cancelled.value }
  #expect(storage.data == bytes && storage.saveCount == writes && manager.state == pending)
  #expect([provider.activationCount, provider.validationCount, provider.releaseCount] == calls)
  storage.failOnSave = nil
  try await manager.retryPersistence()
  #expect(!manager.state.hasPendingPersistence && !manager.state.hasPendingDeactivation)
  #expect(manager.state.isLicensed == !phase.releases)
  #expect(provider.activationCount == 1)
  #expect(provider.validationCount == (phase == .validationResult ? 1 : 0))
  #expect(provider.releaseCount == (phase.releases ? 1 : 0))
  let finalWrites = initialWrites + (phase == .validationResult ? 1 : 2) + (failedSave ? 1 : 0)
  #expect(storage.saveCount == finalWrites)
  try await manager.retryPersistence()
  #expect(storage.saveCount == finalWrites && provider.activationCount == 1)
  let restored = try LicenseManager(provider: provider, storage: storage, clock: clock.read)
  #expect(restored.state.isLicensed == manager.state.isLicensed)
  #expect(!restored.state.hasPendingActivation && !restored.state.hasPendingDeactivation)
}

@Test @MainActor private func cancellationWhileAcknowledgingAFailedSaveLeavesItRetryable()
  async throws
{
  let clock = ReloadClock()
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let manager = try LicenseManager(provider: provider, storage: storage, clock: clock.read)
  storage.failOnSave = 1
  storage.onSaveCompletion = { count in if count == 1 { clock.fail(onRead: 1) } }
  await #expect(throws: LicenseError(.invalidData)) {
    try await manager.activate(.licenseKey("TEST-KEY"))
  }
  #expect(manager.state.hasPendingPersistence)
  #expect(storage.saveCount == 1 && storage.data == nil && provider.activationCount == 0)

  storage.failOnSave = nil
  clock.onNextRead { withUnsafeCurrentTask { $0?.cancel() } }
  let retry = Task { @MainActor in try await manager.retryPersistence() }
  await #expect(throws: CancellationError.self) { try await retry.value }
  #expect(manager.state.hasPendingPersistence)
  #expect(storage.saveCount == 1 && storage.data == nil && provider.activationCount == 0)

  try await manager.retryPersistence()
  #expect(manager.state.isLicensed && !manager.state.hasPendingPersistence)
  #expect(storage.saveCount == 3 && provider.activationCount == 1)
}

@Test(arguments: [ClockCompletionPhase.activationResult, .validationResult, .releaseResult])
@MainActor private func completedProviderResultsRecoverWithoutRepeatingTheProvider(
  _ phase: ClockCompletionPhase
) async throws {
  let clock = ValidationClock()
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  provider.validationResult = LicenseValidationResult(isValid: true)!
  let manager = try LicenseManager(provider: provider, storage: storage, clock: clock.read)
  if phase.startsLicensed { try await manager.activate(.licenseKey("TEST-KEY")) }
  switch phase {
  case .activationResult: provider.onActivation = { clock.set(.nan) }
  case .validationResult: provider.onValidation = { clock.set(.nan) }
  case .releaseResult: provider.onRelease = { clock.set(.nan) }
  default: break
  }
  await #expect(throws: LicenseError(.invalidData)) { try await phase.run(manager) }
  #expect(!manager.state.isLicensed)
  clock.set(2001)
  try await manager.retryPersistence()
  #expect(manager.state.isLicensed == !phase.releases && !manager.state.hasPendingPersistence)
  #expect(
    !manager.state.isActivating && !manager.state.isRefreshing && !manager.state.isDeactivating)
  #expect(provider.activationCount == 1)
  #expect(provider.validationCount == (phase == .validationResult ? 1 : 0))
  #expect(provider.releaseCount == (phase.releases ? 1 : 0))
  #expect(
    storage.saveCount == (phase == .activationResult ? 2 : phase == .validationResult ? 3 : 4))
}

@Test @MainActor func aNewOperationResumesACompletedProviderResultBeforeStarting() async throws {
  let clock = ValidationClock()
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  provider.onActivation = { clock.set(.nan) }
  provider.validationResult = LicenseValidationResult(isValid: true)!
  let manager = try LicenseManager(provider: provider, storage: storage, clock: clock.read)
  await #expect(throws: LicenseError(.invalidData)) {
    try await manager.activate(.licenseKey("TEST-KEY"))
  }
  clock.set(2001)
  #expect(try await manager.refresh().outcome == .validated)
  #expect(provider.activationCount == 1 && provider.validationCount == 1)
  #expect(storage.saveCount == 3 && manager.state.isLicensed)
}

@Test(arguments: [false, true]) @MainActor
func malformedProviderResultDoesNotLeaveAnInFlightLease(_ oversized: Bool) async throws {
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  if oversized {
    let data = try LicensingNative.encode([
      "source": "test", "planIdentifier": String(repeating: "a", count: 1_048_576),
      "activatedAt": 1_000,
    ])
    provider.activationResult = try LicensingNative.decoder().decode(
      LicenseActivation.self, from: data)
  } else {
    provider.activationFailure = LicenseError(.transportFailure, statusCode: -1)
  }
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  var states = manager.states().makeAsyncIterator()
  _ = await states.next()
  await #expect(throws: LicenseError(.invalidData)) {
    try await manager.activate(.licenseKey("TEST-KEY"))
  }
  // The released lease is published before the caller reads state again.
  let released = try #require(await states.next())
  #expect(!released.isActivating && released.hasPendingActivation)
  #expect(!manager.state.isActivating && manager.state.hasPendingActivation)
  // Fixture authority confirms the malformed response did not create an instance.
  try manager.confirmActivationNotAppliedAfterExternalVerification()
  provider.activationFailure = nil
  provider.activationResult = nil
  try await manager.activate(.licenseKey("TEST-KEY"))
  #expect(manager.state.isLicensed && provider.activationCount == 2)
}

private enum RecoveryEntry: CaseIterable, Sendable {
  case retry, activate, refresh, deactivate

  @MainActor func run(_ manager: LicenseManager) async throws {
    switch self {
    case .retry: try await manager.retryPersistence()
    case .activate: _ = try await manager.activate(.licenseKey("TEST-KEY"))
    case .refresh: _ = try await manager.refresh()
    case .deactivate: _ = try await manager.deactivate()
    }
  }
}

@Test(arguments: RecoveryEntry.allCases) @MainActor
private func cancelledLicensingEntryLeavesPendingRecoveryUntouched(_ entry: RecoveryEntry)
  async throws
{
  for pendingRelease in [false, true] {
    let provider = ReleaseProvider()
    let storage = MemoryStorage()
    let manager = try LicenseManager(
      provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
    if pendingRelease {
      try await manager.activate(.licenseKey("TEST-KEY"))
      storage.fails = true
      await #expect(throws: LicenseError(.storageFailure)) { try await manager.deactivate() }
    } else {
      storage.fails = true
      await #expect(throws: LicenseError(.storageFailure)) {
        try await manager.activate(.licenseKey("TEST-KEY"))
      }
    }
    let state = manager.state
    let bytes = storage.data
    let writes = storage.saveCount
    storage.fails = false
    let task = Task { @MainActor in
      withUnsafeCurrentTask { $0?.cancel() }
      try await entry.run(manager)
    }
    await #expect(throws: CancellationError.self) { try await task.value }
    #expect(storage.saveCount == writes)
    #expect(storage.data == bytes)
    #expect(manager.state == state)
    #expect(provider.activationCount == (pendingRelease ? 1 : 0) && provider.validationCount == 0)
    #expect(provider.releaseCount == 0)

    try await manager.retryPersistence()
    #expect(!manager.state.hasPendingPersistence)
    #expect(provider.activationCount == 1)
    #expect(provider.releaseCount == (pendingRelease ? 1 : 0))
  }
}

@Test(arguments: RecoveryEntry.allCases) @MainActor
private func cancelledLicensingEntryDoesNotReopenAnUnreadableStore(_ entry: RecoveryEntry)
  async throws
{
  let storage = MemoryStorage()
  let bytes = Data("unreadable confidential record".utf8)
  storage.data = bytes
  let provider = ReleaseProvider()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  #expect(manager.initialRestoreError != nil)
  let reads = storage.loadCount
  let task = Task { @MainActor in
    withUnsafeCurrentTask { $0?.cancel() }
    try await entry.run(manager)
  }
  await #expect(throws: CancellationError.self) { try await task.value }
  #expect(storage.loadCount == reads && storage.saveCount == 0)
  #expect(storage.data == bytes && manager.initialRestoreError != nil)
  #expect(
    provider.activationCount == 0 && provider.validationCount == 0 && provider.releaseCount == 0)
  storage.data = nil
  try await manager.retryPersistence()
  #expect(storage.loadCount == reads + 1)
  #expect(manager.initialRestoreError == nil && !manager.state.hasActivation)
}

@Test @MainActor private func cancellationBetweenPersistenceAndReleaseKeepsTheReleasePending()
  async throws
{
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  try await manager.activate(.licenseKey("TEST-KEY"))
  storage.beforeSave = { withUnsafeCurrentTask { $0?.cancel() } }
  let task = Task { @MainActor in try await manager.deactivate() }
  await #expect(throws: CancellationError.self) { try await task.value }
  #expect(provider.releaseCount == 0)
  #expect(manager.state.hasPendingDeactivation && !manager.state.hasPendingPersistence)
  #expect(!manager.state.isDeactivating && !manager.state.isLicensed)
  let record = try #require(JSONSerialization.jsonObject(with: storage.data!) as? [String: Any])
  #expect(record["pendingDeactivation"] as? Bool == true)

  try await manager.deactivate()
  #expect(provider.releaseCount == 1)
  #expect(!manager.state.hasActivation && !manager.state.hasPendingDeactivation)
}

@Test @MainActor private func dueOnlyRefreshResumesInterruptedReleaseAfterRestart() async throws {
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let clock = ValidationClock()
  let first = try LicenseManager(provider: provider, storage: storage, clock: clock.read)
  try await first.activate(.licenseKey("TEST-KEY"))
  #expect(!first.needsRefresh())
  let interruptedStorage = MemoryStorage()
  let firstReleaseWrite = storage.saveCount + 1
  storage.onSaveCompletion = { count in
    guard count == firstReleaseWrite else { return }
    // A process interruption leaves the first journal, without a cancellation result.
    interruptedStorage.data = storage.data
    withUnsafeCurrentTask { $0?.cancel() }
  }
  let release = Task { @MainActor in try await first.deactivate() }
  await #expect(throws: CancellationError.self) { try await release.value }
  storage.onSaveCompletion = nil
  #expect(provider.releaseCount == 0)

  let interruptedBytes = try #require(interruptedStorage.data)
  let interrupted = try #require(
    JSONSerialization.jsonObject(with: interruptedBytes) as? [String: Any])
  #expect(interrupted["pendingDeactivation"] as? Bool == true)
  #expect(interrupted["lastOperationFailure"] is NSNull)
  #expect(interrupted["nextRefreshAt"] is NSNull)

  let cancelledBytes = try #require(storage.data)
  let cancelled = try #require(
    JSONSerialization.jsonObject(with: cancelledBytes) as? [String: Any])
  let failure = cancelled["lastOperationFailure"] as? [String: Any]
  #expect(failure?["code"] as? String == "cancelled")
  #expect(failure?["mutationOutcome"] as? String == "notApplied")
  #expect(
    cancelled["nextRefreshAt"] as? Double == 2000
      + LicenseRefreshPolicy.default.failureRetryInterval)

  clock.set(2001)
  let restarted = try LicenseManager(
    provider: provider, storage: interruptedStorage, clock: clock.read)
  #expect(restarted.state.hasPendingDeactivation && !restarted.state.isLicensed)
  #expect(restarted.needsRefresh(), "The previous validation schedule must not delay the release.")
  let result = try await restarted.refresh(onlyIfDue: true)
  #expect(result.outcome == .deactivated)
  #expect(provider.releaseCount == 1 && provider.validationCount == 0)
  #expect(!restarted.state.hasActivation && !restarted.state.hasPendingDeactivation)
}

@Test @MainActor private func cancelledValidationDoesNotCommitLateProviderFailure() async throws {
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  try await manager.activate(.licenseKey("TEST-KEY"))
  let bytes = storage.data
  let writes = storage.saveCount
  let nextRefresh = manager.state.nextRefreshAt
  provider.onValidation = { withUnsafeCurrentTask { $0?.cancel() } }
  provider.validationFailure = LicenseError(.rateLimited, retryAfterSeconds: 120)
  let refresh = Task { @MainActor in try await manager.refresh() }
  await #expect(throws: CancellationError.self) { try await refresh.value }
  #expect(storage.data == bytes && storage.saveCount == writes)
  #expect(manager.state.nextRefreshAt == nextRefresh && !manager.state.isRefreshing)
}

@Test(arguments: [false, true]) @MainActor
private func completedProviderEffectsRemainDurableAfterCancellation(_ failFinalSave: Bool)
  async throws
{
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  provider.onActivation = { withUnsafeCurrentTask { $0?.cancel() } }
  if failFinalSave { storage.failOnSave = 2 }
  let activation = Task { @MainActor in try await manager.activate(.licenseKey("TEST-KEY")) }
  if failFinalSave {
    await #expect(throws: LicenseError(.storageFailure)) { try await activation.value }
    #expect(manager.state.hasPendingPersistence && provider.activationCount == 1)
    storage.failOnSave = nil
    try await manager.retryPersistence()
  } else {
    #expect(try await activation.value.isLicensed)
  }
  let restored = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  #expect(restored.state.isLicensed && restored.state.hasActivation)

  provider.onRelease = { withUnsafeCurrentTask { $0?.cancel() } }
  if failFinalSave { storage.failOnSave = storage.saveCount + 2 }
  let release = Task { @MainActor in try await manager.deactivate() }
  if failFinalSave {
    await #expect(throws: LicenseError(.storageFailure)) { try await release.value }
    #expect(manager.state.hasPendingPersistence && provider.releaseCount == 1)
    storage.failOnSave = nil
  } else {
    #expect(try await !release.value.hasActivation)
  }
  try await manager.retryPersistence()
  let released = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  #expect(!released.state.hasActivation && !released.state.hasPendingDeactivation)
  #expect(provider.activationCount == 1 && provider.releaseCount == 1)
}

@Test @MainActor func failedFinalReleaseSaveRetriesOnlyHostStorage() async throws {
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  try await manager.activate(.licenseKey("TEST-KEY"))
  storage.failOnSave = storage.saveCount + 2
  await #expect(throws: LicenseError(.storageFailure)) { try await manager.deactivate() }
  #expect(provider.releaseCount == 1)
  #expect(manager.state.hasPendingPersistence && manager.state.hasPendingDeactivation)

  try await manager.retryPersistence()
  #expect(provider.releaseCount == 1)
  #expect(storage.saveCount == 5)
  #expect(!manager.state.hasActivation && !manager.state.hasPendingPersistence)
}

private struct Provider: LicenseProvider {
  let failsDeactivation: Bool
  func activate(_: LicenseActivationRequest) async throws -> LicenseActivation {
    LicenseActivation(
      source: LicenseSource(identifier: "test")!, planIdentifier: "paid",
      activatedAt: Date(timeIntervalSince1970: 1000), licenseKey: "TEST-KEY",
      activationIdentifier: "test-instance")!
  }
  func validate(_: LicenseActivation) async throws -> LicenseValidationResult {
    throw LicenseError(.transportFailure)
  }
  func deactivate(_: LicenseActivation) async throws {
    if failsDeactivation { throw LicenseError(.transportFailure) }
  }
}

private actor AccessCounter {
  var calls = 0
  private var nextFailure: (any Error)?

  init(nextFailure: (any Error)? = nil) { self.nextFailure = nextFailure }

  func check(cancelFirst: Bool = false) throws -> Bool {
    calls += 1
    if cancelFirst && calls == 1 { withUnsafeCurrentTask { $0?.cancel() } }
    if let failure = nextFailure {
      nextFailure = nil
      throw failure
    }
    return true
  }
}

@Test(arguments: [false, true]) @MainActor
func callbackTransientFailureCanRetryWithoutPendingActivation(_ recreateManager: Bool)
  async throws
{
  let access = AccessCounter(nextFailure: URLError(.notConnectedToInternet))
  let provider = CallbackLicenseProvider(
    configuration: try .init(
      source: LicenseSource(identifier: "host-membership")!, planIdentifier: "paid"),
    checkAccess: { try await access.check() }, clock: { Date(timeIntervalSince1970: 2000) })
  let storage = MemoryStorage()
  var manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  let failure = LicenseError(.transportFailure, mutationOutcome: .notApplied)
  await #expect(throws: failure) { try await manager.activate(.automatic) }
  #expect(await access.calls == 1)
  #expect(!manager.state.hasPendingActivation && !manager.state.hasPendingPersistence)
  #expect(!manager.state.hasActivation && !manager.state.isLicensed)
  #expect(manager.state.lastOperationFailure == failure && storage.saveCount == 2)

  if recreateManager {
    manager = try LicenseManager(
      provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  }
  #expect(manager.initialRestoreError == nil && storage.saveCount == 2)
  #expect(!manager.state.hasPendingActivation && !manager.state.hasPendingPersistence)
  #expect(!manager.state.hasActivation && !manager.state.isLicensed)
  #expect(manager.state.lastOperationFailure == failure)
  #expect(try await manager.activate(.automatic).isLicensed)
  #expect(await access.calls == 2)
  #expect(!manager.state.hasPendingActivation && !manager.state.hasPendingPersistence)
  #expect(manager.state.lastOperationFailure == nil)
}

@Test(arguments: [false, true]) @MainActor
func callbackCancellationCannotGrantAccessOrLeavePendingActivation(_ returnsLateAccess: Bool)
  async throws
{
  let access = AccessCounter(nextFailure: returnsLateAccess ? nil : CancellationError())
  let provider = CallbackLicenseProvider(
    configuration: try .init(
      source: LicenseSource(identifier: "host-membership")!, planIdentifier: "paid"),
    checkAccess: { try await access.check(cancelFirst: returnsLateAccess) },
    clock: { Date(timeIntervalSince1970: 2000) })
  let storage = MemoryStorage()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  let task = Task { @MainActor in try await manager.activate(.automatic) }
  await #expect(throws: CancellationError.self) { try await task.value }
  let failure = LicenseError(.cancelled, mutationOutcome: .notApplied)
  #expect(await access.calls == 1)
  #expect(!manager.state.hasActivation && !manager.state.isLicensed)
  #expect(!manager.state.hasPendingActivation && !manager.state.hasPendingPersistence)
  #expect(manager.state.lastOperationFailure == failure && storage.saveCount == 2)

  let restored = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  #expect(restored.initialRestoreError == nil && storage.saveCount == 2)
  #expect(restored.state.lastOperationFailure == failure)
  #expect(!restored.state.hasActivation && !restored.state.isLicensed)
  #expect(!restored.state.hasPendingActivation && !restored.state.hasPendingPersistence)
  #expect(try await restored.activate(.automatic).isLicensed)
  #expect(await access.calls == 2)
  #expect(!restored.state.hasPendingActivation && !restored.state.hasPendingPersistence)
  #expect(restored.state.lastOperationFailure == nil)
}

@Test @MainActor func callbackTypedFailureMetadataSurvivesManagerRestore() async throws {
  for original in [
    LicenseError(
      .serverFailure, statusCode: 503, retryAfterSeconds: 120, mutationOutcome: .unknown),
    LicenseError(.rateLimited, statusCode: 429, retryAfterSeconds: 90),
    LicenseError(.cancelled, statusCode: 499, retryAfterSeconds: 30, mutationOutcome: .unknown),
  ] {
    let access = AccessCounter(nextFailure: original)
    let provider = CallbackLicenseProvider(
      configuration: try .init(
        source: LicenseSource(identifier: "host-membership")!, planIdentifier: "paid"),
      checkAccess: { try await access.check() }, clock: { Date(timeIntervalSince1970: 2000) })
    let storage = MemoryStorage()
    let manager = try LicenseManager(
      provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
    let expected = LicenseError(
      original.code, statusCode: original.statusCode, retryAfterSeconds: original.retryAfterSeconds,
      mutationOutcome: .notApplied)
    if original.code == .cancelled {
      await #expect(throws: CancellationError.self) { try await manager.activate(.automatic) }
    } else {
      await #expect(throws: expected) { try await manager.activate(.automatic) }
    }
    #expect(manager.state.lastOperationFailure == expected)
    #expect(!manager.state.hasPendingActivation && !manager.state.hasPendingPersistence)
    #expect(!manager.state.hasActivation && !manager.state.isLicensed)
    let restored = try LicenseManager(
      provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
    #expect(restored.initialRestoreError == nil)
    #expect(restored.state.lastOperationFailure == expected)
    #expect(!restored.state.hasPendingActivation && !restored.state.hasPendingPersistence)
    #expect(!restored.state.hasActivation && !restored.state.isLicensed)
    #expect(await access.calls == 1)
    #expect(try await restored.activate(.automatic).isLicensed)
    #expect(await access.calls == 2)
  }
}

@Test @MainActor func callbackPersistenceRecoveryDoesNotRepeatAccessChecks() async throws {
  let access = AccessCounter()
  let provider = CallbackLicenseProvider(
    configuration: try .init(
      source: LicenseSource(identifier: "host-membership")!, planIdentifier: "paid"),
    checkAccess: { try await access.check() }, clock: { Date(timeIntervalSince1970: 2000) })
  let storage = MemoryStorage()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  storage.failOnSave = 2
  await #expect(throws: LicenseError(.storageFailure)) { try await manager.activate(.automatic) }
  #expect(await access.calls == 1)
  storage.failOnSave = nil
  try await manager.retryPersistence()
  #expect(manager.state.isLicensed && manager.state.source?.identifier == "host-membership")
  storage.fails = true
  await #expect(throws: LicenseError(.storageFailure)) { try await manager.deactivate() }
  storage.fails = false
  try await manager.retryPersistence()
  #expect(!manager.state.hasActivation && !manager.state.hasPendingPersistence)
  #expect(await access.calls == 1)
}

@Test @MainActor func persistenceRetryAndPendingDeactivationSurviveRestore() async throws {
  let storage = MemoryStorage()
  let manager = try LicenseManager(
    provider: Provider(failsDeactivation: true), storage: storage,
    clock: { Date(timeIntervalSince1970: 2000) })
  storage.fails = true
  await #expect(throws: LicenseError(.storageFailure)) {
    try await manager.activate(.licenseKey("TEST-KEY"))
  }
  #expect(!manager.state.isLicensed)
  #expect(manager.state.hasPendingPersistence)
  storage.fails = false
  try await manager.retryPersistence()
  #expect(manager.state.isLicensed)
  let refresh = try await manager.refresh()
  #expect(refresh.outcome == .gracePeriod && refresh.state.status == .gracePeriod)
  await #expect(
    throws: LicenseError(
      .mutationOutcomeUnknown, mutationOutcome: .unknown)
  ) { try await manager.deactivate() }
  #expect(manager.state.lastOperationFailure == LicenseError(.transportFailure))
  #expect(!manager.state.isLicensed)
  let restored = try LicenseManager(
    provider: Provider(failsDeactivation: false), storage: storage,
    clock: { Date(timeIntervalSince1970: 2100) })
  #expect(restored.state.hasPendingDeactivation)
  let released = try await restored.refresh()
  #expect(released.outcome == .deactivated)
  #expect(!restored.state.hasActivation)
}

@Test @MainActor func unsupportedRecordsRemainUntouchedAndBlockAutomaticRestore() throws {
  let storage = MemoryStorage()
  let original = Data("{\"version\":0}".utf8)
  storage.data = original
  let manager = try LicenseManager(provider: Provider(failsDeactivation: false), storage: storage)
  #expect(manager.initialRestoreError?.code == .unsupportedVersion)
  #expect(!manager.state.isLicensed)
  #expect(storage.data == original)
  #expect(storage.saveCount == 0)
}

@Test @MainActor func malformedStorageCannotReplaceTheCommittedSession() throws {
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let now = Date(timeIntervalSince1970: 2000)
  let manager = try LicenseManager(provider: provider, storage: storage, clock: { now })
  let activation = try #require(
    LicenseActivation(
      source: .unspecified, planIdentifier: "fixture", activatedAt: now))
  let committed = try manager.adoptExternallyVerifiedActivation(activation)
  for raw in ["", " \nnull\t", "{"] {
    let bytes = Data(raw.utf8)
    storage.data = bytes
    let opened = try LicenseManager(
      provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: .nan) })
    #expect(opened.initialRestoreError == LicenseError(.storageFailure))
    #expect(!opened.state.isLicensed)
    #expect(throws: LicenseError(.storageFailure)) { try manager.reload() }
    #expect(manager.state == committed)
    #expect(storage.data == bytes && storage.saveCount == 1)
  }
  storage.data = nil
  try manager.reload()
  #expect(manager.initialRestoreError == nil && !manager.state.isLicensed)
  #expect(storage.saveCount == 1)
  #expect(
    provider.activationCount == 0 && provider.validationCount == 0 && provider.releaseCount == 0)
}

@MainActor private final class PausedProvider: LicenseProvider {
  var validation: CheckedContinuation<LicenseValidationResult, any Error>?
  func activate(_: LicenseActivationRequest) async throws -> LicenseActivation {
    throw LicenseError(.requestFailure)
  }
  func validate(_: LicenseActivation) async throws -> LicenseValidationResult {
    try await withCheckedThrowingContinuation { validation = $0 }
  }
  func deactivate(_: LicenseActivation) async throws {}
}

@Test @MainActor func snapshotsObserveInFlightFailureAndLocalDeadline() async throws {
  let provider = PausedProvider()
  let manager = try LicenseManager(
    provider: provider, storage: MemoryStorage(),
    clock: { Date(timeIntervalSince1970: 2000) })
  let expiry = Date(timeIntervalSince1970: 2100)
  try manager.adoptExternallyVerifiedActivation(
    LicenseActivation(
      source: LicenseSource(identifier: "test")!,
      planIdentifier: "paid", activatedAt: Date(timeIntervalSince1970: 1000),
      licenseKey: "PRIVATE-KEY", activationIdentifier: "instance", expiresAt: expiry)!)
  var states = manager.states().makeAsyncIterator()
  let initial = try #require(await states.next())
  #expect(initial.nextTransitionAt == expiry)
  let refresh = Task { try await manager.refresh() }
  let busy = try #require(await states.next())
  #expect(busy.isRefreshing)
  #expect(try await manager.refresh().outcome == .skippedRefreshInProgress)
  // The provider has suspended before the consumer can resume on the same actor.
  try #require(provider.validation).resume(throwing: LicenseError(.transportFailure))
  provider.validation = nil
  _ = try await refresh.value
  let completed = try #require(await states.next())
  #expect(!completed.isRefreshing)
  #expect(completed.status == .gracePeriod)
  #expect(completed.nextTransitionAt == expiry)
  #expect(
    !String(decoding: try JSONEncoder().encode(completed), as: UTF8.self).contains("PRIVATE-KEY"))
}

@Test func refreshOutcomeWireValuesRejectUnknownDecisions() throws {
  let expected = [
    "validated", "gracePeriod", "expired", "invalid", "deactivated",
    "skippedActivationInProgress", "skippedRefreshInProgress", "skippedDeactivationInProgress",
    "skippedNoActivation", "skippedRefreshDisabled", "skippedNotDue",
  ]
  #expect(LicenseRefreshOutcome.allCases.map(\.rawValue) == expected)
  for outcome in LicenseRefreshOutcome.allCases {
    let encoded = try JSONEncoder().encode(outcome)
    #expect(try JSONDecoder().decode(LicenseRefreshOutcome.self, from: encoded) == outcome)
  }
  for value in ["\"futureOutcome\"", "\"refreshed\"", "0"] {
    #expect(throws: DecodingError.self) {
      try JSONDecoder().decode(LicenseRefreshOutcome.self, from: Data(value.utf8))
    }
  }
  try LicensingNative.requireABI(5)
  for oldABI in [UInt32(1), 2, 3, 4] {
    #expect(throws: LicenseError(.incompatibleABIVersion)) {
      try LicensingNative.requireABI(oldABI)
    }
  }
  let failure = try JSONDecoder().decode(
    LicenseError.self, from: Data("{\"code\":\"unsupportedVersion\"}".utf8))
  #expect(failure.code == .unsupportedVersion)
  #expect(failure.retryAfterSeconds == nil && failure.mutationOutcome == nil)
  let mutation = LicenseError(
    .cancelled, statusCode: 429, retryAfterSeconds: UInt64.max, mutationOutcome: .notApplied)
  let encoded = try JSONEncoder().encode(mutation)
  let wire = try #require(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
  #expect(wire["mutationOutcome"] as? String == "notApplied")
  #expect(try JSONDecoder().decode(LicenseError.self, from: encoded) == mutation)
  let unknown = try JSONDecoder().decode(
    LicenseError.self,
    from: Data(
      "{\"code\":\"mutationOutcomeUnknown\",\"mutationOutcome\":\"unknown\"}".utf8))
  #expect(unknown == LicenseError(.mutationOutcomeUnknown, mutationOutcome: .unknown))
  #expect(
    try JSONDecoder().decode(
      LicenseError.self,
      from: Data(
        "{\"code\":\"activationPending\"}".utf8)) == LicenseError(.activationPending))
  #expect(throws: DecodingError.self) {
    try JSONDecoder().decode(LicenseMutationOutcome.self, from: Data("\"applied\"".utf8))
  }
}

@Test @MainActor func refreshReturnsTypedNoActivationAndDisabledDecisions() async throws {
  let provider = ValidationProvider()
  let manager = try LicenseManager(
    provider: provider, storage: MemoryStorage(), refreshPolicy: .never,
    clock: { Date(timeIntervalSince1970: 2000) })
  #expect(try await manager.refresh().outcome == .skippedNoActivation)
  try await manager.activate(.licenseKey("TEST-KEY"))
  #expect(try await manager.refresh().outcome == .skippedRefreshDisabled)
  #expect(provider.validations == 0)
}

@Test @MainActor func activationJournalMustSaveBeforeTheProviderCanRun() async throws {
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  storage.fails = true
  await #expect(throws: LicenseError(.storageFailure)) {
    try await manager.activate(.licenseKey("TEST-KEY"))
  }
  #expect(provider.activationCount == 0 && storage.data == nil)
  #expect(manager.state.hasPendingPersistence && !manager.state.isLicensed)
  provider.onActivation = {
    let record = try? JSONSerialization.jsonObject(with: storage.data!) as? [String: Any]
    let intent = record?["pendingActivation"] as? [String: Any]
    #expect(intent?["licenseKey"] as? String == "TEST-KEY")
    #expect(record?["version"] as? Int == 2)
  }
  storage.fails = false
  try await manager.retryPersistence()
  try await manager.retryPersistence()
  #expect(provider.activationCount == 1 && storage.saveCount == 3)
  #expect(manager.state.isLicensed && !manager.state.hasPendingActivation)
}

@Test @MainActor func cancellationAfterActivationJournalClearsIntentWithoutEnteringProvider()
  async throws
{
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  storage.beforeSave = { withUnsafeCurrentTask { $0?.cancel() } }
  let task = Task { @MainActor in try await manager.activate(.licenseKey("TEST-KEY")) }
  await #expect(throws: CancellationError.self) { try await task.value }
  #expect(provider.activationCount == 0 && storage.saveCount == 2)
  #expect(!manager.state.hasPendingActivation && !manager.state.hasPendingPersistence)
  #expect(
    manager.state.lastOperationFailure == LicenseError(.cancelled, mutationOutcome: .notApplied))
  let restored = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  #expect(restored.state.lastOperationFailure == manager.state.lastOperationFailure)
  #expect(!restored.state.hasPendingActivation && !restored.state.hasActivation)
}

@Test(.timeLimit(.minutes(1))) @MainActor
func failedMutationReceiptRetainsItsErrorUntilPersistenceSucceeds() async throws {
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let original = LicenseError(
    .serverFailure, statusCode: 503, retryAfterSeconds: 120, mutationOutcome: .unknown)
  let reported = LicenseError(
    .mutationOutcomeUnknown, statusCode: 503, retryAfterSeconds: 120, mutationOutcome: .unknown)
  provider.activationFailure = original
  provider.onActivation = { #expect(storage.saveCount == 1 && storage.data != nil) }
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  storage.failOnSave = 2
  await #expect(throws: LicenseError(.storageFailure)) {
    try await manager.activate(.licenseKey("TEST-KEY"))
  }
  #expect(manager.state.hasPendingPersistence && storage.saveCount == 2)
  var states = manager.states().makeAsyncIterator()
  #expect(try #require(await states.next()).hasPendingPersistence)
  storage.failOnSave = nil
  await #expect(throws: reported) { try await manager.retryPersistence() }
  let persisted = try #require(await states.next())
  #expect(!persisted.hasPendingPersistence && persisted.hasPendingActivation)
  #expect(persisted.lastOperationFailure == original)
  try await manager.retryPersistence()

  let restored = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  #expect(restored.state.lastOperationFailure == original)
  await #expect(throws: LicenseError(.activationPending)) {
    try await restored.activate(.licenseKey("TEST-KEY"))
  }
  #expect(
    provider.activationCount == 1 && provider.validationCount == 0 && provider.releaseCount == 0)
  #expect(storage.saveCount == 3)
}

@Test @MainActor func providerCancellationCannotProveActivationDidNotApply() async throws {
  for kind in ["plain", "unknown", "notApplied"] {
    let storage = MemoryStorage()
    let provider = ReleaseProvider()
    let original = LicenseError(
      .cancelled, retryAfterSeconds: kind == "plain" ? nil : 90,
      mutationOutcome: kind == "plain" ? nil : kind == "unknown" ? .unknown : .notApplied)
    if kind == "plain" {
      provider.activationFailure = CancellationError()
    } else {
      provider.activationFailure = original
    }
    let manager = try LicenseManager(
      provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
    if kind == "notApplied" {
      await #expect(throws: CancellationError.self) {
        try await manager.activate(.licenseKey("TEST-KEY"))
      }
    } else {
      await #expect(
        throws: LicenseError(
          .mutationOutcomeUnknown,
          retryAfterSeconds: original.retryAfterSeconds, mutationOutcome: .unknown)
      ) {
        try await manager.activate(.licenseKey("TEST-KEY"))
      }
    }
    #expect(manager.state.hasPendingActivation == (kind != "notApplied"))
    #expect(manager.state.lastOperationFailure == original && storage.saveCount == 2)
    let restored = try LicenseManager(
      provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
    if kind != "notApplied" {
      // Fixture authority has established that the saved request never applied.
      try restored.confirmActivationNotAppliedAfterExternalVerification()
      #expect(restored.state.lastOperationFailure == nil)
    }
    #expect(!restored.state.hasPendingActivation)
    #expect(
      provider.activationCount == 1 && provider.validationCount == 0 && provider.releaseCount == 0)
  }
}

@Test @MainActor func automaticRefreshHonorsPersistedValidationAndReleaseCooldowns() async throws {
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let clock = ValidationClock()
  let policy = try LicenseRefreshPolicy(
    validationInterval: 3600, failureGracePeriod: 600, serverFailureGracePeriod: 300)
  let manager = try LicenseManager(
    provider: provider, storage: storage,
    refreshPolicy: policy, clock: clock.read)
  try await manager.activate(.licenseKey("TEST-KEY"))
  provider.validationFailure = LicenseError(.rateLimited, statusCode: 429, retryAfterSeconds: 120)
  #expect(try await manager.refresh().outcome == .gracePeriod)
  #expect(provider.validationCount == 1 && !manager.needsRefresh())
  #expect(manager.state.nextRefreshAt == Date(timeIntervalSince1970: 2120))
  let saves = storage.saveCount
  let restored = try LicenseManager(
    provider: provider, storage: storage,
    refreshPolicy: policy, clock: clock.read)
  #expect(storage.saveCount == saves && restored.state.nextRefreshAt == manager.state.nextRefreshAt)
  #expect(try await restored.refresh(onlyIfDue: true).outcome == .skippedNotDue)
  #expect(provider.validationCount == 1 && storage.saveCount == saves)
  clock.set(2250)
  provider.validationResult = LicenseValidationResult(isValid: true)!
  #expect(try await restored.refresh(onlyIfDue: true).outcome == .validated)
  #expect(provider.validationCount == 2)

  provider.releaseFailure = LicenseError(.transportFailure, retryAfterSeconds: 90)
  await #expect(
    throws: LicenseError(
      .mutationOutcomeUnknown, retryAfterSeconds: 90,
      mutationOutcome: .unknown)
  ) { try await restored.deactivate() }
  #expect(restored.state.hasPendingDeactivation && provider.releaseCount == 1)
  let releaseSaves = storage.saveCount
  #expect(try await restored.refresh(onlyIfDue: true).outcome == .skippedNotDue)
  #expect(provider.releaseCount == 1 && storage.saveCount == releaseSaves)
  clock.set(2500)
  provider.releaseFailure = nil
  #expect(try await restored.refresh(onlyIfDue: true).outcome == .deactivated)
  #expect(provider.releaseCount == 2 && !restored.state.hasActivation)
}

@Test @MainActor func externallyVerifiedReleaseOnlyPersistsTheConfirmation() throws {
  let storage = MemoryStorage()
  storage.data = Data(
    """
    {"version":2,"status":"deactivated","pendingDeactivation":true,
     "activation":{"source":"fixture","planIdentifier":"paid","activatedAt":1000,
                   "activationIdentifier":"fixture-instance"},
     "lastOperationFailure":{"code":"serverFailure"}}
    """.utf8)
  let provider = ReleaseProvider()
  let restored = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  try #require(restored.state.hasPendingDeactivation)
  // Fixture authority confirmed release of the saved remote instance externally.
  try restored.confirmDeactivationAfterExternalVerification()
  #expect(storage.saveCount == 1)
  #expect(
    provider.activationCount == 0 && provider.validationCount == 0 && provider.releaseCount == 0)
  #expect(!restored.state.hasActivation && !restored.state.hasPendingDeactivation)
  #expect(restored.state.lastOperationFailure == nil)
}

@Test @MainActor func versionOneUpgradeDoesNotRewriteSecureStorageOnLoad() async throws {
  let storage = MemoryStorage()
  let provider = ReleaseProvider()
  let manager = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  try await manager.activate(.licenseKey("TEST-KEY"))
  var legacy = try #require(JSONSerialization.jsonObject(with: storage.data!) as? [String: Any])
  legacy["version"] = 1
  for name in ["observedAt", "nextRefreshAt", "pendingActivation", "lastOperationFailure"] {
    legacy.removeValue(forKey: name)
  }
  let json = String(decoding: try JSONSerialization.data(withJSONObject: legacy), as: UTF8.self)
  let bytes = try #require(json.data(using: .utf16))
  let writes = storage.saveCount
  storage.data = bytes
  let restored = try LicenseManager(
    provider: provider, storage: storage, clock: { Date(timeIntervalSince1970: 2000) })
  #expect(restored.initialRestoreError == nil && restored.state.isLicensed)
  #expect(restored.state.plan == manager.state.plan)
  #expect(storage.data == bytes && storage.saveCount == writes)
  #expect(
    provider.activationCount == 1 && provider.validationCount == 0 && provider.releaseCount == 0)
}
