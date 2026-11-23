import Foundation

/// Host bridge for a Rust session that executes its own provider effects.
/// Use the command and reply JSON of `software_licensing::Session`, with
/// provider effects represented as `{ "kind": "provider", "id": ... }`.
@MainActor
public protocol LicenseSessionDriver: AnyObject {
  func dispatch(_ command: Data) throws -> Data
  func stageStoredRecord(_ record: Data) throws

  /// Wait for the native reply to be retained by the session. Forward task
  /// cancellation to the provider without abandoning the reply. An already
  /// cancelled task must resolve the effect without entering the provider.
  /// Report provider failures through the session reply.
  func executeProviderEffect(_ id: UInt64) async throws

  /// Apply the retained reply and return the resulting session response.
  /// Discard cancelled validation but preserve known mutation outcomes.
  func completeProviderEffect(_ id: UInt64, now: TimeInterval, cancelled: Bool) throws -> Data
}
