import Foundation

/// Provider identity with the exact UTF-8 spelling used by Rust.
public struct LicenseSource: Codable, Equatable, Hashable, Sendable {
  public let identifier: String

  public init?(identifier: String) {
    guard
      let value: String = try? LicensingNative.normalized("identifier", identifier, as: String.self)
    else { return nil }
    self.identifier = value
  }

  public init(from decoder: any Decoder) throws {
    identifier = try decoder.singleValueContainer().decode(String.self)
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode(identifier)
  }

  public static func == (lhs: Self, rhs: Self) -> Bool {
    lhs.identifier.utf8.elementsEqual(rhs.identifier.utf8)
  }

  public func hash(into hasher: inout Hasher) {
    for byte in identifier.utf8 { hasher.combine(byte) }
  }

  public static let unspecified = LicenseSource(identifier: "unspecified")!
}

/// Credential-bearing value for provider calls. Keep it out of UI state and logs.
public struct LicenseActivation: Codable, Equatable, Hashable, Sendable, CustomStringConvertible {
  public let source: LicenseSource
  public let planIdentifier: String
  public let activatedAt: Date
  public let licenseKey: String?
  public let activationIdentifier: String?
  public let expiresAt: Date?

  public var description: String { "LicenseActivation([redacted])" }

  public init?(
    source: LicenseSource, planIdentifier: String, activatedAt: Date,
    licenseKey: String? = nil, activationIdentifier: String? = nil, expiresAt: Date? = nil
  ) {
    var value: [String: Any] = [
      "source": source.identifier, "planIdentifier": planIdentifier,
      "activatedAt": activatedAt.timeIntervalSince1970,
    ]
    value["licenseKey"] = licenseKey
    value["activationIdentifier"] = activationIdentifier
    value["expiresAt"] = expiresAt?.timeIntervalSince1970
    guard let result = try? LicensingNative.normalized("activation", value, as: Self.self) else {
      return nil
    }
    self = result
  }
}

public struct LicenseValidationResult: Codable, Equatable, Hashable, Sendable {
  public let isValid: Bool
  public let planIdentifier: String?
  public let expiresAt: Date?

  public init?(isValid: Bool, planIdentifier: String? = nil, expiresAt: Date? = nil) {
    var value: [String: Any] = ["isValid": isValid]
    value["planIdentifier"] = planIdentifier
    value["expiresAt"] = expiresAt?.timeIntervalSince1970
    guard let result = try? LicensingNative.normalized("validation", value, as: Self.self) else {
      return nil
    }
    self = result
  }
}

public struct LicenseRefreshPolicy: Codable, Equatable, Hashable, Sendable {
  public let isEnabled: Bool
  public let validationInterval: TimeInterval
  public let failureGracePeriod: TimeInterval
  public let serverFailureGracePeriod: TimeInterval
  public let failureRetryInterval: TimeInterval

  public init(
    isEnabled: Bool = true, validationInterval: TimeInterval,
    failureGracePeriod: TimeInterval, serverFailureGracePeriod: TimeInterval,
    failureRetryInterval: TimeInterval = 60
  ) throws {
    self = try LicensingNative.normalized(
      "policy",
      [
        "isEnabled": isEnabled, "validationInterval": validationInterval,
        "failureGracePeriod": failureGracePeriod,
        "serverFailureGracePeriod": serverFailureGracePeriod,
        "failureRetryInterval": failureRetryInterval,
      ], as: Self.self)
  }

  public static let `default` = try! LicensingNative.decoder().decode(
    LicenseRefreshPolicy.self, from: LicensingNative.invoke(.value, ["kind": "defaultPolicy"]))
  public static let never = try! LicenseRefreshPolicy(
    isEnabled: false, validationInterval: 0, failureGracePeriod: 0, serverFailureGracePeriod: 0)
}

public enum LicenseStatus: String, Codable, Equatable, Hashable, CaseIterable, Sendable {
  case unlicensed, active, gracePeriod, expired, invalid, deactivated
}

/// A provider may report `notApplied` only when it can prove no remote mutation occurred.
/// `unknown` means the server may have changed state and requires external reconciliation.
public enum LicenseMutationOutcome: String, Codable, Equatable, Hashable, Sendable {
  case notApplied, unknown
}

public struct LicensePlan: Codable, Equatable, Hashable, Sendable {
  public let identifier: String
  public let isLicensed: Bool
  public let expiresAt: Date?

  public static func == (lhs: Self, rhs: Self) -> Bool {
    lhs.isLicensed == rhs.isLicensed && lhs.expiresAt == rhs.expiresAt
      && lhs.identifier.utf8.elementsEqual(rhs.identifier.utf8)
  }

  public func hash(into hasher: inout Hasher) {
    hasher.combine(isLicensed)
    hasher.combine(expiresAt)
    for byte in identifier.utf8 { hasher.combine(byte) }
  }
}

/// Rust's time-evaluated snapshot, containing no license key or activation identifier.
public struct LicenseState: Codable, Equatable, Hashable, Sendable {
  public let plan: LicensePlan
  public let source: LicenseSource?
  public let status: LicenseStatus
  public let lastValidatedAt: Date?
  public let gracePeriodExpiresAt: Date?
  public let lastRefreshFailure: LicenseError?
  public let isActivating: Bool
  public let isRefreshing: Bool
  public let isDeactivating: Bool
  public let hasActivation: Bool
  public let hasPendingPersistence: Bool
  public let hasPendingDeactivation: Bool
  public let nextTransitionAt: Date?
  public let nextRefreshAt: Date?
  public let hasPendingActivation: Bool
  public let lastOperationFailure: LicenseError?
  public var isLicensed: Bool { plan.isLicensed }
}

public enum LicenseActivationRequest: Sendable {
  case automatic
  case licenseKey(String)
}

/// Native effects only. Rust owns operation ordering, state, expiry and persistence.
public protocol LicenseProvider: Sendable {
  func activate(_ request: LicenseActivationRequest) async throws -> LicenseActivation
  func validate(_ activation: LicenseActivation) async throws -> LicenseValidationResult
  func deactivate(_ activation: LicenseActivation) async throws
}

/// Atomic storage of a single confidential Rust record, scoped by the host app.
/// Deactivation saves a recovery record; deletion is a separate concrete-storage operation.
public protocol LicenseStorage: Sendable {
  func load() throws -> Data?
  func save(_ record: Data) throws
}

/// Final refresh decision after persistence, evaluated at completion time.
/// Grace/invalid/expired may reflect a failed check; inspect state.lastRefreshFailure.
public enum LicenseRefreshOutcome: String, Codable, Equatable, Hashable, CaseIterable, Sendable {
  case validated, gracePeriod, expired, invalid, deactivated
  case skippedActivationInProgress, skippedRefreshInProgress, skippedDeactivationInProgress
  case skippedNoActivation, skippedRefreshDisabled
  case skippedNotDue
}

public struct LicenseRefreshResult: Sendable {
  public let outcome: LicenseRefreshOutcome
  public let state: LicenseState
}
