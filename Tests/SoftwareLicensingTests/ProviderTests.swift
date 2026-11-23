import Foundation
import SoftwareLicensing
import Synchronization
import Testing

private func configuration(
  source: String = "host-membership", plan: String = "paid"
) throws -> CallbackLicenseProvider.Configuration {
  try .init(source: LicenseSource(identifier: source)!, planIdentifier: plan)
}

@Test func callbackMappingRejectsMismatchedCredentialsWithoutCheckingNativeAccess() async throws {
  let provider = CallbackLicenseProvider(
    configuration: try configuration(),
    checkAccess: {
      Issue.record("Mismatched credentials must not invoke native access")
      return true
    })
  let activation = LicenseActivation(
    source: LicenseSource(identifier: "host-membership")!, planIdentifier: "paid",
    activatedAt: Date(timeIntervalSince1970: 1000), licenseKey: "TEST-KEY")!
  #expect(try await !provider.validate(activation).isValid)
  await #expect(throws: LicenseError(.invalidLicense, mutationOutcome: .notApplied)) {
    try await provider.deactivate(activation)
  }
  await #expect(throws: LicenseError(.invalidLicenseKey, mutationOutcome: .notApplied)) {
    try await provider.activate(.licenseKey("TEST-KEY"))
  }
}

private actor CallbackCounter {
  var calls = 0
  func check() -> Bool {
    calls += 1
    return true
  }
}

@Test func callbackUsesHostSourceAndPlanWithoutCredentialsOrExtraDeactivationChecks() async throws {
  let counter = CallbackCounter()
  let provider = CallbackLicenseProvider(
    configuration: try configuration(source: "com.example.native", plan: "membership"),
    checkAccess: { await counter.check() }, clock: { Date(timeIntervalSince1970: 1000) })
  let activation = try await provider.activate(.automatic)
  #expect(activation.source.identifier == "com.example.native")
  #expect(activation.planIdentifier == "membership")
  #expect(activation.licenseKey == nil && activation.activationIdentifier == nil)
  #expect(try await provider.validate(activation).isValid)
  try await provider.deactivate(activation)
  #expect(await counter.calls == 2)
}

@Test func callbackDenialAndCancellationCannotGrantAccess() async throws {
  let denied = CallbackLicenseProvider(configuration: try configuration(), checkAccess: { false })
  await #expect(throws: LicenseError(.invalidLicense, mutationOutcome: .notApplied)) {
    try await denied.activate(.automatic)
  }
  let cancelled = CallbackLicenseProvider(
    configuration: try configuration(), checkAccess: { throw CancellationError() })
  let cancellation = LicenseError(.cancelled, mutationOutcome: .notApplied)
  await #expect(throws: cancellation) { try await cancelled.activate(.automatic) }
  let counter = CallbackCounter()
  let provider = CallbackLicenseProvider(
    configuration: try configuration(), checkAccess: { await counter.check() })
  let task = Task {
    withUnsafeCurrentTask { $0?.cancel() }
    await #expect(throws: cancellation) { try await provider.activate(.automatic) }
  }
  await task.value
  #expect(await counter.calls == 0)
}

@Test func callbackReadFailuresPreserveTypedMetadataAndAreKnownNotApplied() async throws {
  let activation = LicenseActivation(
    source: LicenseSource(identifier: "host-membership")!, planIdentifier: "paid",
    activatedAt: Date(timeIntervalSince1970: 1000))!
  let failures: [(any Error, LicenseError)] = [
    (
      URLError(.notConnectedToInternet),
      LicenseError(.transportFailure, mutationOutcome: .notApplied)
    ),
    (
      LicenseError(
        .serverFailure, statusCode: 503, retryAfterSeconds: 120, mutationOutcome: .unknown),
      LicenseError(
        .serverFailure, statusCode: 503, retryAfterSeconds: 120, mutationOutcome: .notApplied)
    ),
    (CancellationError(), LicenseError(.cancelled, mutationOutcome: .notApplied)),
  ]
  for (failure, expected) in failures {
    let provider = CallbackLicenseProvider(
      configuration: try configuration(), checkAccess: { throw failure })
    await #expect(throws: expected) { try await provider.activate(.automatic) }
    await #expect(throws: expected) { try await provider.validate(activation) }
  }
}

