import CSoftwareLicensing
import Foundation

/// Credential-free errors with optional HTTP status, retry delay and mutation outcome.
public struct LicenseError: Error, Codable, Equatable, Hashable, Sendable, LocalizedError {
  public enum Code: String, Codable, Sendable {
    case invalidData, invalidLicenseKey, invalidLicense, expiredLicense
    /// Unsupported persisted record schema; stored bytes remain untouched.
    case unsupportedVersion
    /// Native binary and binding ABI versions differ, independently of storage.
    case incompatibleABIVersion = "incompatibleAbiVersion"
    case activationLimitReached, activationAlreadyPresent, activationPending,
      mutationOutcomeUnknown,
      invalidProviderConfiguration,
      unexpectedProviderResponse
    case transportFailure, requestFailure, serverFailure, rateLimited, storageFailure,
      operationInProgress
    case persistencePending, deactivationPending, clockRollback, gracePeriodExpired
    case cancelled, unsupportedPlatform
  }

  public let code: Code
  public let statusCode: Int?
  public let retryAfterSeconds: UInt64?
  public let mutationOutcome: LicenseMutationOutcome?

  public init(
    _ code: Code, statusCode: Int? = nil, retryAfterSeconds: UInt64? = nil,
    mutationOutcome: LicenseMutationOutcome? = nil
  ) {
    self.code = code
    self.statusCode = statusCode
    self.retryAfterSeconds = retryAfterSeconds
    self.mutationOutcome = mutationOutcome
  }

  public var errorDescription: String? { code.rawValue }
}

/// Internal value bridge shared by the package's optional provider products.
package enum LicensingNative {
  package enum Function { case value, accessReply }
  fileprivate static let maximumJSONBytes = 1_048_576

  package static func encoder() -> JSONEncoder {
    let encoder = JSONEncoder()
    encoder.dateEncodingStrategy = .secondsSince1970
    return encoder
  }

  package static func decoder() -> JSONDecoder {
    let decoder = JSONDecoder()
    decoder.dateDecodingStrategy = .secondsSince1970
    return decoder
  }

  package static func object(_ value: some Encodable) throws -> Any {
    try JSONSerialization.jsonObject(with: encoder().encode(value), options: .fragmentsAllowed)
  }

  package static func encode(_ object: Any) throws -> Data {
    // Foundation can raise an Objective-C exception for non-finite numbers,
    // which Swift catch cannot intercept. Wrap fragments for JSON validation.
    guard JSONSerialization.isValidJSONObject([object]) else {
      throw LicenseError(.invalidData)
    }
    do {
      return try JSONSerialization.data(withJSONObject: object, options: .fragmentsAllowed)
    } catch {
      throw LicenseError(.invalidData)
    }
  }

  package static func checkABI() throws {
    try requireABI(software_licensing_abi_version())
  }

  package static func requireABI(_ version: UInt32) throws {
    guard version == SOFTWARE_LICENSING_ABI_VERSION else {
      throw LicenseError(.incompatibleABIVersion)
    }
  }

  package static func invoke(_ function: Function, _ object: Any) throws -> Data {
    try checkABI()
    let data = try encode(object)
    return try data.withUnsafeBytes { bytes in
      let pointer = bytes.bindMemory(to: UInt8.self).baseAddress
      let result =
        switch function {
        case .value: software_licensing_value(pointer, bytes.count)
        case .accessReply: software_licensing_access_reply(pointer, bytes.count)
        }
      return try consume(result)
    }
  }

  package static func normalized<T: Codable>(_ kind: String, _ value: Any, as _: T.Type) throws -> T
  {
    try decoder().decode(T.self, from: invoke(.value, ["kind": kind, "value": value]))
  }

  static func consume(_ owner: OpaquePointer?) throws -> Data {
    guard let owner else { throw LicenseError(.invalidData) }
    defer { software_licensing_json_destroy(owner) }
    let view = software_licensing_json_view(owner)
    guard let bytes = view.data, view.length <= maximumJSONBytes else {
      throw LicenseError(.invalidData)
    }
    let data = Data(bytes: bytes, count: view.length)
    if view.code != 0 {
      throw (try? decoder().decode(LicenseError.self, from: data)) ?? LicenseError(.invalidData)
    }
    return data
  }
}

// The Rust owner serializes dispatch with a Mutex; ARC keeps it live for each call.
final class LicenseSession: @unchecked Sendable {
  private let handle: OpaquePointer

  init() {
    precondition(
      software_licensing_abi_version() == SOFTWARE_LICENSING_ABI_VERSION,
      "SoftwareLicensing ABI mismatch")
    handle = software_licensing_session_create()!
  }

  deinit { software_licensing_session_destroy(handle) }

  var hasCompletion: Bool { software_licensing_session_has_completion(handle) != 0 }

  func completeProvider(_ id: UInt64, result: [String: Any], now: TimeInterval) throws -> Data {
    var completion: [String: Any] = [
      "command": "completeEffect", "id": id,
      "completion": ["kind": "provider", "result": result], "now": timestamp(now),
    ]
    if let data = try? LicensingNative.encode(completion),
      data.count <= LicensingNative.maximumJSONBytes
    {
      return try dispatch(data)
    }
    // Submit the completed effect even when its result cannot cross the ABI.
    // Rust retains or releases the effect without repeating the provider call.
    completion["completion"] = ["kind": "provider"]
    return try dispatch(completion)
  }

  func completePersistence(_ id: UInt64, success: Bool, now: TimeInterval) throws -> Data {
    try dispatch([
      "command": "completeEffect", "id": id,
      "completion": ["kind": "persisted", "success": success], "now": timestamp(now),
    ])
  }

  func resumeCompletion(now: TimeInterval, cancelled: Bool) throws -> Data {
    try dispatch(["command": "resumeCompletion", "now": timestamp(now), "cancelled": cancelled])
  }

  private func timestamp(_ instant: TimeInterval) -> Any {
    if instant.isFinite { return instant }
    return NSNull()
  }

  func dispatch(_ object: [String: Any]) throws -> Data {
    try dispatch(LicensingNative.encode(object))
  }

  private func dispatch(_ data: Data) throws -> Data {
    try data.withUnsafeBytes { bytes in
      try LicensingNative.consume(
        software_licensing_session_dispatch(
          handle, bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count))
    }
  }

  func stageStoredRecord(_ record: Data) throws {
    let bytes = record.withUnsafeBytes { Data($0) }
    _ = try bytes.withUnsafeBytes { buffer in
      try LicensingNative.consume(
        software_licensing_session_stage_record(
          handle, buffer.bindMemory(to: UInt8.self).baseAddress, buffer.count))
    }
  }
}
