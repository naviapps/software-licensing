import Foundation

/// Adapts a host-owned native access check without linking or configuring a vendor SDK.
/// The callback must be read-only; failures are known not applied, including cancellation.
public struct CallbackLicenseProvider: LicenseProvider {
  public struct Configuration: Sendable {
    public let source: LicenseSource
    public let planIdentifier: String

    /// Validates the host's source and plan using the shared Rust value contract.
    public init(source: LicenseSource, planIdentifier: String) throws {
      guard
        let value = LicenseActivation(
          source: source, planIdentifier: planIdentifier,
          activatedAt: Date(timeIntervalSince1970: 0))
      else { throw LicenseError(.invalidProviderConfiguration) }
      self.source = value.source
      self.planIdentifier = value.planIdentifier
    }
  }

  private let configuration: Configuration
  private let checkAccess: @Sendable () async throws -> Bool
  private let clock: @Sendable () -> Date

  public init(
    configuration: Configuration,
    checkAccess: @escaping @Sendable () async throws -> Bool,
    clock: @escaping @Sendable () -> Date = { Date() }
  ) {
    self.configuration = configuration
    self.checkAccess = checkAccess
    self.clock = clock
  }

  public func activate(_ request: LicenseActivationRequest) async throws -> LicenseActivation {
    let key: String? = if case .licenseKey(let key) = request { key } else { nil }
    let reply = try await perform(operation: "activate", licenseKey: key)
    guard let activation = reply.activation else {
      throw LicenseError(.unexpectedProviderResponse, mutationOutcome: .notApplied)
    }
    return activation
  }

  public func validate(_ activation: LicenseActivation) async throws -> LicenseValidationResult {
    let reply = try await perform(operation: "validate", activation: activation)
    guard let validation = reply.validation else {
      throw LicenseError(.unexpectedProviderResponse, mutationOutcome: .notApplied)
    }
    return validation
  }

  public func deactivate(_ activation: LicenseActivation) async throws {
    _ = try await perform(operation: "deactivate", activation: activation)
  }

  private struct Reply: Decodable {
    let activation: LicenseActivation?
    let validation: LicenseValidationResult?
    let failure: LicenseError?
  }
  private struct Decision: Decodable {
    let needsAccessCheck: Bool
    let reply: Reply?
  }
  private func perform(
    operation: String, activation: LicenseActivation? = nil, licenseKey: String? = nil
  ) async throws -> Reply {
    var decision: Decision
    do {
      try Task.checkCancellation()
      var input: [String: Any] = [
        "source": configuration.source.identifier, "planIdentifier": configuration.planIdentifier,
        "operation": operation, "now": clock().timeIntervalSince1970,
      ]
      input["activation"] = try activation.map(LicensingNative.object)
      input["licenseKey"] = licenseKey
      decision = try LicensingNative.decoder().decode(
        Decision.self, from: LicensingNative.invoke(.accessReply, input))
      if decision.needsAccessCheck {
        try Task.checkCancellation()
        input["hasAccess"] = try await checkAccess()
        try Task.checkCancellation()
        input["now"] = clock().timeIntervalSince1970
        decision = try LicensingNative.decoder().decode(
          Decision.self, from: LicensingNative.invoke(.accessReply, input))
      }
      try Task.checkCancellation()
    } catch {
      let failure =
        (error as? LicenseError)
        ?? LicenseError(
          error is CancellationError || Task.isCancelled ? .cancelled : .transportFailure)
      throw LicenseError(
        failure.code, statusCode: failure.statusCode, retryAfterSeconds: failure.retryAfterSeconds,
        mutationOutcome: .notApplied)
    }
    guard let reply = decision.reply else {
      throw LicenseError(.unexpectedProviderResponse, mutationOutcome: .notApplied)
    }
    if let error = reply.failure { throw error }
    return reply
  }
}