@Test func callbackCancellationWhileThrowingCannotBecomeTransportFailure() async throws {
  let provider = CallbackLicenseProvider(
    configuration: try configuration(),
    checkAccess: {
      withUnsafeCurrentTask { $0?.cancel() }
      throw URLError(.networkConnectionLost)
    })
  let task = Task {
    await #expect(throws: LicenseError(.cancelled, mutationOutcome: .notApplied)) {
      try await provider.activate(.automatic)
    }
  }
  _ = await task.value
}

@Test(arguments: [1, 2])
func callbackMappingFailuresBeforeAndAfterAccessAreKnownNotApplied(
  _ failingClockRead: Int
)
  async throws
{
  let reads = Mutex(0)
  let access = CallbackCounter()
  let provider = CallbackLicenseProvider(
    configuration: try configuration(), checkAccess: { await access.check() },
    clock: {
      reads.withLock { count in
        count += 1
        return Date(timeIntervalSince1970: count == failingClockRead ? .nan : 1000)
      }
    })
  await #expect(throws: LicenseError(.invalidData, mutationOutcome: .notApplied)) {
    try await provider.activate(.automatic)
  }
  #expect(await access.calls == (failingClockRead == 1 ? 0 : 1))
}

@Test(arguments: [1, 2])
func callbackCancellationDuringMappingCannotGrantAccess(_ cancelledClockRead: Int) async throws {
  let reads = Mutex(0)
  let access = CallbackCounter()
  let provider = CallbackLicenseProvider(
    configuration: try configuration(), checkAccess: { await access.check() },
    clock: {
      reads.withLock { count in
        count += 1
        if count == cancelledClockRead { withUnsafeCurrentTask { $0?.cancel() } }
        return Date(timeIntervalSince1970: 1000)
      }
    })
  let task = Task {
    await #expect(throws: LicenseError(.cancelled, mutationOutcome: .notApplied)) {
      try await provider.activate(.automatic)
    }
  }
  _ = await task.value
  #expect(await access.calls == (cancelledClockRead == 1 ? 0 : 1))
}

@Test func callbackConfigurationIsValidatedBeforeAnyAccessCheck() throws {
  let source = LicenseSource(identifier: "host-membership")!
  #expect(throws: LicenseError(.invalidProviderConfiguration)) {
    try CallbackLicenseProvider.Configuration(source: source, planIdentifier: "unlicensed")
  }
  let normalized = try CallbackLicenseProvider.Configuration(
    source: source, planIdentifier: " paid ")
  #expect(normalized.planIdentifier == "paid")
}

@Test func publicFactoriesProjectNativeValuesAndErrors() throws {
  let source = try #require(LicenseSource(identifier: " fixture "))
  let now = Date(timeIntervalSince1970: 1000)
  let activation = try #require(
    LicenseActivation(source: source, planIdentifier: " paid ", activatedAt: now))
  #expect(activation.source.identifier == "fixture")
  #expect(activation.planIdentifier == "paid")
  #expect(activation.activatedAt == now)
  #expect(LicenseActivation(source: source, planIdentifier: "unlicensed", activatedAt: now) == nil)
  #expect(
    LicenseValidationResult(isValid: true, planIdentifier: " paid ")?.planIdentifier == "paid")
  #expect(LicenseValidationResult(isValid: false, planIdentifier: "paid") == nil)
  let policy = try LicenseRefreshPolicy(
    validationInterval: 3600, failureGracePeriod: 600, serverFailureGracePeriod: 300)
  #expect(policy.failureRetryInterval == 60)
  #expect(throws: LicenseError(.invalidData)) {
    try LicenseRefreshPolicy(
      validationInterval: .nan, failureGracePeriod: 600, serverFailureGracePeriod: 300)
  }
}
